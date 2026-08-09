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

mod lifecycle;
use lifecycle::Ctx;
use rowt_core::classify::{classify, ClassifyInput, Lane};
use rowt_core::lanes::{apply, dump, Lanes, Op};
use rowt_platform::{Mac, Platform};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const PROG: &str = "rowt";

/// `exec` the child, replacing this process — the shell does the same so the TUI
/// owns the terminal rather than running under a parent that would outlive it.
trait ExecReplace { fn exec_replace(&mut self) -> String; }
impl ExecReplace for std::process::Command {
    fn exec_replace(&mut self) -> String {
        use std::os::unix::process::CommandExt;
        format!("could not exec: {}", self.exec())
    }
}

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

fn port() -> u16 {
    env_or("ROWT_PORT", "7890").parse().unwrap_or(7890)
}

/// `awk '/Enabled/{print $2} /Server/{s=$2} /Port/{print "         "s":"$2}'`
/// then `tr '\n' ' '` — reproduced rather than tidied, because the shell's
/// output is the specification. Note "Authenticated Proxy Enabled: 0" also
/// matches /Enabled/, which is why "Proxy" appears in the result.
fn proxy_awk(body: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut server = String::new();
    for line in body.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if line.contains("Enabled") {
            out.push(f.get(1).unwrap_or(&"").to_string());
        }
        if line.contains("Server") {
            server = f.get(1).unwrap_or(&"").to_string();
        }
        if line.contains("Port") {
            out.push(format!("         {server}:{}", f.get(1).unwrap_or(&"")));
        }
    }
    let mut s = out.join(" ");
    if !s.is_empty() {
        s.push(' ');
    }
    s
}

