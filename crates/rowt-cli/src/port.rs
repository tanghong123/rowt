//! `rowt port add <N> <name> [--final escape|direct|corp] | rm <name> | list` —
//! named extra ports: loopback mixed inbounds that run main's rules and differ
//! only in their catch-all.
//!
//! `cmd_port`, `_port_add`, `_port_rm`, `_port_list`, `_port_apply`,
//! `_port_bind_probe` and `_ports_write` from bin/rowt (rowt #s73). The table
//! is read by `rowt_core::ports`, which the render, `status` and `explain`
//! already share. The shell's wording is the specification: its `die`s reach
//! the audit log, `list` reaches stdout, and both are compared by cli-diff.

use crate::lifecycle::{self, Ctx};
use crate::{die, PROG};
use rowt_core::ports::{self, Port};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

const HEADER: &str = "# rowt: named extra ports (name, port, catch-all) — 'rowt port add <N> <name> [--final escape|direct|corp]', 'port rm <name>', 'port list'";

fn info(msg: &str) {
    eprintln!("==> {msg}");
}
fn warn(msg: &str) {
    eprintln!("warning: {msg}");
}

fn tsv(ctx: &Ctx) -> PathBuf {
    ctx.cfg.join("ports.tsv")
}
fn rows(ctx: &Ctx) -> Vec<Port> {
    ports::rows(&lifecycle::read(&tsv(ctx)))
}
fn line(p: &Port) -> String {
    format!("{}\t{}\t{}", p.name, p.port, p.fin)
}
fn final_route() -> String {
    lifecycle::env_or("ROWT_FINAL", "direct")
}
fn local(ctx: &Ctx) -> bool {
    ctx.sget("mode") == "local"
}

pub fn cmd_port(ctx: &Ctx, args: &[String]) -> Result<String, String> {
    let action = args.first().map(|s| s.as_str()).unwrap_or("list");
    let rest = if args.is_empty() { &args[..] } else { &args[1..] };
    match action {
        "add" => add(ctx, rest),
        "rm" | "remove" => rm(ctx, rest),
        "list" | "ls" => list(ctx, rest),
        _ => die(&ctx.cfg, &format!("usage: {PROG} port <add <N> <name> [--final escape|direct|corp] | rm <name> | list>")),
    }
}

/// `_ports_write`: the rows sorted and deduplicated as `LC_ALL=C sort -u`, the
/// header above them — or no file at all when there are none.
fn write(ctx: &Ctx, mut lines: Vec<String>) {
    lines.retain(|l| !l.is_empty());
    lines.sort();
    lines.dedup();
    let p = tsv(ctx);
    if lines.is_empty() {
        let _ = std::fs::remove_file(&p);
        return;
    }
    let n = ctx.cfg.join("ports.tsv.n");
    if std::fs::write(&n, format!("{HEADER}\n{}\n", lines.join("\n"))).is_ok() {
        let _ = std::fs::rename(&n, &p);
    }
}

fn check_name(ctx: &Ctx, name: &str) {
    match name {
        "main" => die(&ctx.cfg, &format!("'main' is 127.0.0.1:{}, rowt's own port", ctx.port)),
        "escape" | "corp" | "block" | "direct" | "hotspot" => {
            die(&ctx.cfg, &format!("'{name}' is a lane name — pick another name for the port"))
        }
        _ => {}
    }
    let ok = (1..=32).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !ok {
        die(&ctx.cfg, &format!("a port name is 1-32 of a-z, 0-9 and '-' (got '{name}')"));
    }
}

