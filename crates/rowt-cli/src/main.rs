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

mod help;
mod lifecycle;
mod shell;
mod seeds {
    include!(concat!(env!("OUT_DIR"), "/seeds.rs"));
}
use lifecycle::Ctx;
use rowt_core::classify::{classify, ClassifyInput, Lane};
use rowt_core::lanes::{apply, dump, Lanes, Op};
use rowt_platform::{Mac, Platform};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub const PROG: &str = "rowt";

/// The mutating operation in flight, so a fatal error can be attributed to it in
/// the audit log without threading context through every call — the shell keeps
/// `_AUDIT_OP` for exactly this.
static AUDIT_OP: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
pub fn set_audit_op(s: &str) {
    if let Ok(mut g) = AUDIT_OP.lock() {
        *g = s.to_string();
    }
}

/// `die` — the error, an ABORT line if a mutation was in flight, exit 1.
pub fn die(cfg: &Path, msg: &str) -> ! {
    eprintln!("error: {msg}");
    let op = AUDIT_OP.lock().map(|g| g.clone()).unwrap_or_default();
    if !op.is_empty() {
        shell::audit(cfg, &format!("ABORT {op}: {msg}"));
    }
    std::process::exit(1);
}

/// Which arms rowt-rs answers itself.
///
/// Everything else falls through to bin/rowt, which is installed alongside for
/// exactly this reason (PORTING.md §6.6). That is what makes rowt-rs a complete
/// front door today rather than after the last arm lands: an unported command
/// runs, it just runs in the shell. `parity cli-ledger` reads this table, so the
/// published coverage is measured from the same source that decides it.
/// Every name here MUST be answered by `run`. A name listed but unimplemented is
/// strictly worse than one that was never listed: before the fallthrough existed
/// it failed loudly, and now it would claim coverage while failing. Add a name
/// the same commit that lands its arm, never before.
fn native(cmd: &str, sub: &str) -> bool {
    match cmd {
        "explain" | "route" | "version" | "--version" | "-V" | "status" | "audit"
        | "shell-init" | "completion" | "monitor" | "mon" | "render" | "reload"
        | "restart" | "up" | "down" | "help" | "-h" | "--help" | "_splitter" => true,
        "escape" | "corp" | "block" => matches!(
            sub,
            "" | "list" | "dump" | "add" | "rm" | "remove" | "clear" | "import"
                | "errors" | "stats" | "log"
        ),
        "direct" | "connections" | "conns" | "_complete" => true,
        "proxy" => matches!(sub, "" | "status" | "check" | "env"),
        // read arms only — the rest drive the Python importers
        "server" => matches!(sub, "" | "list" | "dump"),
        "sub" => matches!(sub, "" | "list" | "dump"),
        "router" => matches!(sub, "" | "up" | "down" | "restart" | "status"),
        _ => false,
    }
}

/// Hand the whole invocation to bin/rowt. Found next to this binary (how the
/// Formula lays it out), then via ROWT_LEGACY, then in the repo tree — so it
/// works installed, in a sandbox, and from `cargo run` alike.
fn delegate(args: &[String]) -> ! {
    use std::os::unix::process::CommandExt;
    let mut cands: Vec<PathBuf> = Vec::new();
    if let Ok(p) = std::env::var("ROWT_LEGACY") {
        cands.push(PathBuf::from(p));
    }
    if let Ok(here) = std::env::current_exe() {
        if let Some(d) = here.parent() {
            cands.push(d.join("rowt-legacy"));
            cands.push(d.join("rowt"));
            cands.push(d.join("../../bin/rowt"));
        }
    }
    let Some(sh) = cands.into_iter().find(|c| c.is_file()) else {
        eprintln!("error: no legacy rowt found — set ROWT_LEGACY=/path/to/bin/rowt");
        std::process::exit(1);
    };
    // The shell already reaches for Rust binaries (`_render_bin`, `_watch_bin`),
    // and §6.6 has it becoming a wrapper. If it ever reaches back for rowt-rs on
    // a path that delegates, exec-ing again would spin forever and take the
    // machine with it. One marker turns that into an error message.
    if std::env::var("ROWT_DELEGATED").as_deref() == Ok("1") {
        eprintln!("error: refusing to delegate twice — {} would exec back into rowt-rs", sh.display());
        std::process::exit(1);
    }
    // `exec`, not spawn-and-wait: the shell must own the terminal and the exit
    // status directly, or an interactive command (sudo prompt, `<lane> log`)
    // behaves differently through rowt-rs than through rowt.
    let e = std::process::Command::new("bash").arg(sh).args(args)
        .env("ROWT_DELEGATED", "1").exec();
    eprintln!("error: could not exec the legacy rowt: {e}");
    std::process::exit(1);
}

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

