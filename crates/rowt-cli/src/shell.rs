//! `main()`'s preamble and postamble — everything bin/rowt does around the
//! command table rather than inside it.
//!
//! It is easy to port `run_command` and believe the CLI is done. But bash runs
//! `migrate`, rotates three logs, routes help, and brackets every mutating
//! command with audit BEGIN/END before and after the dispatch. A rowt-rs that
//! skipped this would print the right answers while silently keeping no record
//! of what it changed — and `cli-diff` only sees it because the gate diffs the
//! audit log too.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn env_num(k: &str, d: u64) -> u64 {
    std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
}

// ---------------------------------------------------------------- audit

/// `_audit_ctx` — pid/parent/tty/version, the field that distinguishes a
/// hands-on `rowt down` from the watchdog's.
fn audit_ctx() -> String {
    let pid = std::process::id();
    let ppid = unsafe { libc::getppid() };
    let comm = Command::new("ps").args(["-o", "comm=", "-p", &ppid.to_string()])
        .stderr(Stdio::null()).output().ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let comm = comm.rsplit('/').next().unwrap_or("").replace(' ', "");
    let comm = if comm.is_empty() { "?".to_string() } else { comm };
    let tty = Command::new("ps").args(["-o", "tty=", "-p", &pid.to_string()])
        .stderr(Stdio::null()).output().ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).replace(' ', ""))
        .unwrap_or_default();
    let tty = match tty.trim() {
        "" | "?" | "??" | "-" => "no-tty".to_string(),
        t => t.to_string(),
    };
    format!("pid={pid} ppid={ppid} by={comm}({tty}) v{}", env!("ROWT_SHELL_VERSION"))
}

/// `date '+%Y-%m-%d %H:%M:%S %z'` — via date(1) rather than a time crate, so the
/// locale and zone handling are literally the same program the shell calls.
fn stamp() -> String {
    Command::new("date").arg("+%Y-%m-%d %H:%M:%S %z")
        .stderr(Stdio::null()).output().ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim_end().to_string())
        .unwrap_or_default()
}

pub fn audit(cfg: &Path, msg: &str) {
    let log = cfg.join("log/audit.log");
    let line = format!("{}  {}  {msg}\n", stamp(), audit_ctx());
    let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&log) else { return };
    use std::io::Write;
    let _ = f.write_all(line.as_bytes());
    // Bounded without copytruncate: the audit log is meant to be kept, so it is
    // trimmed to the cap only once it drifts well past it.
    let max = env_num("ROWT_AUDIT_MAX", 5000);
    if max == 0 {
        return;
    }
    let body = fs::read_to_string(&log).unwrap_or_default();
    let lines: Vec<&str> = body.lines().collect();
    if lines.len() as u64 > max + 500 {
        let keep = lines[lines.len() - max as usize..].join("\n");
        let _ = fs::write(&log, keep + "\n");
    }
}

/// `_is_readonly` — false is the safe default (record it), because missing a
/// mutation is worse than an extra line.
pub fn is_readonly(cmd: &str, arg: &str) -> bool {
    let sub = |d: &'static str| if arg.is_empty() { d } else { arg };
    match cmd {
        "status" | "report" | "explain" | "route" | "connections" | "conns" | "monitor" | "mon"
        | "ping" | "probe" | "version" | "run" | "onboard" | "shell-init" | "completion"
        | "_complete" | "help" | "audit" | "metrics" | "-h" | "--help" | "--version" | "-V" => true,
        "proxy" => !matches!(sub("status"), "on" | "off"),
        "router" => !matches!(sub("status"), "up" | "down" | "restart"),
        "vm" => !matches!(sub("status"), "up" | "down" | "restart" | "delete"),
        "watch" => !matches!(sub("status"), "install" | "uninstall" | "refresh"),
        "config" => sub("list") != "import",
        "escape" | "corp" | "block" | "direct" => {
            matches!(arg, "" | "errors" | "stats" | "list" | "log")
        }
        "server" => matches!(arg, "" | "list" | "ls" | "show"),
        "sub" => matches!(arg, "" | "list" | "ls" | "show" | "count"),
        _ => false,
    }
}

// ---------------------------------------------------------------- logs