fn add(ctx: &Ctx, args: &[String]) -> Result<String, String> {
    let usage = format!("usage: {PROG} port add <N> <name> [--final escape|direct|corp]");
    let (port_s, name) = match (args.first(), args.get(1)) {
        (Some(p), Some(n)) if !p.is_empty() && !n.is_empty() => (p.clone(), n.clone()),
        _ => die(&ctx.cfg, &usage),
    };
    let mut fin = String::new();
    let mut it = args[2..].iter();
    while let Some(a) = it.next() {
        if a == "--final" {
            match it.next() {
                Some(v) => fin = v.clone(),
                None => die(&ctx.cfg, &usage),
            }
        } else if let Some(v) = a.strip_prefix("--final=") {
            fin = v.to_string();
        } else {
            die(&ctx.cfg, &format!("unknown argument: {a}  ({usage})"));
        }
    }
    if fin.is_empty() {
        fin = final_route();
    }
    if !matches!(fin.as_str(), "escape" | "direct" | "corp") {
        die(&ctx.cfg, &format!("--final wants escape, direct or corp (got '{fin}')"));
    }
    if !port_s.bytes().all(|b| b.is_ascii_digit()) {
        die(&ctx.cfg, &format!("the port wants a number, got '{port_s}'"));
    }
    // `$((10#$port))`: leading zeros go, and the number is what is checked.
    let port: u64 = port_s.trim_start_matches('0').parse().unwrap_or(if port_s.trim_start_matches('0').is_empty() { 0 } else { u64::MAX });
    if !(1..=65535).contains(&port) {
        die(&ctx.cfg, &format!("the port must be 1-65535, got {port}"));
    }
    if port < 1024 {
        die(&ctx.cfg, "ports under 1024 need root, and rowt's router runs as you — use 1024 or above");
    }
    check_name(ctx, &name);
    let (main, vm, clash, vmc) = (ctx.port as u64, ctx.port as u64 + 1, ctx.clash_port as u64, ctx.clash_port as u64 + 1);
    if [main, vm, clash, vmc].contains(&port) {
        die(&ctx.cfg, &format!("port {port} is one of rowt's own (main {main}, vm {vm}, clash {clash}/{vmc}) — pick another"));
    }
    let local = local(ctx);
    let had = rows(ctx);
    let had_l: Vec<String> = had.iter().map(line).collect();
    let without: Vec<String> = had.iter().filter(|r| r.name != name).map(line).collect();
    let dest = ports::dest(&fin, local).to_string();
    let far = if dest == "escape" { " (names resolved at the far end)" } else { "" };
    let want = format!("{name}\t{port}\t{fin}");
    if let Some(row) = had.iter().find(|r| r.name == name) {
        // The same row again is a no-op that still goes through the apply; a
        // different port or catch-all under the name is a change, refused.
        if line(row) == want {
            return apply(ctx, &had_l, &without, Some(port), None, &format!("port {name} is already 127.0.0.1:{port}, unmatched -> {dest}"));
        }
        die(&ctx.cfg, &format!(
            "a port named '{name}' exists: 127.0.0.1:{}, unmatched -> {} — '{PROG} port rm {name}' first",
            row.port, row.fin
        ));
    }
    // awk's `$2 == p` compares as numbers: 07892 is port 7892.
    let taken: Vec<&str> = had.iter().filter(|r| r.port.parse::<u64>().ok() == Some(port)).map(|r| r.name.as_str()).collect();
    if !taken.is_empty() {
        die(&ctx.cfg, &format!("port {port} is already the port named '{}'", taken.join("\n")));
    }
    // Someone else's listener would stop the router from starting at all. lsof
    // names the owner when it can; a connect comes next (a root-owned listener
    // lsof cannot see as a user); a short run of sing-box binding exactly that
    // port is the last word. The running router is the one owner allowed.
    let hp = lifecycle::read(&ctx.pidfile()).trim_end_matches('\n').to_string();
    let owner = cmd_out("lsof", &["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-t"]).lines().next().unwrap_or("").to_string();
    if !owner.is_empty() && owner != hp {
        let comm = Command::new("ps").args(["-o", "comm=", "-p", &owner]).stderr(Stdio::null()).output().ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').to_string())
            .unwrap_or_else(|| "?".into());
        die(&ctx.cfg, &format!("127.0.0.1:{port} is already in use (pid {owner}, {comm}) — pick another"));
    }
    if owner.is_empty() {
        if open(port) {
            die(&ctx.cfg, &format!("127.0.0.1:{port} already answers, and not for rowt (an owner lsof cannot see — a root service?) — pick another"));
        }
        if !bind_probe(ctx, port) {
            die(&ctx.cfg, &format!("127.0.0.1:{port} cannot be bound by sing-box here (held by something lsof cannot see, or blocked) — pick another"));
        }
    }
    let mut next = without.clone();
    next.push(want);
    write(ctx, next);
    apply(ctx, &had_l, &without, Some(port), None, &format!(
        "port {name}: 127.0.0.1:{port} — unmatched traffic -> {dest}{far}; main 127.0.0.1:{} unchanged",
        ctx.port
    ))
}

