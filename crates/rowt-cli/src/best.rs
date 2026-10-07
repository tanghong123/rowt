//! `best` — rowt's own auto mode, and the latency table it shares with
//! `rowt ping` and the monitor. The shell's `_latency_probe`,
//! `_latency_table_update`, `_escape_sel`, `_use_best` and `_watch_best`.
//!
//! The rule and the table format live in `rowt_core::latency`; this is the I/O
//! around them. `auto` stays sing-box's urltest, which can only switch on single
//! samples (no API stores a median); in `best` the watchdog keeps the current
//! server unless another's MEDIAN beats it by more than the tolerance.

use crate::lifecycle::{self, Ctx};
use crate::{env_or, read, PROG};
use rowt_core::latency::{self, Row};
use serde_json::Value;
use std::path::PathBuf;

pub fn table_path(ctx: &Ctx) -> PathBuf {
    ctx.cfg.join("latency.tsv")
}

fn pool(ctx: &Ctx) -> Vec<String> {
    let v: Vec<Value> = serde_json::from_str(&read(&ctx.cfg.join("servers.json"))).unwrap_or_default();
    v.iter().filter_map(|s| s.get("tag").and_then(|t| t.as_str()).map(|t| t.to_string())).collect()
}

fn tolerance() -> u64 {
    env_or("ROWT_BEST_TOLERANCE", "50").parse().unwrap_or(50)
}

fn best_timeout() -> u32 {
    env_or("ROWT_BEST_TIMEOUT", "5").parse().unwrap_or(5)
}

/// `date -u '+%Y-%m-%d %H:%M:%S'`, or with `-v-NS` for a cutoff N seconds back
/// — the same program the shell runs, so both stamps agree to the format.
fn utc(back_secs: Option<u64>) -> String {
    let mut c = std::process::Command::new("date");
    c.arg("-u");
    if let Some(n) = back_secs {
        c.arg(format!("-v-{n}S"));
    }
    c.arg("+%Y-%m-%d %H:%M:%S").stderr(std::process::Stdio::null()).output().ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// `_latency_probe`: three samples per server, one after another, at most ten
/// servers at a time. (tag, figure, samples answered), in input order.
pub fn probe(ctx: &Ctx, timeout: u32, tags: &[String]) -> Vec<(String, Option<u64>, u64)> {
    let Some(ep) = lifecycle::controller(ctx) else { return Vec::new() };
    let secret = lifecycle::clash_secret(ctx);
    let enc = crate::urlencode(&env_or("ROWT_PING_URL", "https://www.gstatic.com/generate_204"));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let out: std::sync::Mutex<Vec<Option<(String, Option<u64>, u64)>>> =
        std::sync::Mutex::new(vec![None; tags.len()]);
    std::thread::scope(|scope| {
        for _ in 0..tags.len().min(10) {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(t) = tags.get(i) else { break };
                let ok: Vec<u64> = (0..latency::SAMPLES).filter_map(|_| {
                    // curl max-time must exceed the clash delay timeout, else it cuts early
                    let o = std::process::Command::new("curl")
                        .args(["--noproxy", "*", "-sS", "-m", &(timeout + 3).to_string(),
                               "-H", &format!("Authorization: Bearer {secret}"),
                               &format!("http://{ep}/proxies/{t}/delay?timeout={timeout}000&url={enc}")])
                        .stderr(std::process::Stdio::null()).output().ok();
                    o.and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
                        .and_then(|v| v.get("delay").and_then(|d| d.as_u64()))
                }).collect();
                let row = (t.clone(), latency::aggregate(&ok), ok.len() as u64);
                if let Ok(mut g) = out.lock() {
                    g[i] = Some(row);
                }
            });
        }
    });
    out.into_inner().unwrap_or_default().into_iter().flatten().collect()
}