fn set_mode(p: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode))
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
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
    let ctx = Ctx::new(cfg.to_path_buf());
    let lanes = load_lanes(cfg);
    let mode = ctx.mode();
    let private: Vec<String> =
        ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "100.64.0.0/10", "169.254.0.0/16"]
            .iter().map(|s| s.to_string()).collect();
    let final_route = Lane::parse(&env_or("ROWT_FINAL", "direct")).unwrap_or(Lane::Direct);
    let pd = env_or("ROWT_PRIVATE_DEFAULT", "corp");
    // Resolved only on the branch that needs it — see `needs_resolution`.
    let ip = if rowt_core::classify::needs_resolution(dest, &lanes.escape, &lanes.corp, &lanes.block) {
        rowt_platform::resolve_ip(&rowt_core::classify::normalize_dest(dest))
    } else {
        String::new()
    };
    let c = classify(dest, &ClassifyInput {
        escape_list: &lanes.escape, corp_list: &lanes.corp, block_list: &lanes.block,
        private_cidrs: &private, private_default: &pd, final_route,
        local_mode: mode == "local", resolved_ip: &ip,
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
    } else if lifecycle::host_running(&ctx).is_some() {
        // Deliberately no `--noproxy '*'` — that would override -x and send the
        // probe DIRECT, which is exactly not the path being tested.
        let code = lifecycle::curl_code(&format!("http://127.0.0.1:{}", ctx.port),
                                        &format!("https://{}/", c.dest));
        out.push_str(&format!("\n  live:    HTTP {code} through the router{}",
            if code == "000" { "  (000 = lane not reachable)" } else { "" }));
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

/// `printf "%-Ns"` pads to N BYTES in awk under LC_ALL=C, where Rust's `{:<N}`
/// pads to N chars. Identical for ASCII and different the moment a host is an
/// IDN — and connection tables are exactly where a non-ASCII host shows up.
fn pad(s: &str, width: usize) -> String {
    let mut o = s.to_string();
    for _ in s.len()..width {
        o.push(' ');
    }
    o
}

/// awk's `hb()` — bytes, humanised. `%.0f`/`%.1f` round half-to-even in C, which
/// is what Rust's `{:.0}`/`{:.1}` do too.
fn human_bytes(b: u64) -> String {
    match b {
        b if b < 1024 => format!("{b}B"),
        b if b < 1_048_576 => format!("{:.0}K", b as f64 / 1024.0),
        b if b < 1_073_741_824 => format!("{:.1}M", b as f64 / 1_048_576.0),
        b => format!("{:.1}G", b as f64 / 1_073_741_824.0),
    }
}

/// The router's live connections, from the clash API, with the lane each is on.
/// Unlike `<lane> errors` this shows SUCCESSFUL traffic — it is a snapshot of
/// what is open, not a log of what failed.
fn connections_show(ctx: &Ctx, filt: &str) -> String {
    if lifecycle::host_running(ctx).is_none() {
        return format!("router isn't running — '{PROG} up' first.");
    }
    let Some(body) = lifecycle::clash_curl(ctx, "GET", "/connections", None).filter(|b| !b.is_empty())
    else {
        return format!("couldn't reach the clash API ({}) — is the router up?", ctx.controller());
    };
    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let empty = vec![];
    let conns = v.get("connections").and_then(|c| c.as_array()).unwrap_or(&empty);

    struct Row { lane: &'static str, hp: String, up: u64, down: u64, rule: String }
    let mut rows: Vec<Row> = Vec::new();
    for c in conns {
        // jq's `index("block")` over an ARRAY is an exact element match, not a
        // substring search — a chain named "blocklist" would not count.
        let chains: Vec<&str> =
            c.get("chains").and_then(|x| x.as_array())
             .map(|a| a.iter().filter_map(|x| x.as_str()).collect()).unwrap_or_default();
        let lane = if chains.contains(&"block") { "block" }
                   else if chains.contains(&"direct") { "direct" }
                   else if chains.contains(&"corp") { "corp" }
                   else { "escape" };
        if !filt.is_empty() && lane != filt {
            continue;
        }
        let md = c.get("metadata");
        let nz = |k: &str| md.and_then(|m| m.get(k)).and_then(|x| x.as_str()).filter(|s| !s.is_empty());
        let host = nz("host").or_else(|| nz("destinationIP")).unwrap_or("?").to_string();
        // `@tsv` renders a number as a number and a string as itself, so a port
        // arrives either way; `// "?"` only when the key is absent or null.
        let port = md.and_then(|m| m.get("destinationPort")).map(|p| match p {
            Value::String(s) => s.clone(),
            Value::Null => "?".into(),
            other => other.to_string(),
        }).unwrap_or_else(|| "?".into());
        let num = |k: &str| c.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
        // `match(r, /^[A-Za-z_]+/)` — the rule's leading word only ("RuleSet",
        // "DomainSuffix"), so the column stays narrow.
        let raw = c.get("rule").and_then(|x| x.as_str()).unwrap_or("");
        let rule: String = raw.chars().take_while(|c| c.is_ascii_alphabetic() || *c == '_').collect();
        rows.push(Row { lane, hp: format!("{host}:{port}"), up: num("upload"), down: num("download"),
                        rule: if rule.is_empty() { raw.to_string() } else { rule } });
    }
    if rows.is_empty() {
        let on = if filt.is_empty() { String::new() } else { format!(" on the {filt} lane") };
        return format!("no active connections{on} right now.");
    }

    let mut out = format!("{} active connections:", rows.len());
    for l in ["escape", "direct", "corp", "block"] {
        let n = rows.iter().filter(|r| r.lane == l).count();
        if n > 0 {
            out.push_str(&format!("  {l}={n}"));
        }
    }

    // Aggregate by lane + host:port, which collapses the many parallel dials one
    // client makes to a single API host into one row.
    use std::collections::BTreeMap;
    let mut agg: BTreeMap<(&str, String), (u64, u64, u64, String)> = BTreeMap::new();
    for r in &rows {
        let e = agg.entry((r.lane, r.hp.clone())).or_insert((0, 0, 0, String::new()));
        e.0 += 1;
        e.1 += r.up;
        e.2 += r.down;
        e.3 = r.rule.clone(); // last one wins, as awk's R[k]= does
    }
    // `sort -t<TAB> -k1,1 -k4,4nr`: lane ascending, download DESCENDING, and the
    // whole line ascending as the last resort (`-r` rides on the k4 key alone,
    // not on the final comparison).
    let mut lines: Vec<(&str, u64, u64, u64, String, String)> = agg
        .into_iter()
        .map(|((lane, hp), (c, u, d, rule))| (lane, c, u, d, hp, rule))
        .collect();
    lines.sort_by(|a, b| {
        a.0.cmp(b.0)
            .then_with(|| b.3.cmp(&a.3))
            .then_with(|| {
                let f = |x: &(&str, u64, u64, u64, String, String)| {
                    format!("{}\t{}\t{}\t{}\t{}\t{}", x.0, x.1, x.2, x.3, x.4, x.5)
                };
                f(a).cmp(&f(b))
            })
    });
    for (lane, count, up, down, hp, rule) in lines {
        out.push_str(&format!("\n  {} {} {:>2}× ↑{} ↓{} {}",
            pad(lane, 7), pad(&hp, 40), count,
            pad(&human_bytes(up), 7), pad(&human_bytes(down), 8), rule));
    }
    out
}

/// `_period_cutoff` — the parse is pure, the clock is not. `date -v` rather than
/// a time crate: same program, same DST and zone rules, no second answer to keep
/// in sync.
fn period_cutoff(p: &str) -> Result<String, ()> {
    let Some((n, u)) = rowt_core::laneerr::parse_period(p)? else { return Ok(String::new()) };
    let flag = format!("-v-{n}{}", match u { 'm' => 'M', 'h' => 'H', _ => 'd' });
    let o = std::process::Command::new("date").arg(&flag).arg("+%Y-%m-%d %H:%M:%S")
        .stderr(std::process::Stdio::null()).output().map_err(|_| ())?;
    Ok(String::from_utf8_lossy(&o.stdout).trim_end().to_string())
}

/// `lane_errors` — the per-lane failure summary.
fn cmd_lane_errors(cfg: &Path, lane: &str, period: &str) -> Result<String, String> {
    let base = cfg.join(format!("log/lane-{lane}.log"));
    let Ok(cutoff) = period_cutoff(period) else {
        die(cfg, &format!("usage: {PROG} {lane} errors [5m|10m|1h|24h|7d|all]"));
    };
    // block refuses by design, so its verb is different — "nothing blocked" is
    // a healthy sinkhole, "no escape errors" is a healthy tunnel.
    let (noun, empty) = if lane == "block" {
        ("blocked", "nothing blocked".to_string())
    } else {
        ("failed", format!("no {lane} errors"))
    };

    // The live log plus every rotation of it (`for f in "$base" "$base".*`).
    let mut bodies = String::new();
    let mut any = false;
    let mut files: Vec<PathBuf> = Vec::new();
    if base.is_file() {
        files.push(base.clone());
    }
    if let Ok(rd) = std::fs::read_dir(cfg.join("log")) {
        let prefix = format!("lane-{lane}.log.");
        let mut rot: Vec<PathBuf> = rd.flatten()
            .map(|e| e.path())
            .filter(|p| p.file_name().map(|n| n.to_string_lossy().starts_with(&prefix)).unwrap_or(false))
            .collect();
        // Glob order, which is what the shell's `"$base".*` expands to.
        rot.sort();
        files.extend(rot);
    }
    for f in &files {
        any = true;
        bodies.push_str(&read(f));
    }
    let ctx = Ctx::new(cfg.to_path_buf());
    if !any {
        let mut o = format!("{empty} recorded — only failed/refused connections are logged, not successful traffic (so an empty list means no errors, not no traffic).");
        if lifecycle::host_running(&ctx).is_none() {
            o.push_str(&format!("\n  (the router isn't running — '{PROG} up' first, then reproduce the app)"));
        }
        return Ok(o);
    }

    let rows = rowt_core::laneerr::tally(&bodies, &cutoff);
    let tot: u64 = rows.iter().map(|r| r.count).sum();
    let doms = rows.len();
    let window = if cutoff.is_empty() { String::new() } else { format!(" in the last {period}") };
    let mut o = format!("{lane} lane — {tot} {noun} connection(s){window}, across {doms} domain(s):");
    if tot == 0 {
        o.push_str(&format!("\n  ({empty}{window} — remember only errors are logged, not successful traffic)"));
        return Ok(o);
    }
    for r in rows.iter().take(40) {
        o.push_str(&format!("\n  {:>7}  {:<8} {}", r.count, r.cat, r.domain));
    }
    if doms > 40 {
        o.push_str(&format!("\n  … {} more (widen with 'all', or narrow with '5m')", doms - 40));
    }
    if lane == "direct" || lane == "corp" {
        o.push_str(&format!("\n  → tunnel the real ones: {PROG} escape add <domain>  (timeout/reset/refused ⇒ likely blocked; dns ⇒ often transient)"));
    }
    Ok(o)
}

/// `_lane_log_tail` — `tail -f` on the lane's log, exec'd so Ctrl-C reaches it.
fn cmd_lane_log(cfg: &Path, lane: &str) -> Result<String, String> {
    let f = cfg.join(format!("log/lane-{lane}.log"));
    if !f.is_file() {
        die(cfg, &format!("no {lane} log yet — start the router first: {PROG} up"));
    }
    Err(std::process::Command::new("tail").arg("-f").arg(&f).exec_replace())
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
        // Same bare-newline shape as `sub dump` — an empty lane dumps one.
        "dump" => {
            match args.first() {
                Some(f) => {
                    std::fs::write(f, format!("{}\n", dump(&body).join("\n")))
                        .map_err(|e| format!("write: {e}"))?;
                    eprintln!("  dumped {label} list -> {f}");
                }
                None => println!("{}", dump(&body).join("\n")),
            }
            Ok(String::new())
        }
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

fn run(cfg: &Path, cmd: &str, rest: &[String]) -> Result<String, String> {
    let cfg = cfg.to_path_buf();
    let rest: Vec<String> = rest.to_vec();
    match cmd {
        "explain" | "route" => {
            let d = rest.first().ok_or(format!("usage: {PROG} explain <domain|ip>"))?;
            Ok(cmd_explain(&cfg, d))
        }
        "escape" | "corp" | "block" => {
            let lane = Lane::parse(cmd).unwrap();
            let action = rest.first().cloned().unwrap_or_else(|| "list".into());
            let args = &rest[1.min(rest.len())..];
            match action.as_str() {
                // block's window defaults to a day, not ten minutes: a sinkhole
                // that refused nothing in the last ten minutes is normal.
                "errors" | "stats" => {
                    let d = if cmd == "block" { "24h" } else { "10m" };
                    cmd_lane_errors(&cfg, cmd, args.first().map(|s| s.as_str()).unwrap_or(d))
                }
                "log" => cmd_lane_log(&cfg, cmd),
                _ => cmd_lane(&cfg, lane, &action, args),
            }
        }
        // `server` / `sub`: the READ arms only. Everything that changes the pool
        // — add, import, rm, clear, update — runs the Python importers and
        // `rebuild_servers`, so it stays with the shell (PORTING.md §5 phase 6:
        // 1,484 lines of tested parsing Python is the lowest-ROI thing in the
        // tree to rewrite, and may stay Python indefinitely).
        "server" => {
            let ctx = Ctx::new(cfg.clone());
            let servers: Vec<Value> =
                serde_json::from_str(&read(&cfg.join("servers.json"))).unwrap_or_default();
            match rest.first().map(|s| s.as_str()).unwrap_or("list") {
                "list" => {
                    if servers.is_empty() {
                        die(&cfg, &format!("no servers — '{PROG} server add <link>' or '{PROG} sub add <url>'"));
                    }
                    let now = lifecycle::clash_selected(&ctx).unwrap_or_default();
                    let live = if now.is_empty() { String::new() } else { format!(", live: {now}") };
                    let mut o = format!("servers (selected: {}{live}):", ctx.sget("selected"));
                    for s in &servers {
                        let g = |k: &str| s.get(k).map(|v| match v {
                            Value::String(x) => x.clone(), o => o.to_string(),
                        }).unwrap_or_default();
                        let tag = g("tag");
                        let mark = if tag == now && !now.is_empty() { "* " } else { "  " };
                        o.push_str(&format!("\n{mark}{} {} {}:{}",
                            pad(&tag, 18), pad(&g("type"), 7), g("server"), g("server_port")));
                    }
                    Ok(o)
                }
                "dump" => {
                    let manual = read(&cfg.join("manual.json"));
                    let out = serde_json::from_str::<Value>(&manual)
                        .map(|v| serde_json::to_string_pretty(&v).unwrap_or_else(|_| "[]".into()))
                        .unwrap_or_else(|_| "[]".into());
                    match rest.get(1) {
                        Some(f) => {
                            std::fs::write(f, format!("{out}\n")).map_err(|e| e.to_string())?;
                            let _ = set_mode(Path::new(f), 0o600);
                            eprintln!("  dumped manual servers -> {f} (contains secrets, chmod 600)");
                            Ok(String::new())
                        }
                        None => { println!("{out}"); Ok(String::new()) }
                    }
                }
                o => die(&cfg, &format!("usage: {PROG} server [list | add <link>… | rm <tag>… | clear | import <--detect|--from SRC [--output FILE]|--apply [--input FILE]> | dump [file]]  (got {o})")),
            }
        }
        "sub" => {
            let subs = read(&cfg.join("subs.txt"));
            let active: Vec<&str> = subs.lines()
                .filter(|l| { let t = l.trim(); !t.is_empty() && !t.starts_with('#') }).collect();
            match rest.first().map(|s| s.as_str()).unwrap_or("list") {
                "list" => {
                    let mut o = String::from("subscriptions:");
                    if subs.is_empty() {
                        o.push_str("\n  (none)");
                    } else {
                        // `grep -n` numbers by position in the FILE, so a comment
                        // line shifts the numbers of everything after it — and
                        // those numbers are what `sub rm <n>` takes.
                        for (i, l) in subs.lines().enumerate() {
                            let t = l.trim();
                            if !t.is_empty() && !t.starts_with('#') {
                                o.push_str(&format!("\n  {}  {l}", i + 1));
                            }
                        }
                    }
                    let n = serde_json::from_str::<Vec<Value>>(&read(&cfg.join("manual.json")))
                        .map(|v| v.len().to_string()).unwrap_or_else(|_| "0".into());
                    o.push_str(&format!("\nmanual servers: {n}"));
                    if subs.is_empty() {
                        o.push_str(&format!("\n  {PROG} sub add <url> | rm <n|url> | update | clear"));
                    }
                    Ok(o)
                }
                "dump" => {
                    let body = active.join("\n");
                    match rest.get(1) {
                        Some(f) => {
                            std::fs::write(f, format!("{body}\n")).map_err(|e| e.to_string())?;
                            eprintln!("  dumped subscriptions -> {f}");
                            Ok(String::new())
                        }
                        // `printf '%s\n' "$entries"` prints a bare newline when
                        // there are no subscriptions. main() suppresses an empty
                        // result (so `_complete` offers no blank candidate), so
                        // this one prints for itself.
                        None => { println!("{body}"); Ok(String::new()) }
                    }
                }
                o => die(&cfg, &format!("usage: {PROG} sub [list | add <url>… | rm <n|url> | update | clear | import [--apply] | dump [file]]  (got {o})")),
            }
        }
        // The hidden helper the completion functions call at tab-time. Driven by
        // the same registry that renders `usage`, so the command set can never
        // drift from what completes.
        "_complete" => {
            let first = |syntax: &str| syntax.split_whitespace().next().unwrap_or("").to_string();
            if rest.is_empty() {
                return Ok(help::reg_rows()
                    .map(|(_, _, syn, desc)| format!("{}\t{desc}", first(syn)))
                    .collect::<Vec<_>>().join("\n"));
            }
            // Only one level deep — anything further is the shell's business.
            if rest.len() != 1 {
                return Ok(String::new());
            }
            let sub = rest[0].as_str();
            let tags = || -> Vec<String> {
                serde_json::from_str::<Vec<Value>>(&read(&cfg.join("servers.json")))
                    .unwrap_or_default().iter()
                    .filter_map(|s| s.get("tag").and_then(|t| t.as_str()).map(|t| t.to_string()))
                    .collect()
            };
            match sub {
                "use" => {
                    let mut o: Vec<String> = tags().iter().map(|t| format!("{t}\tserver")).collect();
                    o.push("auto\tpick fastest live".into());
                    return Ok(o.join("\n"));
                }
                "ping" => return Ok(tags().iter().map(|t| format!("{t}\tserver"))
                                     .collect::<Vec<_>>().join("\n")),
                "help" => return Ok(help::reg_rows()
                    .map(|(_, _, syn, _)| format!("{}\t", first(syn)))
                    .collect::<Vec<_>>().join("\n")),
                _ => {}
            }
            let Some((_, _, syn, _)) = help::reg_rows().find(|(_, _, s, _)| first(s) == sub) else {
                return Ok(String::new());
            };
            Ok(help::choice_tokens(syn).iter().map(|t| format!("{t}\t"))
               .collect::<Vec<_>>().join("\n"))
        }
        "connections" | "conns" => {
            let ctx = Ctx::new(cfg);
            match rest.first().map(|s| s.as_str()) {
                Some("-w") | Some("--watch") => {
                    let filt = rest.get(1).cloned().unwrap_or_default();
                    loop {
                        print!("\x1b[H\x1b[2J");
                        let t = std::process::Command::new("date").arg("+%H:%M:%S")
                            .output().map(|o| String::from_utf8_lossy(&o.stdout).trim_end().to_string())
                            .unwrap_or_default();
                        println!("rowt connections — {t}   (Ctrl-C to stop)");
                        println!("{}", connections_show(&ctx, &filt));
                        std::thread::sleep(std::time::Duration::from_secs(2));
                    }
                }
                other => Ok(connections_show(&ctx, other.unwrap_or(""))),
            }
        }
        // 'direct' is not an editable lane — it is the pass-through default, so
        // only the diagnostics exist. What FAILED going direct is the useful
        // question: those are the escape-lane candidates.
        "direct" => match rest.first().map(|s| s.as_str()).unwrap_or("errors") {
            "errors" | "stats" => {
                cmd_lane_errors(&cfg, "direct", rest.get(1).map(|s| s.as_str()).unwrap_or("10m"))
            }
            "log" => cmd_lane_log(&cfg, "direct"),
            _ => die(&cfg, &format!("usage: {PROG} direct [errors [5m|10m|1h|…|all] | log]")),
        },
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
        // `native` gates what reaches here, so this is unreachable in practice;
        // it stays as the same last line of defence the shell's `*)` arm is.
        other => Err(format!("unknown command: {other}")),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = config_dir();
    // Falls through to the shell BEFORE the preamble runs, not after: bash's own
    // main() migrates, rotates and audits, and doing it on both sides would
    // double every audit line.
    match args.first() {
        None => delegate(&args), // no args: the shell's greeting + full command list
        Some(cmd) => {
            // Help is answered here for every arm, ported or not — the text comes
            // out of bin/rowt at build time, so a delegated command's help is the
            // same page either way and this only saves a process.
            let wants_help = matches!(cmd.as_str(), "help" | "-h" | "--help")
                || (!matches!(cmd.as_str(), "run" | "monitor" | "mon")
                    && args[1..].iter().any(|a| a == "--help" || a == "-h"));
            let sub = args.get(1).cloned().unwrap_or_default();
            if !wants_help && !native(cmd, &sub) {
                delegate(&args);
            }
        }
    }
    match shell::dispatch(&cfg, &args, |cmd, rest| run(&cfg, cmd, rest)) {
        // An arm with nothing to say prints NOTHING, not a blank line: the shell
        // reaches its `return` without an echo, and `_complete nosuchcommand`
        // feeding one empty candidate to the completion system is a real
        // difference, not a cosmetic one.
        Ok(s) => {
            if !s.is_empty() {
                println!("{s}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