fn rm(ctx: &Ctx, args: &[String]) -> Result<String, String> {
    let name = match args {
        [n] if !n.is_empty() => n.clone(),
        _ => die(&ctx.cfg, &format!("usage: {PROG} port rm <name>")),
    };
    if name == "main" {
        die(&ctx.cfg, &format!("main (127.0.0.1:{}) is rowt's own port and stays", ctx.port));
    }
    let had = rows(ctx);
    let Some(row) = had.iter().find(|r| r.name == name) else {
        info(&format!("no port named '{name}' — nothing to remove ('{PROG} port list')"));
        return Ok(String::new());
    };
    let port: u64 = row.port.parse().unwrap_or(0);
    let had_l: Vec<String> = had.iter().map(line).collect();
    let without: Vec<String> = had.iter().filter(|r| r.name != name).map(line).collect();
    write(ctx, without.clone());
    apply(ctx, &had_l, &without, None, Some(port), &format!("port {name} removed (was 127.0.0.1:{})", row.port))
}

fn list(ctx: &Ctx, args: &[String]) -> Result<String, String> {
    if !args.is_empty() {
        die(&ctx.cfg, &format!("usage: {PROG} port list"));
    }
    let running = lifecycle::host_running(ctx).is_some();
    let local = local(ctx);
    let mut out = vec![format!(
        "{:<10} {:<18} unmatched -> {} (the system proxy port)",
        "main",
        format!("127.0.0.1:{}", ctx.port),
        final_route()
    )];
    let rendered = ports::rendered(&lifecycle::read(&ctx.host_cfg()));
    let rs = rows(ctx);
    for r in &rs {
        // Every rendered inbound of that name, as the awk prints them.
        let prp: Vec<&str> = rendered.iter().filter(|(n, _)| n == &r.name).map(|(_, p)| p.as_str()).collect();
        let pp: u16 = r.port.parse().unwrap_or(0);
        let note = if prp.join("\n") != r.port {
            format!("  — not in host.json yet: '{PROG} render'")
        } else if running && !open(pp as u64) {
            format!("  — not answering: '{PROG} router restart'")
        } else if running {
            "  (listening)".into()
        } else {
            String::new()
        };
        out.push(format!("{:<10} {:<18} unmatched -> {}{note}", r.name, format!("127.0.0.1:{}", r.port), catchall(&r.fin, local)));
    }
    if rs.is_empty() {
        out.push(format!(
            "(no named ports — '{PROG} port add <N> <name> [--final escape|direct|corp]' adds one, e.g. 'port add 7892 guest --final escape' for VMs)"
        ));
    }
    let body = lifecycle::read(&tsv(ctx));
    let kept = body.lines().filter(|l| !l.starts_with('#') && l.split_whitespace().next().is_some()).count();
    let sk = kept.saturating_sub(rs.len());
    if sk != 0 {
        // stdout first: the warning is the shell's last line, after the table.
        println!("{}", out.join("\n"));
        warn(&format!(
            "{sk} line(s) in {} skipped: not name/port/catch-all, a port used twice, or a catch-all other than escape|direct|corp",
            tsv(ctx).display()
        ));
        return Ok(String::new());
    }
    Ok(out.join("\n"))
}

/// `_port_catchall`: the word in the table, with what it means here.
fn catchall(fin: &str, local: bool) -> String {
    match fin {
        "escape" if local => "escape (direct in local mode)".into(),
        "escape" => "escape, names resolved at the far end".into(),
        f => f.into(),
    }
}

