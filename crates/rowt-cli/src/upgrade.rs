//! rowt #s104: after an upgrade, the router keeps running the config the old
//! rowt rendered until something reloads it. Hong chose on #s102: "Watchdog
//! reloads at a quiet moment, and status flags it until then".
//!
//! Four records, beside `host.running` (the cksum of the host.json the router
//! started from, `lifecycle::router_cfg_sum`):
//!
//! - `host.json.by`: the rowt version that rendered `host.json`, written by
//!   every render that puts one in place. Missing on a host.json an older rowt
//!   rendered.
//! - `host.running.by`: the rowt version whose render the router runs: a copy
//!   of `host.json.by` wherever `host.running` is written, and removed with it.
//!   A restart starts the host.json on disk without rendering, so the stamp
//!   comes from the render, not from the rowt that starts the router.
//! - `cache/upgrade-pending`: the watchdog's verdict that this rowt renders a
//!   different config than the running one, with the traffic sample its quiet
//!   test needs: `version, from, epoch, total, quiet_since`, tab-separated.
//! - `cache/upgrade-reloaded`: the version the watchdog reloaded for. Written
//!   BEFORE the reload, so a reload that fails is not tried again: once per
//!   version, whatever happens.
//!
//! The shell's `_watch_upgrade` and `_router_by_*` do the same, step for step:
//! the same files, the same `sysctl` and clash API calls, in the same order.

use crate::env_or;
use crate::lifecycle::{self, read, Ctx};
use rowt_core::watch::{upgrade_quiet, woke_quiet, Sample, Upgrade, UPGRADE_QUIET_BPS};
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// The installed rowt: the shell's `ROWT_VERSION`, read at build time.
pub const VERSION: &str = env!("ROWT_SHELL_VERSION");

pub fn by_path(ctx: &Ctx) -> PathBuf { ctx.cfg.join("host.running.by") }
fn rendered_path(ctx: &Ctx) -> PathBuf { ctx.cfg.join("host.json.by") }
fn pending_path(ctx: &Ctx) -> PathBuf { ctx.cfg.join("cache").join("upgrade-pending") }
fn reloaded_path(ctx: &Ctx) -> PathBuf { ctx.cfg.join("cache").join("upgrade-reloaded") }

/// A render put host.json in place (`_render_by_stamp`).
pub fn stamp_render(ctx: &Ctx) {
    let _ = std::fs::write(rendered_path(ctx), format!("{VERSION}\n"));
}

/// The router now runs host.json (`_router_by_mark`), beside every write of
/// `host.running`: it runs whichever rowt's render that is. When that is this
/// rowt, nothing waits any more.
pub fn mark(ctx: &Ctx) {
    let r = read(&rendered_path(ctx)).trim().to_string();
    if r.is_empty() {
        let _ = std::fs::remove_file(by_path(ctx));
    } else {
        let _ = std::fs::write(by_path(ctx), format!("{r}\n"));
    }
    if r == VERSION {
        let _ = std::fs::remove_file(pending_path(ctx));
    }
}

/// The dry render matched the running config (`_router_by_adopt`): this
/// rowt's render is what runs, whoever rendered it. host.json too, when it is
/// what runs: else the next restart would copy its older stamp back.
fn adopt(ctx: &Ctx) {
    let _ = std::fs::write(by_path(ctx), format!("{VERSION}\n"));
    let _ = std::fs::remove_file(pending_path(ctx));
    let run = read(&lifecycle::router_cfg_sum(ctx)).trim().to_string();
    if let Ok(b) = std::fs::read(ctx.host_cfg()) {
        if format!("{} {}", rowt_core::cksum::posix(&b), b.len()) == run {
            stamp_render(ctx);
        }
    }
}

/// Beside every removal of `host.running`.
pub fn clear(ctx: &Ctx) {
    let _ = std::fs::remove_file(by_path(ctx));
}