fn cmd_proxy(action: &str, arg: Option<&str>) -> Result<(String, bool), String> {
    let p = Mac;
    let prt = port();
    match action {
        "env" => {
            if arg == Some("--off") {
                return Ok((
                    "unset http_proxy https_proxy all_proxy HTTP_PROXY HTTPS_PROXY ALL_PROXY".into(),
                    true,
                ));
            }
            let h = format!("http://127.0.0.1:{prt}");
            let s = format!("socks5h://127.0.0.1:{prt}");
            Ok((
                format!(
                    "export http_proxy={h} https_proxy={h} all_proxy={s}\nexport HTTP_PROXY={h} HTTPS_PROXY={h} ALL_PROXY={s}"
                ),
                true,
            ))
        }
        "status" => {
            let Some(svc) = p.active_service() else {
                return Ok(("system proxy: no active network service (nothing configured)".into(), true));
            };
            let read = |flag: &str| -> String {
                proxy_awk(&rowt_platform::read_proxy(&svc, flag))
            };
            Ok((
                format!(
                    "system proxy (service '{svc}'):\n  socks:  {}\n  https:  {}\n  bypass: {}\n  CLI env: eval \"$({PROG} proxy env)\"   (off: {PROG} proxy env --off)",
                    read("-getsocksfirewallproxy"),
                    read("-getsecurewebproxy"),
                    rowt_platform::read_bypass(&svc),
                ),
                true,
            ))
        }
        "check" => {
            let Some(svc) = p.active_service() else {
                return Ok(("  ✗ no active network service".into(), false));
            };
            if p.proxy_pointing_ok(&svc, prt) && rowt_platform::bypass_ok(&svc) {
                Ok((format!("  ✓ system proxy fully configured for '{svc}' (127.0.0.1:{prt} + local bypass)"), true))
            } else {
                Ok((format!("  ✗ system proxy not fully configured — run '{PROG} proxy on'"), false))
            }
        }
        other => Err(format!("usage: {PROG} proxy [status | check | on [--force] | off | env [--off]] (got {other})")),
    }
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
        "add" | "rm" | "remove" | "clear" | "import" => {
            let op = match action {
                "add" => Op::Add(args.to_vec()),
                "clear" => Op::Clear,
                "import" => {
                    let f = args.first().ok_or(format!(
                        "usage: {PROG} {label} import <file>   (one domain per line)"
                    ))?;
                    Op::Import {
                        lines: read(Path::new(f)).lines().map(|s| s.to_string()).collect(),
                        source: f.clone(),
                    }
                }
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
        "version" | "--version" | "-V" => Ok(format!("{PROG} {}", env!("ROWT_SHELL_VERSION"))),
        // hidden: the log splitter re-execs this binary so sing-box's output is
        // classified by a process that outlives the CLI
        "status" => {
            let ctx = Ctx::new(cfg.clone());
            let mut o = Vec::new();
            let mode = ctx.mode();
            if mode == "local" {
                o.push("mode:         local  (no tunnel — the escape lane routes direct)".to_string());
            } else {
                o.push(format!("mode:         {mode}"));
            }
            let servers: Vec<Value> =
                serde_json::from_str(&read(&cfg.join("servers.json"))).unwrap_or_default();
            let subs = read(&cfg.join("subs.txt")).lines()
                .filter(|l| { let t = l.trim(); !t.is_empty() && !t.starts_with('#') }).count();
            let sel = { let s = ctx.sget("selected"); if s.is_empty() { String::new() } else { s } };
            o.push(format!("servers:      {}  (selected: {sel}, subs: {subs})", servers.len()));
            if let Some(now) = lifecycle::clash_selected(&ctx) {
                o.push(format!("active:       {now}"));
            }
            if mode == "vm" {
                o.push(format!("vm ip:        {}", ctx.sget("vm_ip")));
            }
            o.push(format!("router:       {} on 127.0.0.1:{}",
                match lifecycle::host_running(&ctx) {
                    Some(pid) => format!("running (pid {pid})"),
                    None => "stopped".into(),
                }, ctx.port));
            let p = Mac;
            if let Some(svc) = p.active_service() {
                let body = rowt_platform::read_proxy(&svc, "-getsecurewebproxy");
                let en = body.lines().find(|l| l.starts_with("Enabled:"))
                    .and_then(|l| l.split_whitespace().nth(1)).unwrap_or("");
                o.push(format!("system proxy: {en} ({svc})"));
            }
            if ctx.sget("captive") == "1" {
                o.push("captive:      portal detected — proxy dropped until login clears (watchdog auto-restores)".into());
            }
            let esc = read(&cfg.join("escape-domains.txt"));
            let corp = read(&cfg.join("corp-domains.txt"));
            let blk = read(&cfg.join("block-domains.txt"));
            let n_esc = rowt_core::render::parse_list(&esc, rowt_core::render::Filter::All).len();
            let n_corp = rowt_core::render::parse_list(&corp, rowt_core::render::Filter::Domain).len()
                       + rowt_core::render::parse_list(&corp, rowt_core::render::Filter::Cidr).len();
            let n_blk = rowt_core::render::parse_list(&blk, rowt_core::render::Filter::All).len();
            let ads = if cfg.join("cache/geosite-category-ads-all.srs").is_file() { "+ads" } else { "" };
            let mut geo: Vec<String> = rowt_core::render::geosites_of(&esc);
            geo.extend(rowt_core::render::geosites_of(&blk));
            geo.sort(); geo.dedup();
            let geo_s = if geo.is_empty() { String::new() } else { format!(" +geosite:{}", geo.join(",")) };
            o.push(format!("buckets:      escape={n_esc} corp={n_corp} block={n_blk}{ads}{geo_s} final={}",
                           env_or("ROWT_FINAL", "direct")));
            if ctx.sb().is_file() && ctx.host_cfg().is_file() {
                let ok = std::process::Command::new(ctx.sb()).arg("check").arg("-c").arg(ctx.host_cfg())
                    .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
                    .status().map(|s| s.success()).unwrap_or(false);
                o.push(format!("config:       host.json {}",
                    if ok { "OK".to_string() } else { format!("INVALID (run '{PROG} render')") }));
            }
            if lifecycle::host_running(&ctx).is_some() {
                let vc = lifecycle::curl_code(&format!("http://127.0.0.1:{}", ctx.port), "https://www.google.com/");
                let lane = if mode == "local" { "google, direct" } else { "google, escape lane" };
                let tail = if vc == "000" {
                    if mode == "local" { "  — no route to it" } else { "  — tunnel not reachable" }
                } else { "" };
                o.push(format!("via proxy:    HTTP {vc} ({lane}){tail}"));
            }
            Ok(o.join("\n"))
        }
        "shell-init" => match rest.first().map(|s| s.as_str()) {
            None | Some("") => Ok(include_str!(concat!(env!("OUT_DIR"), "/shell_init.txt")).trim_end().to_string()),
            Some("-h") | Some("--help") => Ok(format!("usage: {PROG} shell-init [--install [<rc-file>]]")),
            Some(o) => Err(format!("usage: {PROG} shell-init [--install [<rc-file>]] (got {o})")),
        },
        "completion" => match rest.first().map(|s| s.as_str()) {
            Some("zsh") => Ok(include_str!(concat!(env!("OUT_DIR"), "/completion_zsh.txt")).trim_end().to_string()),
            Some("bash") => Ok(include_str!(concat!(env!("OUT_DIR"), "/completion_bash.txt")).trim_end().to_string()),
            other => Err(format!("unsupported shell: {}  (supported: zsh, bash)", other.unwrap_or(""))),
        },
        "audit" => {
            let log = cfg.join("log/audit.log");
            match rest.first().map(|s| s.as_str()) {
                Some("path") => Ok(log.display().to_string()),
                Some("clear") => {
                    std::fs::write(&log, "").map_err(|e| format!("could not clear {}: {e}", log.display()))?;
                    eprintln!("==> audit log cleared ({})", log.display());
                    Ok(String::new())
                }
                Some("all") => Ok(if log.is_file() { read(&log).trim_end().to_string() }
                                  else { format!("no audit log yet ({})", log.display()) }),
                None | Some("") | Some("-n") => {
                    let n: usize = rest.get(1).and_then(|v| v.parse().ok()).unwrap_or(40);
                    if !log.is_file() {
                        return Ok(format!("no audit log yet ({})", log.display()));
                    }
                    let body = read(&log);
                    let lines: Vec<&str> = body.lines().collect();
                    let tail = lines[lines.len().saturating_sub(n)..].join("\n");
                    Ok(format!("# {} (last {n})\n{tail}", log.display()))
                }
                Some(o) => Err(format!("usage: {PROG} audit [ -n <N> | all | path | clear ] (got {o})")),
            }
        }
        "monitor" | "mon" => {
            // exec the TUI, handing it the CLI it was launched from so its
            // control layer drives this same binary and config
            let here = std::env::current_exe().map_err(|e| e.to_string())?;
            let mut cands = vec![
                here.parent().map(|d| d.join("rowt-monitor")).unwrap_or_default(),
            ];
            if let Ok(p) = std::env::var("ROWT_MONITOR") { cands.insert(0, PathBuf::from(p)); }
            let bin = cands.into_iter().find(|c| c.is_file())
                .ok_or("rowt-monitor not found — build it, or set ROWT_MONITOR")?;
            let err = std::process::Command::new(bin)
                .args(&rest).env("ROWT_BIN", &here).exec_replace();
            Err(err)
        }
        "_splitter" => {
            let hl = rest.first().map(PathBuf::from).ok_or("_splitter needs a log path")?;
            let ld = rest.get(1).map(PathBuf::from).ok_or("_splitter needs a log dir")?;
            lifecycle::run_splitter(&hl, &ld);
            Ok(String::new())
        }
        "render" => {
            let ctx = Ctx::new(cfg);
            lifecycle::cmd_render(&ctx)
        }
        "router" => {
            let ctx = Ctx::new(cfg);
            match rest.first().map(|s| s.as_str()).unwrap_or("up") {
                "up" => lifecycle::router_up(&ctx),
                "down" => Ok(lifecycle::router_down(&ctx)),
                "restart" => { lifecycle::router_stop(&ctx); lifecycle::router_up(&ctx) }
                "status" => Ok(match lifecycle::host_running(&ctx) {
                    Some(pid) => format!("  router: running (pid {pid}) on 127.0.0.1:{}", ctx.port),
                    None => format!("  router: stopped (would listen on 127.0.0.1:{})", ctx.port),
                }),
                o => Err(format!("usage: {PROG} router [up|down|restart|status] (got {o})")),
            }
        }
        "reload" => {
            let ctx = Ctx::new(cfg);
            lifecycle::cmd_render(&ctx)?;
            lifecycle::router_stop(&ctx);
            let r = lifecycle::router_up(&ctx)?;
            let p = lifecycle::proxy_on(&ctx, false);
            eprintln!("==> reloaded (mode={}).", ctx.mode());
            Ok(format!("{r}\n{p}"))
        }
        "restart" => {
            let ctx = Ctx::new(cfg);
            lifecycle::router_stop(&ctx);
            lifecycle::router_up(&ctx)
        }
        "up" => {
            let ctx = Ctx::new(cfg);
            lifecycle::sset(&ctx, "intent", "up");
            lifecycle::cmd_render(&ctx)?;
            lifecycle::router_stop(&ctx);
            let r = lifecycle::router_up(&ctx)?;
            let p = lifecycle::proxy_on(&ctx, false);
            Ok(format!("{r}\n{p}"))
        }
        "down" => {
            let ctx = Ctx::new(cfg);
            lifecycle::sset(&ctx, "intent", "down");
            let p = lifecycle::proxy_off(&ctx);
            let r = lifecycle::router_down(&ctx);
            Ok(format!("{p}\n{r}"))
        }
        "proxy" => {
            let action = rest.first().map(|s| s.as_str()).unwrap_or("status");
            let (out, ok) = cmd_proxy(action, rest.get(1).map(|s| s.as_str()))?;
            if !ok {
                println!("{out}");
                std::process::exit(1);
            }
            Ok(out)
        }
        "" => Err(format!("usage: {PROG}-rs <version|explain|proxy|escape|corp|block> …")),
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