/// Copytruncate rotation, run on every invocation so an oversize log is trimmed
/// the next time rowt runs at all. Append-mode writers (sing-box) survive it.
pub fn rotate_log(f: &Path) {
    let max = env_num("ROWT_LOG_MAX_BYTES", 5_242_880);
    let keep = env_num("ROWT_LOG_KEEP", 9);
    let Ok(md) = fs::metadata(f) else { return };
    if !md.is_file() || md.len() < max {
        return;
    }
    let gen = |i: u64| f.with_file_name(format!("{}.{i}", f.file_name().unwrap().to_string_lossy()));
    let mut i = keep;
    while i > 1 {
        let _ = fs::rename(gen(i - 1), gen(i));
        i -= 1;
    }
    if keep >= 1 {
        let _ = fs::copy(f, gen(1));
    }
    let _ = fs::write(f, "");
}

// ---------------------------------------------------------------- migrate

/// `seed_lists` + the two historical file moves. Idempotent by construction:
/// each step is guarded on the absence of what it would create.
pub fn migrate(cfg: &Path) {
    seed_lists(cfg);
    let servers = cfg.join("servers.json");
    let outbound = cfg.join("outbound.json");
    let manual = cfg.join("manual.json");
    let subs = cfg.join("subs.txt");
    if !servers.is_file() && outbound.is_file() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&fs::read_to_string(&outbound).unwrap_or_default()) {
            let mut o = v.clone();
            if let Some(m) = o.as_object_mut() {
                m.insert("tag".into(), serde_json::Value::String("server-1".into()));
            }
            if fs::write(&servers, serde_json::to_string_pretty(&serde_json::json!([o])).unwrap()).is_ok() {
                let _ = set_mode(&servers, 0o600);
                crate::lifecycle::sset(&crate::lifecycle::Ctx::new(cfg.to_path_buf()), "selected", "auto");
            }
        }
    }
    if fs::metadata(&servers).map(|m| m.len() > 0).unwrap_or(false)
        && !manual.is_file() && !subs.is_file()
    {
        if fs::copy(&servers, &manual).is_ok() {
            let _ = set_mode(&manual, 0o600);
        }
    }
}

fn set_mode(p: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(mode))
}

/// `seed_lists` — create the four lane files with their headers if missing.
/// The headers are the shell's, extracted at build time for the same reason the
/// help text is: they are text, and two copies of text diverge.
fn seed_lists(cfg: &Path) {
    let _ = fs::create_dir_all(cfg.join("log"));
    let _ = fs::create_dir_all(cfg.join("cache"));
    for (name, hdr) in crate::seeds::SEEDS {
        let p = cfg.join(name);
        if !p.exists() {
            let _ = fs::write(&p, hdr);
        }
    }
}

// ---------------------------------------------------------------- the bracket

/// Run `body` the way bash's `main` does: help routed first, then either
/// straight through (read-only) or bracketed in the audit log.
pub fn dispatch<F>(cfg: &PathBuf, args: &[String], body: F) -> Result<String, String>
where
    F: FnOnce(&str, &[String]) -> Result<String, String>,
{
    migrate(cfg);
    let logdir = cfg.join("log");
    rotate_log(&logdir.join("host.log"));
    rotate_log(&logdir.join("watch.log"));
    for l in ["escape", "corp", "block", "direct"] {
        rotate_log(&logdir.join(format!("lane-{l}.log")));
    }

    // main() sends the no-argument case to the shell before reaching here.
    let Some(cmd) = args.first().map(|s| s.as_str()) else {
        return Ok(String::new());
    };
    let rest: Vec<String> = args[1..].to_vec();

    match cmd {
        "help" => return crate::help::show(cfg, rest.first().map(|s| s.as_str()).unwrap_or("")),
        "-h" | "--help" => return Ok(crate::help::usage(cfg)),
        "--version" | "-V" => return Ok(format!("{} {}", crate::PROG, env!("ROWT_SHELL_VERSION"))),
        _ => {}
    }
    // exec-wrappers pass every argument to the wrapped program, so a `--help`
    // meant for THAT must not trigger rowt's own help.
    if !matches!(cmd, "run" | "monitor" | "mon")
        && rest.iter().any(|a| a == "--help" || a == "-h")
    {
        return crate::help::show(cfg, cmd);
    }

    let first = rest.first().cloned().unwrap_or_default();
    if is_readonly(cmd, &first) {
        return body(cmd, &rest);
    }
    let op = if rest.is_empty() { cmd.to_string() } else { format!("{cmd} {}", rest.join(" ")) };
    let t0 = std::time::Instant::now();
    audit(cfg, &format!("BEGIN {op}"));
    crate::set_audit_op(&op);
    let r = body(cmd, &rest);
    crate::set_audit_op("");
    let rc = if r.is_ok() { 0 } else { 1 };
    audit(cfg, &format!("END   {op} rc={rc} ({}s)", t0.elapsed().as_secs()));
    r
}