/// What `status` reports: the version that rendered the running config (empty
/// when not recorded), and whether this rowt's different render waits for a
/// reload: `waiting` (the watchdog found it and waits for a quiet moment),
/// `failed` (its one reload for this version did not take), or `` (nothing
/// waits, or nobody looked). Either settles once the router runs this
/// version's render, so a reload by hand ends it at once.
pub fn report(ctx: &Ctx) -> (String, &'static str) {
    let by = read(&by_path(ctx)).trim().to_string();
    let state = if by == VERSION {
        ""
    } else if pending_version(ctx).as_deref() == Some(VERSION) {
        "waiting"
    } else if read(&reloaded_path(ctx)).trim() == VERSION {
        "failed"
    } else {
        ""
    };
    (by, state)
}

/// The `status` line, only while the router runs and a reload waits: nothing
/// on a box where the watchdog found nothing (or never looked).
pub fn status_line(ctx: &Ctx) -> Option<String> {
    let (by, state) = report(ctx);
    if state.is_empty() || lifecycle::host_running(ctx).is_none() {
        return None;
    }
    let from = if by.is_empty() { "an older rowt".to_string() } else { format!("rowt {by}") };
    let what = if state == "failed" {
        "the watchdog's one reload for it failed (see watch.log)"
    } else {
        "the watchdog reloads once at a quiet moment"
    };
    Some(format!(
        "upgrade:      rowt {VERSION} renders a different config than the running one ({from} rendered it) — \
         {what}; '{} reload' applies it now", crate::PROG))
}

fn pending_version(ctx: &Ctx) -> Option<String> {
    parse_pending(&read(&pending_path(ctx))).map(|p| p.0)
}

/// `cache/upgrade-pending`'s one line: the version, the rowt that rendered
/// the running config, and the traffic sample (None when its three fields are
/// not all numbers). None when the file names no version.
fn parse_pending(text: &str) -> Option<(String, String, Option<Sample>)> {
    let f: Vec<&str> = text.lines().next().unwrap_or("").split('\t').collect();
    let v = f.first().filter(|v| !v.is_empty())?.to_string();
    // Only what the shell's `^-?[0-9]+$` takes: no `+`, no blanks.
    let n = |i: usize| f.get(i).filter(|x| {
        let d = x.strip_prefix('-').unwrap_or(x);
        !d.is_empty() && d.bytes().all(|c| c.is_ascii_digit())
    }).and_then(|x| x.parse::<i64>().ok());
    let sample = match (n(2), n(3), n(4)) {
        (Some(epoch), Some(total), Some(quiet_since)) if total >= 0 => Some(Sample { epoch, total: total as u64, quiet_since }),
        _ => None,
    };
    Some((v, f.get(1).unwrap_or(&"").to_string(), sample))
}

fn format_pending(v: &str, from: &str, s: Option<Sample>) -> String {
    let sample = s.map(|s| format!("{}\t{}\t{}", s.epoch, s.total, s.quiet_since)).unwrap_or_else(|| "\t\t".into());
    format!("{v}\t{from}\t{sample}\n")
}