fn cmd_out(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

/// `_port_open`: does anything accept a connection on 127.0.0.1:N? A bare
/// connect, no bytes — a request to a mixed port would be proxied, to itself.
fn open(port: u64) -> bool {
    let Ok(p) = u16::try_from(port) else { return false };
    std::net::TcpStream::connect_timeout(&std::net::SocketAddr::from(([127, 0, 0, 1], p)), Duration::from_secs(2)).is_ok()
}

/// `_port_wait`: up to ~5 s for the port to open (`want`) or close.
fn wait(port: u64, want: bool) -> bool {
    for _ in 0..10 {
        if open(port) == want {
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

/// `_port_bind_probe`: can sing-box bind 127.0.0.1:N right now? A run of the
/// engine with that one inbound, under the shell's own perl wrapper — which
/// ends the engine after 5 s whatever becomes of this process, so a rowt killed
/// mid-probe leaves nothing on the port. No engine on disk: nothing to ask.
fn bind_probe(ctx: &Ctx, port: u64) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let sb = ctx.sb();
    let exec = std::fs::metadata(&sb).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false);
    if !exec {
        return true;
    }
    let tmpdir = std::env::var("TMPDIR").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "/tmp".into());
    let cfg = PathBuf::from(format!("{}/rowt-port-probe.{}{}", tmpdir.trim_end_matches('/'), std::process::id(), port));
    let body = format!("{{\"log\":{{\"level\":\"error\"}},\"inbounds\":[{{\"type\":\"mixed\",\"tag\":\"probe\",\"listen\":\"127.0.0.1\",\"listen_port\":{port}}}],\"outbounds\":[{{\"type\":\"direct\",\"tag\":\"direct\"}}]}}\n");
    if std::fs::write(&cfg, body).is_err() {
        return true;
    }
    const WRAP: &str = r#"$p = fork(); if (!$p) { exec @ARGV or exit 127 } $SIG{ALRM} = $SIG{TERM} = $SIG{INT} = sub { kill "TERM", $p; exit 0 }; alarm 5; waitpid($p, 0); exit 0"#;
    let child = Command::new("perl").arg("-e").arg(WRAP).arg(&sb).args(["run", "-c"]).arg(&cfg)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    let mut ok = false;
    if let Ok(mut c) = child {
        for _ in 0..20 {
            if open(port) {
                ok = true;
                break;
            }
            // The engine exited: the bind failed.
            if !matches!(c.try_wait(), Ok(None)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        unsafe { libc::kill(c.id() as i32, libc::SIGTERM) };
        let _ = c.wait();
    }
    let _ = std::fs::remove_file(&cfg);
    ok
}

/// `… >/dev/null`: stdout only, for the length of `f` — stderr's `==>` lines
/// still show, as the shell's do. (`crate::redirected` takes both.)
fn quiet<T>(f: impl FnOnce() -> T) -> T {
    use std::io::Write;
    use std::os::unix::io::AsRawFd;
    let _ = std::io::stdout().flush();
    let null = std::fs::OpenOptions::new().write(true).open("/dev/null").ok();
    let saved = null.as_ref().and_then(|n| unsafe {
        let o = libc::dup(1);
        (o >= 0).then(|| {
            libc::dup2(n.as_raw_fd(), 1);
            o
        })
    });
    let out = f();
    if let Some(o) = saved {
        let _ = std::io::stdout().flush();
        unsafe {
            libc::dup2(o, 1);
            libc::close(o);
        }
    }
    out
}

/// `( cmd_render >/dev/null )`: the render's own `die`s happen inside the
/// subshell — an error and an ABORT line — and the caller carries on.
fn render_sub(ctx: &Ctx) -> bool {
    quiet(|| render_sub_inner(ctx))
}

fn render_sub_inner(ctx: &Ctx) -> bool {
    let tol = std::env::var("ROWT_AUTO_TOLERANCE").ok();
    if rowt_core::render::auto_tolerance(tol.as_deref()).is_none() {
        crate::die_in_subshell(&ctx.cfg, &format!(
            "ROWT_AUTO_TOLERANCE wants whole milliseconds from 0 to 99999 (got '{}')",
            tol.unwrap_or_default()
        ));
        return false;
    }
    match lifecycle::cmd_render(ctx) {
        Ok(_) => true,
        Err(e) => {
            crate::die_in_subshell(&ctx.cfg, &e);
            false
        }
    }
}

/// `cmd_router restart >/dev/null` (and `cmd_router up`): a start that did not
/// come back is a status; anything the shell would `die` on ends the command.
fn router_ok(ctx: &Ctx, r: Result<String, String>) -> bool {
    match r {
        Ok(_) => true,
        Err(e) if e == crate::SILENT => false,
        Err(e) => die(&ctx.cfg, &e),
    }
}

/// `_router_ready 2` for a router this command did not start: alive at each
/// second, and its clash API answered at least once.
fn ready(ctx: &Ctx, tries: u32) -> bool {
    let mut seen = false;
    for _ in 0..tries {
        std::thread::sleep(Duration::from_secs(1));
        if lifecycle::host_running(ctx).is_none() {
            return false;
        }
        if !ctx.controller().is_empty() && lifecycle::clash_ok(ctx) {
            seen = true;
        }
    }
    seen
}

/// `_port_apply`: re-render, then bring the RUNNING router onto the new
/// host.json — sing-box's own SIGHUP reload, in the same process, then a full
/// restart if that did not take, then the change undone if even that cannot
/// start. `had` is the table before this command (a failed render goes back to
/// it); `without` is it without the name at stake (a failed start goes to it).
fn apply(ctx: &Ctx, had: &[String], without: &[String], want_open: Option<u64>, want_closed: Option<u64>, msg: &str) -> Result<String, String> {
    let pi_before = ctx.sget("proxy_intent");
    let has_servers = std::fs::metadata(ctx.cfg.join("servers.json")).map(|m| m.is_file() && m.len() > 0).unwrap_or(false);
    if ctx.sget("mode") != "local" && !has_servers {
        info(&format!("{msg} (saved; applies on the next '{PROG} up')"));
        return Ok(String::new());
    }
    let before = lifecycle::host_cfg_cksum(ctx);
    if !render_sub(ctx) {
        write(ctx, had.to_vec());
        die(&ctx.cfg, "render failed — the ports are as they were");
    }
    if lifecycle::host_running(ctx).is_none() {
        info(&format!("{msg} (router not running: applies on the next '{PROG} router up')"));
        return Ok(String::new());
    }
    let settled = |p: Option<u64>, want: bool| p.is_none_or(|p| open(p) == want);
    if lifecycle::host_cfg_cksum(ctx) == before && settled(want_open, true) && settled(want_closed, false) {
        info(msg);
        return Ok(String::new());
    }
    let pid = lifecycle::read(&ctx.pidfile()).trim_end_matches('\n').to_string();
    let hup = pid.parse::<i32>().map(|p| unsafe { libc::kill(p, libc::SIGHUP) } == 0).unwrap_or(false);
    if hup
        && want_open.is_none_or(|p| wait(p, true))
        && want_closed.is_none_or(|p| wait(p, false))
        && lifecycle::host_running(ctx).is_some()
        && ready(ctx, 2)
    {
        // `_router_cfg_mark`: the reloaded instance runs this host.json.
        let sum = lifecycle::host_cfg_cksum(ctx);
        if !sum.is_empty() {
            let _ = std::fs::write(lifecycle::router_cfg_sum(ctx), format!("{sum}\n"));
        }
        info(&format!("{msg} (router reloaded in place, pid {pid} — open connections were reset)"));
        return Ok(String::new());
    }
    warn("in-place reload did not take — restarting the router");
    // `cmd_router restart >/dev/null`.
    let up = quiet(|| {
        lifecycle::router_stop(ctx);
        lifecycle::router_up(ctx)
    });
    if router_ok(ctx, up) && want_open.is_none_or(|p| wait(p, true)) {
        info(&format!("{msg} (router restarted)"));
        return Ok(String::new());
    }
    // The change itself keeps the router from coming up. Take it back, render
    // without it, bring the router up as it was, put the proxy back, and only
    // then say so: a row that is not served must not stay in the table.
    write(ctx, without.to_vec());
    let _ = render_sub(ctx);
    let back = lifecycle::host_running(ctx).is_some() || {
        let up = quiet(|| lifecycle::router_up(ctx));
        router_ok(ctx, up)
    };
    if back {
        // `( cmd_proxy on >/dev/null 2>&1 )`. Its failure (a password it could
        // not ask for) is the shell's one extra warning line, on stderr only.
        if pi_before == "on" && ctx.sget("proxy_intent") != "on" {
            let _ = crate::redirected(std::path::Path::new("/dev/null"), || lifecycle::cmd_proxy_on(ctx, false));
        }
        let hint = want_open.map(|p| format!(" (is something rowt cannot see listening on 127.0.0.1:{p}?)")).unwrap_or_default();
        die(&ctx.cfg, &format!(
            "the router would not come up with that change — see '{PROG} router log' — so it was undone and the router is running as before{hint}"
        ));
    }
    die(&ctx.cfg, &format!(
        "the router would not come up with that change, nor after undoing it — see '{PROG} status' and '{PROG} router log'"
    ))
}
