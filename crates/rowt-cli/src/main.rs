//! `rowt-rs` — the Rust CLI, built **alongside** `bin/rowt` rather than
//! replacing it.
//!
//! Nothing installs or invokes this: it exists so the command surface can be
//! ported and compared while the shell stays authoritative. `parity cli-diff`
//! runs the same command through both and requires identical stdout and exit
//! status, so each command lands with evidence rather than an intention.
//!
//! It deliberately prints `rowt` rather than its own name — it is emulating
//! that tool, and the difference would otherwise show up as a false diff.
//!
//! Dispatch is hand-rolled on purpose. clap would earn its place once the 525
//! lines of help text are ported, but until then it would only add a dependency
//! and a help format that does not match the shell's.

use rowt_core::classify::{classify, ClassifyInput, Lane};
use rowt_core::lanes::{apply, dump, Lanes, Op};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const PROG: &str = "rowt";

fn env_or(k: &str, d: &str) -> String {
    std::env::var(k).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| d.to_string())
}

fn config_dir() -> PathBuf {
    match std::env::var("XDG_CONFIG_HOME") {
        Ok(x) if !x.is_empty() => PathBuf::from(x).join("rowt"),
        _ => PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config/rowt"),
    }
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

fn sget(state: &str, key: &str) -> String {
    state.lines().filter_map(|l| l.strip_prefix(&format!("{key}="))).next_back().unwrap_or("").into()
}

fn lane_file(cfg: &Path, l: Lane) -> PathBuf {
    cfg.join(match l {
        Lane::Escape => "escape-domains.txt",
        Lane::Corp => "corp-domains.txt",
        Lane::Block => "block-domains.txt",
        Lane::Direct => "direct-domains.txt",
    })
}

fn load_lanes(cfg: &Path) -> Lanes {
    Lanes {
        escape: read(&lane_file(cfg, Lane::Escape)),
        corp: read(&lane_file(cfg, Lane::Corp)),
        block: read(&lane_file(cfg, Lane::Block)),
    }
}

/// `geosites_of` over a lane list — used for explain's rule-set note.
fn geosites(body: &str) -> Vec<String> {
    rowt_core::render::geosites_of(body)
}

fn cmd_explain(cfg: &Path, dest: &str) -> String {
    let lanes = load_lanes(cfg);
    let state = read(&cfg.join("state"));
    let mode = { let m = sget(&state, "mode"); if m.is_empty() { "host".into() } else { m } };
    let private: Vec<String> =
        ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "100.64.0.0/10", "169.254.0.0/16"]
            .iter().map(|s| s.to_string()).collect();
    let final_route = Lane::parse(&env_or("ROWT_FINAL", "direct")).unwrap_or(Lane::Direct);
    let pd = env_or("ROWT_PRIVATE_DEFAULT", "corp");
    let c = classify(dest, &ClassifyInput {
        escape_list: &lanes.escape, corp_list: &lanes.corp, block_list: &lanes.block,
        private_cidrs: &private, private_default: &pd, final_route,
        local_mode: mode == "local", resolved_ip: "",
    });
    let mut out = c.render();

    // The shell warns that opaque rule-sets may still match, whenever the ad set
    // is cached or any lane names a geosite: category. Not shown for `block`,
    // which is already the most restrictive answer.
    if c.lane != Lane::Block {
        let ads = cfg.join("cache/geosite-category-ads-all.srs").is_file();
        let mut all: Vec<String> = geosites(&lanes.escape);
        all.extend(geosites(&lanes.block));
        all.sort();
        all.dedup();
        if ads || !all.is_empty() {
            let gs = all.join(",");
            out.push_str(&format!(
                "\n  note:    a geosite rule-set may still match this (not shown — binary set){}{}",
                if gs.is_empty() { String::new() } else { format!(": geosite:{gs}") },
                if ads { " + ad-block" } else { "" }
            ));
        }
    }
    if c.lane == Lane::Block {
        out.push_str("\n  live:    (skipped — blocked by design)");
    }
    out
}

fn cmd_lane(cfg: &Path, lane: Lane, action: &str, args: &[String]) -> Result<String, String> {
    let label = lane.as_str();
    let lanes = load_lanes(cfg);
    let body = match lane {
        Lane::Escape => &lanes.escape,
        Lane::Corp => &lanes.corp,
        _ => &lanes.block,
    }
    .clone();

    match action {
        "list" => {
            let entries = dump(&body);
            let mut o = format!("{label} list:");
            if entries.is_empty() {
                o.push_str("\n  (empty)");
                o.push_str(&format!(
                    "\n  {PROG} {label} add <e>… | rm <e>… | import <file> | clear | dump [file]"
                ));
            } else {
                for e in entries {
                    o.push_str(&format!("\n  {e}"));
                }
            }
            Ok(o)
        }
        "dump" => Ok(dump(&body).join("\n")),
        "add" | "rm" | "remove" | "clear" => {
            let op = match action {
                "add" => Op::Add(args.to_vec()),
                "clear" => Op::Clear,
                _ => Op::Rm(args.to_vec()),
            };
            let e = apply(&lanes, lane, &op);
            for l in [Lane::Escape, Lane::Corp, Lane::Block] {
                let (before, after) = match l {
                    Lane::Escape => (&lanes.escape, &e.lanes.escape),
                    Lane::Corp => (&lanes.corp, &e.lanes.corp),
                    _ => (&lanes.block, &e.lanes.block),
                };
                if before != after {
                    std::fs::write(lane_file(cfg, l), after).map_err(|x| format!("write: {x}"))?;
                }
            }
            Ok(e.messages.join("\n"))
        }
        other => Err(format!(
            "usage: {PROG} {label} [list | add <e>… | rm <e>… | import <file> | clear | dump [file]] (got {other})"
        )),
    }
}

fn run() -> Result<String, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = config_dir();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("");
    let rest: Vec<String> = args.iter().skip(1).cloned().collect();
    match cmd {
        "explain" | "route" => {
            let d = rest.first().ok_or(format!("usage: {PROG} explain <domain|ip>"))?;
            Ok(cmd_explain(&cfg, d))
        }
        "escape" | "corp" | "block" => {
            let lane = Lane::parse(cmd).unwrap();
            let action = rest.first().cloned().unwrap_or_else(|| "list".into());
            cmd_lane(&cfg, lane, &action, &rest[1.min(rest.len())..])
        }
        "" => Err(format!("usage: {PROG}-rs <explain|escape|corp|block> …")),
        other => Err(format!("unknown command: {other}")),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(s) => {
            println!("{s}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