/// The watchdog's look, on a tick where the router runs, nothing reloads for
/// the network's sake and the tunnel answers. None when nothing waits: the
/// router runs this version's render, or it could not be told (no
/// `host.running`), or this version was reloaded for once already.
///
/// The first tick after an upgrade renders without writing anything (no lane
/// sets, no DoH, no warnings) and compares the bytes with `host.running`. The
/// same: recorded as this version's, and nothing more happens. Different:
/// pending, and from then on each tick asks only whether now is quiet.
pub fn observe(ctx: &Ctx, now: i64) -> Option<Upgrade> {
    let by = read(&by_path(ctx)).trim().to_string();
    if by == VERSION {
        return None;
    }
    let run = read(&lifecycle::router_cfg_sum(ctx)).trim().to_string();
    if run.is_empty() || read(&reloaded_path(ctx)).trim() == VERSION {
        return None;
    }
    let (fresh, prev) = match parse_pending(&read(&pending_path(ctx))) {
        Some((v, _, prev)) if v == VERSION => (false, prev),
        _ => {
            let host = lifecycle::build_host_with(ctx, false).ok()?;
            let body = serde_json::to_string_pretty(&host).ok()? + "\n";
            let sum = format!("{} {}", rowt_core::cksum::posix(body.as_bytes()), body.len());
            if sum == run {
                adopt(ctx);
                return None;
            }
            (true, None)
        }
    };
    let (woke, sleep) = wake_sleep();
    // The clash API only when the wake has not already answered.
    let total = if woke_quiet(now, woke, sleep) { None } else { crate::watch::router_bytes_total(ctx) };
    // Digits only, at most nine, as the shell's `case` takes it; anything else
    // is the default. Nine keeps bps × gap inside the shell's 64-bit integers.
    let bps = Some(env_or("ROWT_RELOAD_QUIET_BPS", "")).filter(|b| (1..=9).contains(&b.len()) && b.bytes().all(|c| c.is_ascii_digit()))
        .and_then(|b| b.parse::<u64>().ok()).unwrap_or(UPGRADE_QUIET_BPS);
    let (quiet, next) = upgrade_quiet(now, woke, sleep, prev, total, bps);
    let _ = std::fs::create_dir_all(ctx.cfg.join("cache"));
    let _ = std::fs::write(pending_path(ctx), format_pending(VERSION, &by, next));
    Some(Upgrade { version: VERSION.to_string(), from: by, quiet: quiet.to_string(), fresh })
}

/// `Action::MarkUpgrade`: never again for this version.
pub fn mark_reloaded(ctx: &Ctx, v: &str) {
    let _ = std::fs::create_dir_all(ctx.cfg.join("cache"));
    let _ = std::fs::write(reloaded_path(ctx), format!("{v}\n"));
    let _ = std::fs::remove_file(pending_path(ctx));
}

/// `sysctl -n kern.waketime kern.sleeptime`: the seconds of each, 0 when
/// unknown. One line each, `{ sec = N, usec = M } <date>`.
fn wake_sleep() -> (i64, i64) {
    let o = Command::new("sysctl").args(["-n", "kern.waketime", "kern.sleeptime"])
        .stderr(Stdio::null()).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    let mut it = o.lines().map(|l| rowt_platform::first_digits(l).and_then(|d| d.parse::<i64>().ok()).unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0))
}


#[cfg(test)]
mod tests {
    use super::*;

    /// The file the shell writes with `printf '%s\t%s\t%s\t%s\t%s\n'` and
    /// reads with `cut -f`: empty fields stay fields, so a missing `from` or
    /// sample does not shift the rest.
    #[test]
    fn the_pending_record_round_trips_with_its_empty_fields() {
        let s = Some(Sample { epoch: 1_700_000_000, total: 1100, quiet_since: 1_699_999_100 });
        assert_eq!(format_pending("3.11.0", "3.10.1", s), "3.11.0\t3.10.1\t1700000000\t1100\t1699999100\n");
        assert_eq!(parse_pending(&format_pending("3.11.0", "3.10.1", s)), Some(("3.11.0".into(), "3.10.1".into(), s)));
        assert_eq!(format_pending("3.11.0", "", None), "3.11.0\t\t\t\t\n");
        assert_eq!(parse_pending("3.11.0\t\t\t\t\n"), Some(("3.11.0".into(), String::new(), None)));
        assert_eq!(parse_pending("3.11.0\t3.10.1\t12\tx\t0\n"), Some(("3.11.0".into(), "3.10.1".into(), None)));
        assert_eq!(parse_pending("3.11.0\t3.10.1\t12\t-1\t0\n").unwrap().2, None, "a negative counter is no sample");
        assert_eq!(parse_pending("3.11.0\t3.10.1\t+12\t1\t0\n").unwrap().2, None, "the shell's regex takes no sign but -");
        assert_eq!(parse_pending("3.11.0\t3.10.1\t12\t1\t-5\n").unwrap().2, Some(Sample { epoch: 12, total: 1, quiet_since: -5 }));
        assert_eq!(parse_pending(""), None);
        assert_eq!(parse_pending("\t3.10.1\t\t\t\n"), None);
    }

