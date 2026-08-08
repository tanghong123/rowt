//! `rowt-render` — reads the same config directory bin/rowt does and prints the
//! sing-box configuration to stdout.
//!
//!   rowt-render host --config-dir DIR --iface en0
//!   rowt-render vm   --config-dir DIR
//!
//! The interface is an argument rather than something this binary discovers:
//! interface detection is a platform call, and the platform layer does not
//! arrive until Phase 5 (PORTING.md §4). Everything else — env knobs included —
//! is read exactly where the shell reads it, so the two implementations can be
//! run against one sandbox and diffed.

use rowt_render::{geosites_of, group, parse_list, render_host, render_vm, Filter, Geo, HostInput, Lists};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string())
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// `sget`: the LAST `key=value` line wins.
fn sget(state: &str, key: &str) -> String {
    state
        .lines()
        .filter_map(|l| l.strip_prefix(&format!("{key}=")))
        .next_back()
        .unwrap_or("")
        .to_string()
}

struct Args {
    mode: String,
    config_dir: PathBuf,
    iface: String,
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let mode = it.next().ok_or("usage: rowt-render <host|vm> --config-dir DIR [--iface enX]")?;
    if mode != "host" && mode != "vm" {
        return Err(format!("unknown mode: {mode} (want host or vm)"));
    }
    let mut config_dir = None;
    let mut iface = String::new();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--config-dir" => config_dir = it.next().map(PathBuf::from),
            "--iface" => iface = it.next().unwrap_or_default(),
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    Ok(Args {
        mode,
        config_dir: config_dir.ok_or("--config-dir is required")?,
        iface,
    })
}

fn run() -> Result<String, String> {
    let args = parse_args()?;
    let cfg = &args.config_dir;
    let cache = cfg.join("cache");

    let state = read(&cfg.join("state"));
    let selected = {
        let s = sget(&state, "selected");
        if s.is_empty() { "auto".to_string() } else { s }
    };
    let secret = sget(&state, "clash_secret");

    let servers: Vec<Value> = serde_json::from_str(&read(&cfg.join("servers.json")))
        .map_err(|e| format!("servers.json: {e}"))?;

    let escape_src = read(&cfg.join("escape-domains.txt"));
    let corp_src = read(&cfg.join("corp-domains.txt"));
    let block_src = read(&cfg.join("block-domains.txt"));

    let port: u64 = env_or("ROWT_PORT", "7890").parse().map_err(|_| "bad ROWT_PORT")?;
    let clash_port: u64 = env_or("ROWT_CLASH_PORT", "9090").parse().map_err(|_| "bad ROWT_CLASH_PORT")?;
    let vm_port = port + 1;
    let vm_clash = clash_port + 1;
    let interval = env_or("ROWT_AUTO_INTERVAL", "20m");
    let log_level = env_or("ROWT_LOG_LEVEL", "warn");

    if args.mode == "vm" {
        // The guest's members are unbound: it has one interface and binding to
        // the host's physical NIC name would be meaningless inside the VM.
        let escs = group(&servers, "", &selected, &interval);
        let cfgv = render_vm(&escs, "0.0.0.0", vm_port, &format!("0.0.0.0:{vm_clash}"), &secret, &log_level);
        return Ok(serde_json::to_string_pretty(&cfgv).unwrap());
    }

    let mode = {
        let m = sget(&state, "mode");
        if m.is_empty() { "host".to_string() } else { m }
    };
    // In vm mode the host keeps no members of its own — it hands escape traffic
    // to the guest over SOCKS and the selector lives in the VM.
    let (escs, clash) = if mode == "vm" {
        let vm_ip = sget(&state, "vm_ip");
        if vm_ip.is_empty() {
            return Err("vm ip unknown (run: rowt vm up)".into());
        }
        (
            json!([{"type": "socks", "tag": "escape", "server": vm_ip, "server_port": vm_port, "version": "5"}]),
            String::new(),
        )
    } else {
        (
            group(&servers, &args.iface, &selected, &interval),
            format!("127.0.0.1:{clash_port}"),
        )
    };

    // Cache-or-skip, exactly as the shell does: a category with no .srs on disk
    // is left out of this render rather than failing it, so a first render
    // offline still produces a usable config.
    let cached = |name: &str| cache.join(format!("geosite-{name}.srs")).is_file();
    let lane_geo = |src: &str| -> Vec<String> {
        geosites_of(src).into_iter().filter(|n| cached(n)).collect()
    };
    let all: BTreeSet<String> = geosites_of(&escape_src)
        .into_iter()
        .chain(geosites_of(&block_src))
        .collect();
    let sets: Vec<(String, String)> = all
        .iter()
        .filter(|n| cached(n))
        .map(|n| {
            (
                format!("geosite-{n}"),
                cache.join(format!("geosite-{n}.srs")).to_string_lossy().into_owned(),
            )
        })
        .collect();
    let ads = cache.join("geosite-category-ads-all.srs");
    let ads_path = if ads.is_file() { ads.to_string_lossy().into_owned() } else { String::new() };

    for name in &all {
        if !cached(name) {
            eprintln!("error: geosite:{name} not cached — run 'rowt fetch host' (skipping it in this render)");
        }
    }
    if !geosites_of(&corp_src).is_empty() {
        eprintln!("error: geosite: in corp-domains.txt is ignored — only the escape and block lanes support geosite:");
    }

    let input = HostInput {
        escapes: escs,
        listen: "127.0.0.1".into(),
        port,
        iface: args.iface.clone(),
        clash,
        secret,
        log_level,
        final_route: env_or("ROWT_FINAL", "direct"),
        dns_direct: env_or("ROWT_DNS_DIRECT", "223.5.5.5"),
        private_default: env_or("ROWT_PRIVATE_DEFAULT", "corp"),
        private_cidrs: ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "100.64.0.0/10", "169.254.0.0/16"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        lists: Lists {
            escape_domains: parse_list(&escape_src, Filter::Domain),
            corp_domains: parse_list(&corp_src, Filter::Domain),
            corp_cidrs: parse_list(&corp_src, Filter::Cidr),
            block_domains: parse_list(&block_src, Filter::Domain),
        },
        geo: Geo {
            escape: lane_geo(&escape_src),
            block: lane_geo(&block_src),
            sets,
            ads_path,
        },
    };
    Ok(serde_json::to_string_pretty(&render_host(&input)).unwrap())
}

fn main() -> ExitCode {
    match run() {
        Ok(s) => {
            println!("{s}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("rowt-render: {e}");
            ExitCode::FAILURE
        }
    }
}