/// `_latency_table_update`: fold probe results into the table — those rows
/// replaced and stamped now, the rest kept, servers gone from the pool dropped.
pub fn table_update(ctx: &Ctx, fresh: &[(String, Option<u64>, u64)]) {
    let now = utc(None);
    let rows: Vec<Row> = fresh.iter()
        .map(|(t, m, n)| Row { tag: t.clone(), median: *m, ok: *n, at: now.clone() })
        .collect();
    let old = latency::table_parse(&read(&table_path(ctx)));
    let body = latency::table_render(&latency::table_merge(&old, &rows, &pool(ctx)));
    let tmp = ctx.cfg.join(format!(".latency.tsv.{}", std::process::id()));
    if std::fs::write(&tmp, body).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    if std::fs::rename(&tmp, table_path(ctx)).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

fn rows(ctx: &Ctx) -> Vec<Row> {
    latency::table_parse(&read(&table_path(ctx)))
}

/// `_latency_ms`: "81 ms" / "no answer" / "unmeasured".
pub fn latency_ms(ctx: &Ctx, tag: &str) -> String {
    match rows(ctx).into_iter().find(|r| r.tag == tag) {
        Some(Row { median: Some(m), .. }) => format!("{m} ms"),
        Some(_) => "no answer".into(),
        None => "unmeasured".into(),
    }
}

/// `_escape_sel`: `selected`, except in `best` mode, where it is the last pick
/// ("" before the first — the render then defaults to the first server).
pub fn escape_sel(ctx: &Ctx) -> String {
    latency::escape_selection(&ctx.sget("selected"), &ctx.sget("best_pick"))
}

fn choose(ctx: &Ctx, current: &str) -> Option<String> {
    if !table_path(ctx).is_file() {
        return None;
    }
    latency::choose(current, &pool(ctx), &rows(ctx), tolerance())
}

/// `_best_watch_note`.
fn watch_note() {
    let loaded = std::process::Command::new("launchctl").args(["list", crate::watch::LABEL])
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .status().map(|s| s.success()).unwrap_or(false);
    if !loaded {
        eprintln!("warning: the watchdog is not running, so this pick will not be re-checked — '{PROG} watch install'");
    }
}

/// `_use_best`.
pub fn use_best(ctx: &Ctx, prev: &str) -> Result<String, String> {
    lifecycle::sset(ctx, "selected", "best");
    if lifecycle::host_running(ctx).is_none() {
        lifecycle::cmd_render(ctx)?;
        eprintln!("==> escape -> best (applies on next '{PROG} router up'; the watchdog picks the fastest server by median latency)");
        watch_note();
        return Ok(String::new());
    }
    eprintln!("==> measuring every server (median of 3 samples, at most 10 at a time)…");
    let fresh = probe(ctx, best_timeout(), &pool(ctx));
    table_update(ctx, &fresh);
    let now = lifecycle::clash_selected(ctx).unwrap_or_default();
    let mut pick = choose(ctx, &now).unwrap_or_default();
    if pick.is_empty() { pick = ctx.sget("best_pick"); }
    if pick.is_empty() { pick = pool(ctx).first().cloned().unwrap_or_default(); }
    let live = prev != "auto"
        && lifecycle::clash_curl(ctx, "PUT", "/proxies/escape", Some(&format!("{{\"name\":\"{pick}\"}}"))).is_some();
    lifecycle::sset(ctx, "best_pick", &pick);
    lifecycle::cmd_render(ctx)?;
    if live {
        eprintln!("==> escape -> best: {pick} ({}, live)", latency_ms(ctx, &pick));
    } else {
        lifecycle::router_stop(ctx);
        lifecycle::router_up(ctx)?;
        eprintln!("==> escape -> best: {pick} ({})", latency_ms(ctx, &pick));
    }
    watch_note();
    Ok(String::new())
}

/// `_watch_best`: once per tick, when the network has not just moved.
pub fn watch_best(ctx: &Ctx, ifc: &str, health_fails: &str) {
    if ctx.sget("selected") != "best" || ctx.mode() == "local" {
        return;
    }
    let ip = std::process::Command::new("ipconfig").args(["getifaddr", ifc])
        .stderr(std::process::Stdio::null()).output().ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    if ip.is_empty() {
        return;
    }
    let stale = if health_fails.trim() == "1" {
        pool(ctx)
    } else {
        let secs: u64 = env_or("ROWT_BEST_INTERVAL", "600").parse().unwrap_or(600);
        latency::stale(&pool(ctx), &rows(ctx), &utc(Some(secs)))
    };
    if !stale.is_empty() {
        let fresh = probe(ctx, best_timeout(), &stale);
        table_update(ctx, &fresh);
    }
    let now = lifecycle::clash_selected(ctx).unwrap_or_default();
    let Some(pick) = choose(ctx, &now) else { return };
    if pick == now {
        if ctx.sget("best_pick") != pick {
            lifecycle::sset(ctx, "best_pick", &pick);
        }
        return;
    }
    if lifecycle::clash_curl(ctx, "PUT", "/proxies/escape", Some(&format!("{{\"name\":\"{pick}\"}}"))).is_none() {
        crate::watch::watch_log(ctx, &format!("best: could not switch to {pick}"));
        return;
    }
    lifecycle::sset(ctx, "best_pick", &pick);
    let from = if now.is_empty() { "?".to_string() } else { now.clone() };
    let m = format!("best: switched {from} ({}) -> {pick} ({})", latency_ms(ctx, &now), latency_ms(ctx, &pick));
    crate::watch::watch_log(ctx, &m);
    crate::shell::audit(&ctx.cfg, &format!("watchdog {m}"));
}