    /// A scratch config dir, removed when dropped.
    struct Scratch(Ctx);
    impl Drop for Scratch {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0.cfg); }
    }
    fn scratch(name: &str) -> Scratch {
        let cfg = std::env::temp_dir().join(format!("rowt-s104-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cfg);
        std::fs::create_dir_all(cfg.join("cache")).unwrap();
        Scratch(Ctx { cfg, port: 7890, clash_port: 9090 })
    }
    fn put(ctx: &Ctx, rel: &str, body: &str) { std::fs::write(ctx.cfg.join(rel), body).unwrap(); }
    fn get(ctx: &Ctx, rel: &str) -> Option<String> { std::fs::read_to_string(ctx.cfg.join(rel)).ok() }

    /// rowt #s104 review: `rowt restart` right after an upgrade starts the old
    /// render. It must not record this version, nor clear the waiting reload.
    #[test]
    fn a_restart_of_an_older_render_keeps_the_upgrade_waiting() {
        let s = scratch("restart");
        let c = &s.0;
        put(c, "cache/upgrade-pending", &format_pending(VERSION, "3.10.1", None));
        // A host.json from before the stamp: the router no longer names a version.
        put(c, "host.running.by", "3.10.1\n");
        mark(c);
        assert_eq!(get(c, "host.running.by"), None);
        assert_eq!(report(c), (String::new(), "waiting"));
        // A host.json an older 3.11-era rowt stamped: its version, still waiting.
        put(c, "host.json.by", "3.10.9\n");
        mark(c);
        assert_eq!(get(c, "host.running.by").as_deref(), Some("3.10.9\n"));
        assert_eq!(report(c), ("3.10.9".into(), "waiting"));
        // This version's render, started: settled.
        stamp_render(c);
        mark(c);
        assert_eq!(get(c, "host.running.by").as_deref(), Some(&*format!("{VERSION}\n")));
        assert_eq!(get(c, "cache/upgrade-pending"), None);
        assert_eq!(report(c), (VERSION.to_string(), ""));
    }

    #[test]
    fn adopting_stamps_host_json_only_when_it_is_what_runs() {
        let s = scratch("adopt");
        let c = &s.0;
        put(c, "host.json", "{}\n");
        put(c, "host.json.by", "3.10.1\n");
        put(c, "host.running", "1 1\n");
        put(c, "cache/upgrade-pending", &format_pending(VERSION, "3.10.1", None));
        adopt(c);
        assert_eq!(get(c, "host.running.by").as_deref(), Some(&*format!("{VERSION}\n")));
        assert_eq!(get(c, "cache/upgrade-pending"), None);
        assert_eq!(get(c, "host.json.by").as_deref(), Some("3.10.1\n"), "host.json is not what runs");
        put(c, "host.running", &format!("{} 3\n", rowt_core::cksum::posix(b"{}\n")));
        adopt(c);
        assert_eq!(get(c, "host.json.by").as_deref(), Some(&*format!("{VERSION}\n")));
    }

    #[test]
    fn a_failed_reload_is_reported_until_this_render_runs() {
        let s = scratch("failed");
        let c = &s.0;
        put(c, "host.running.by", "3.10.1\n");
        put(c, "cache/upgrade-pending", &format_pending(VERSION, "3.10.1", None));
        mark_reloaded(c, VERSION);
        assert_eq!(get(c, "cache/upgrade-pending"), None);
        assert_eq!(report(c), ("3.10.1".into(), "failed"));
        put(c, "host.running.by", &format!("{VERSION}\n"));
        assert_eq!(report(c), (VERSION.to_string(), ""));
    }
}
