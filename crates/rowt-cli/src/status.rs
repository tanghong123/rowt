//! rowt #s41: the escape lane's health in `rowt status` — the `escape:` block
//! and `status --json` — from what the watchdog and the follower last
//! observed (`watch.last`, `follow.last`; `rowt_core::status`), the clash
//! API's own records (which server the selector is on, each member's last
//! test), and the files status already reads. Passive by default: nothing
//! here probes the lane unless no observation is fresh, or `--probe` asks
//! (Hong, #s42). One copy: bin/rowt reaches it as `rowt-rs _status`, the way
//! it reaches the escape-name check.
//!
//! The watchdog's knobs — how often it ticks, where its streak recovers — are
//! the installed agent's (its plist), not this shell's environment: `watch
//! install` bakes `ROWT_*` into the plist, and a `status` run under other
//! values would call a healthy record stale or say down at the wrong count.
use std::path::Path;

use rowt_core::status::{self as st, Obs};
use rowt_platform::{Mac, Platform};
use serde_json::{json, Value};

use crate::lifecycle::{self, Ctx};
use crate::{env_or, read, PROG};

/// The schema version of `status --json`. Changes are additive: a new key, or
/// a new value for an enum only with a new number.
pub const SCHEMA: u32 = 1;

/// One escape server as it was last tested: by the clash API's delay history
/// when it has one, else by the watchdog's latency table.
pub struct Member {
    pub tag: String,
    pub kind: String,
    /// What its last test said: answered (`Some(true)`), failed
    /// (`Some(false)`), or never tested that rowt knows of (`None`).
    pub alive: Option<bool>,
    pub last_delay_ms: Option<u32>,
    pub last_test_at: Option<String>,
}

/// Everything the block and the JSON are built from, gathered once.
pub struct Health {
    pub mode: String,
    pub router_pid: Option<i32>,
    pub captive: bool,
    pub hold: Option<(String, String)>,
    pub selection: String,
    pub preferred: Option<String>,
    pub active: Option<String>,
    pub fails: u32,
    pub fails_limit: u32,
    pub interval: i64,
    pub window: i64,
    pub watch_obs: Option<Obs>,
    pub follow_obs: Option<Obs>,
    /// The observation judged: the freshest fresh record, or the probe made now.
    pub obs: Option<Obs>,
    pub probed_now: bool,
    pub last_switch: Option<(String, String, String, &'static str)>,
    pub members: Vec<Member>,
    pub verdict: st::Verdict,
}

/// The watchdog's tick interval and streak limit as the INSTALLED agent has
/// them (`StartInterval` and `ROWT_HEALTH_FAILS` in its plist), else this
/// environment's, else the defaults.
fn watchdog_knobs() -> (i64, u32) {
    let plist = read(&crate::watch::plist_path());
    let field = |tag: &str, key: &str| -> Option<String> {
        let open = format!("<key>{key}</key><{tag}>");
        let rest = &plist[plist.find(&open)? + open.len()..];
        Some(rest[..rest.find('<')?].trim().to_string())
    };
    let interval = field("integer", "StartInterval").or_else(|| std::env::var("ROWT_WATCH_INTERVAL").ok())
        .and_then(|v| v.parse().ok()).unwrap_or(120);
    let fails_limit = if plist.is_empty() { std::env::var("ROWT_HEALTH_FAILS").ok() } else { field("string", "ROWT_HEALTH_FAILS") }
        .and_then(|v| v.parse().ok()).unwrap_or(3);
    (interval, fails_limit)
}

fn hold_of(ctx: &Ctx) -> Option<(String, String)> {
    if ctx.mode() != "host" {
        return None;
    }
    let body = read(&ctx.cfg.join("watch.hold"));
    let (since, tag) = body.trim_end_matches('\n').split_once('\t')?;
    (!tag.is_empty() && ctx.sget("selected") == tag).then(|| (since.to_string(), tag.to_string()))
}

/// `GET /proxies`: every outbound with its delay history, in servers.json
/// order — and, for one the API has no history of, the watchdog's latency
/// table (`latency.tsv`). sing-box's clash API DELETES a server's delay
/// history when the server fails a test (Clash Meta records a 0 ms entry
/// instead), so from the API alone a server that stopped answering reads
/// exactly like one never tested: on 2026-11-06 the selected server failed
/// the watchdog's probe for five minutes and this line said "untested" of it,
/// next to members with numbers (#s86). The table the watchdog writes every
/// round keeps a no-answer row (an empty median) with the round's time, so it
/// answers where the API has forgotten. The API's own entry wins where it has
/// one: both come from the same round, and the API's is the later writer.
fn members(ctx: &Ctx) -> Vec<Member> {
    let servers: Vec<Value> = serde_json::from_str(&read(&ctx.cfg.join("servers.json"))).unwrap_or_default();
    let Some(body) = lifecycle::clash_curl(ctx, "GET", "/proxies", None) else { return Vec::new() };
    let Some(all) = serde_json::from_str::<Value>(&body).ok().and_then(|v| v.get("proxies").cloned()) else { return Vec::new() };
    let table = rowt_core::latency::table_parse(&read(&crate::best::table_path(ctx)));
    servers.iter().filter_map(|s| {
        let tag = s.get("tag")?.as_str()?.to_string();
        let kind = s.get("type").and_then(|t| t.as_str()).unwrap_or("").to_string();
        let last = all.get(&tag).and_then(|p| p.get("history")).and_then(|h| h.as_array()).and_then(|h| h.last().cloned());
        let row = table.iter().find(|r| r.tag == tag);
        Some(member_of(tag, kind, last.as_ref(), row))
    }).collect()
}

/// A member from the API's last history entry when there is one, else from
/// its latency-table row, else untested.
fn member_of(tag: String, kind: String, last: Option<&Value>, row: Option<&rowt_core::latency::Row>) -> Member {
    if let Some(e) = last {
        let delay = e.get("delay").and_then(|d| d.as_u64()).map(|d| d as u32);
        let at = e.get("time").and_then(|t| t.as_str()).and_then(st::rfc3339_to_stamp);
        return Member { tag, kind, alive: delay.map(|d| d > 0), last_delay_ms: delay.filter(|d| *d > 0), last_test_at: at };
    }
    match row {
        // An empty median is the table's "no answer"; `at` is the round's
        // UTC stamp, in the same form the API's time is converted to.
        Some(r) => Member {
            tag, kind,
            alive: Some(r.median.is_some()),
            last_delay_ms: r.median.map(|m| m.min(u32::MAX as u64) as u32),
            last_test_at: Some(r.at.clone()),
        },
        None => Member { tag, kind, alive: None, last_delay_ms: None, last_test_at: None },
    }
}

/// One member on the `members:` line.
fn member_text(m: &Member) -> String {
    let at = m.last_test_at.as_deref().map(|s| s.get(11..19).unwrap_or(s).to_string()).unwrap_or_default();
    match (m.alive, m.last_delay_ms) {
        (Some(true), Some(d)) => format!("{} {d} ms at {at}", m.tag),
        (Some(_), _) => format!("{} no answer at {at}", m.tag),
        (None, _) => format!("{} untested", m.tag),
    }
}

pub fn gather(ctx: &Ctx, force_probe: bool) -> Health {
    let mode = ctx.mode();
    let router_pid = lifecycle::host_running(ctx);
    let captive = ctx.sget("captive") == "1";
    let hold = hold_of(ctx);
    let sel = { let s = ctx.sget("selected"); if s.is_empty() { "auto".to_string() } else { s } };
    let selection = match sel.as_str() {
        "auto" => "auto",
        "best" => "best",
        _ if ctx.sget("failover") == "1" => "failover",
        _ => "pinned",
    }.to_string();
    let preferred = (selection == "pinned" || selection == "failover").then(|| sel.clone());
    let active = if router_pid.is_some() {
        lifecycle::clash_selected(ctx).and_then(|now| if now == "auto" { lifecycle::clash_auto_pick(ctx) } else { Some(now) })
    } else {
        None
    };
    let fails: u32 = read(&ctx.cfg.join("watch.health")).trim().parse().unwrap_or(0);
    let (interval, fails_limit) = watchdog_knobs();
    let window = interval * 3;
    let now: i64 = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let watch_obs = Obs::parse(&read(&ctx.cfg.join("watch.last")));
    let follow_obs = Obs::parse(&read(&ctx.cfg.join("follow.last")));
    fn is_fresh(o: &Option<Obs>, now: i64, window: i64) -> Option<&Obs> {
        o.as_ref().filter(|o| st::fresh(&o.observed_at, now, window))
    }
    let best = st::freshest(is_fresh(&watch_obs, now, window), is_fresh(&follow_obs, now, window)).cloned();
    // The lane is probed only where an observation could be judged at all —
    // not in local mode, not with the router stopped, not under a portal or a
    // hold, which settle the verdict first — and only when nothing fresh was
    // observed, or on `--probe`. The answer is reported, never written.
    let probable = mode != "local" && router_pid.is_some() && !captive && hold.is_none();
    let (obs, probed_now) = if probable && (force_probe || best.is_none()) {
        let target = crate::watch::probe_target(ctx);
        // One try, the quick timeout: reported below, never recorded.
        let delay = crate::watch::delay_of_tag(ctx, &target);
        // A stale record still says when the lane was last seen ok; its
        // "since" stamps do not carry across the gap.
        let prev = st::freshest(watch_obs.as_ref(), follow_obs.as_ref())
            .map(|o| Obs { ok_since: String::new(), fail_since: String::new(), ..o.clone() });
        (Some(Obs::next(prev.as_ref(), &crate::best::utc(None), "status", &target, if delay.is_some() { "ok" } else { "fail" }, delay)), true)
    } else {
        (best, false)
    };
    let last_switch = {
        let parse = |s: String, by: &'static str| -> Option<(String, String, String, &'static str)> {
            // "<stamp> <from> -> <to>"
            let (at, rest) = (s.get(..19)?.to_string(), s.get(20..)?.to_string());
            let (from, to) = rest.split_once(" -> ")?;
            Some((at, from.to_string(), to.to_string(), by))
        };
        let f = ctx.sget("failover_switch");
        let a = ctx.sget("auto_switch");
        if !f.is_empty() { parse(f, "failover") } else if !a.is_empty() { parse(a, "auto") } else { None }
    };
    let members = if router_pid.is_some() { members(ctx) } else { Vec::new() };
    let verdict = st::verdict(&st::Inputs {
        mode: &mode,
        router_running: router_pid.is_some(),
        captive,
        hold: hold.as_ref().map(|(s, t)| (s.as_str(), t.as_str())),
        obs: obs.as_ref(),
        fails,
        fails_limit,
    });
    Health { mode, router_pid, captive, hold, selection, preferred, active, fails, fails_limit, interval, window,
             watch_obs, follow_obs, obs, probed_now, last_switch, members, verdict }
}

fn what(target: &str) -> String {
    match target {
        "escape" => "the lane".to_string(),
        "auto" => "auto's pick".to_string(),
        t => t.to_string(),
    }
}

fn who(source: &str) -> &'static str {
    match source {
        "watchdog" => "the watchdog's probe",
        "follower" => "the follower's test",
        _ => "a probe made now",
    }
}

fn utc(s: &str) -> String {
    if s.is_empty() { "never".to_string() } else { format!("{s} UTC") }
}

/// The `escape:` block — the verdict row, a note when `status` had to probe,
/// and the members' last tests. Nothing for `holding`: `watch::hold_row`
/// already says it, with the remedy.
pub fn rows(ctx: &Ctx, probe: bool) -> Vec<String> {
    let h = gather(ctx, probe);
    let mut o = Vec::new();
    let v = &h.verdict;
    let row = |s: String| format!("{:<14}{s}", "escape:");
    match (v.state, v.reason) {
        ("holding", _) => {}
        ("unknown", Some("local-mode")) => o.push(row("direct — local mode, no tunnel (the lane routes direct)".into())),
        ("down", Some("router-stopped")) => o.push(row(format!("DOWN — the router is stopped ('{PROG} up' starts it)"))),
        ("down", Some("captive-portal")) => o.push(row("DOWN — captive portal: the proxy is dropped until login clears (the watchdog restores it)".into())),
        ("unknown", _) => o.push(row("unknown — nothing observed, and no probe could be made (is the clash API reachable?)".into())),
        (state, reason) => {
            let ob = h.obs.as_ref().expect("an observation behind a judged state");
            let w = what(&ob.target);
            let at = &ob.observed_at;
            let d = ob.delay_ms.map(|d| format!("{d} ms")).unwrap_or_else(|| "no delay recorded".into());
            let s = match (state, reason) {
                ("ok", _) => format!("ok — {w} answered {} at {at} UTC ({d}); ok since {}", who(&ob.source), utc(&v.since)),
                ("degraded", Some("slow")) => format!("degraded — {w} answered only the patient probe at {at} UTC ({d}, {}): slow, not wedged; ok since {}", who(&ob.source), utc(&v.since)),
                ("degraded", _) => format!("degraded — {w} did not answer {} at {at} UTC: {} of {} failures before the watchdog reloads; failing since {}; last ok {}",
                                           who(&ob.source), h.fails, h.fails_limit, utc(&v.since), utc(&ob.last_ok_at)),
                ("down", Some("all-dead")) => format!("DOWN — no server answered the follower at {at} UTC; down since {}; last ok {}", utc(&v.since), utc(&ob.last_ok_at)),
                ("down", Some("offline")) => format!("DOWN — offline at {at} UTC (the NIC had no address); last ok {}", utc(&ob.last_ok_at)),
                ("down", _) if ob.source == "status" => format!("DOWN — {w} did not answer a probe made now ({at} UTC); last ok {}", utc(&ob.last_ok_at)),
                ("down", _) => format!("DOWN — {w} did not answer {} at {at} UTC ({} of {} failures); down since {}; last ok {}",
                                       who(&ob.source), h.fails, h.fails_limit, utc(&v.since), utc(&ob.last_ok_at)),
                _ => format!("{state} — {w} at {at} UTC"),
            };
            o.push(row(s));
        }
    }
    if h.probed_now {
        let stale = st::freshest(h.watch_obs.as_ref(), h.follow_obs.as_ref());
        o.push(format!("{:<14}{}", "", match stale {
            Some(s) => format!("probed now: the last observation ({}, {} UTC) is older than {} s", s.source, s.observed_at, h.window),
            None => format!("probed now: nothing has been observed yet (the watchdog probes every {} s once installed)", h.interval),
        }));
    }
    if !h.members.is_empty() {
        let parts: Vec<String> = h.members.iter().map(member_text).collect();
        o.push(format!("{:<14}{}", "members:", parts.join(", ")));
    }
    o
}

/// `status --json`: schema [`SCHEMA`]. Every key always present; null where
/// it does not apply; stamps UTC `YYYY-MM-DD HH:MM:SS`; no age fields; never
/// the clash secret; no server addresses.
pub fn json(ctx: &Ctx, probe: bool) -> String {
    let h = gather(ctx, probe);
    let cfg = &ctx.cfg;
    let p = Mac;
    let svc = p.active_service();
    let proxy_on = svc.as_ref().map(|s| {
        rowt_platform::read_proxy(s, "-getsecurewebproxy").lines()
            .find(|l| l.starts_with("Enabled:")).and_then(|l| l.split_whitespace().nth(1)) == Some("Yes")
    });
    let pac = svc.as_ref().map(|s| rowt_platform::pac_on(s));
    let sb = ctx.sb();
    let sbv = crate::fetch::sb_version(&sb);
    let plist = crate::watch::plist_path();
    let loaded = crate::watch::loaded();
    let esc = read(&cfg.join("escape-domains.txt"));
    let corp = read(&cfg.join("corp-domains.txt"));
    let blk = read(&cfg.join("block-domains.txt"));
    let n = |src: &str, f| rowt_core::render::parse_list(src, f).len();
    let mut geo: Vec<String> = rowt_core::render::geosites_of(&esc);
    geo.extend(rowt_core::render::geosites_of(&blk));
    geo.sort(); geo.dedup();
    let port_rows = rowt_core::ports::rows(&read(&cfg.join("ports.tsv")));
    let rendered = rowt_core::ports::rendered(&read(&ctx.host_cfg()));
    let ports: Vec<Value> = port_rows.iter().filter(|p| !p.name.is_empty()).map(|p| json!({
        "name": p.name,
        "port": p.listen_port(),
        "unmatched": p.fin,
        "rendered": rendered.iter().any(|(n, rp)| *n == p.name && *rp == p.port),
    })).collect();
    let obs_json = |o: &Obs, by: &str| json!({
        "at": o.observed_at, "source": by, "target": o.target,
        "ok": matches!(o.result.as_str(), "ok" | "slow"),
        "result": o.result, "delay_ms": o.delay_ms,
    });
    let last_ok_at = [h.watch_obs.as_ref(), h.follow_obs.as_ref(), h.obs.as_ref()].iter().flatten()
        .map(|o| o.last_ok_at.clone()).filter(|s| !s.is_empty())
        .max_by_key(|s| st::stamp_epoch(s).unwrap_or(i64::MIN));
    let intent = { let i = ctx.sget("intent"); if i.is_empty() { Value::Null } else { Value::String(i) } };
    let engine = if sbv.is_empty() { Value::Null } else { Value::String(sbv.clone()) };
    let engine_pinned = if sbv.is_empty() { Value::Null } else { Value::Bool(crate::fetch::sb_pinned(&sb)) };
    let since = if h.verdict.since.is_empty() { Value::Null } else { Value::String(h.verdict.since.clone()) };
    let servers: Vec<Value> = h.members.iter().map(|m| json!({
        "tag": m.tag, "type": m.kind, "alive": m.alive,
        "last_delay_ms": m.last_delay_ms, "last_test_at": m.last_test_at,
    })).collect();
    let last_probe = h.obs.as_ref().map(|o| obs_json(o, &o.source));
    let last_switch = h.last_switch.as_ref().map(|(at, from, to, by)| json!({"at": at, "from": from, "to": to, "by": by}));
    let hold = h.hold.as_ref().map(|(since, tag)| json!({"since": since, "tag": tag}));
    let v = json!({
        "schema": SCHEMA,
        "rowt": env!("ROWT_SHELL_VERSION"),
        "generated_at": crate::best::utc(None),
        "mode": h.mode,
        "intent": intent,
        "router": {
            "running": h.router_pid.is_some(),
            "pid": h.router_pid,
            "port": ctx.port,
            "engine": engine,
            "engine_pinned": engine_pinned,
        },
        "proxy": { "system": proxy_on, "service": svc, "pac": pac },
        "captive": h.captive,
        "watchdog": {
            "loaded": loaded,
            "installed": plist.is_file(),
            "interval_s": h.interval,
            "last_observation_at": h.watch_obs.as_ref().map(|o| o.observed_at.clone()),
        },
        "follower": {
            "running": lifecycle::follower_running(ctx),
            "every_s": env_or("ROWT_AUTO_FOLLOW_EVERY", "15").parse::<u64>().unwrap_or(15),
            "last_observation_at": h.follow_obs.as_ref().map(|o| o.observed_at.clone()),
        },
        "escape": {
            "state": h.verdict.state,
            "reason": h.verdict.reason,
            "since": since,
            "selection": h.selection,
            "preferred": h.preferred,
            "active": h.active,
            "fails": h.fails,
            "fails_limit": h.fails_limit,
            "last_probe": last_probe,
            "probed_now": h.probed_now,
            "last_ok_at": last_ok_at,
            "last_switch": last_switch,
            "hold": hold,
            "servers": servers,
        },
        "lanes": {
            "escape": n(&esc, rowt_core::render::Filter::All) + n(&esc, rowt_core::render::Filter::Exact),
            "corp": n(&corp, rowt_core::render::Filter::Domain) + n(&corp, rowt_core::render::Filter::Cidr) + n(&corp, rowt_core::render::Filter::Exact),
            "block": n(&blk, rowt_core::render::Filter::All) + n(&blk, rowt_core::render::Filter::Exact),
            "ads": cfg.join("cache/geosite-category-ads-all.srs").is_file(),
            "geosites": geo,
            "final": env_or("ROWT_FINAL", "direct"),
        },
        "ports": ports,
    });
    serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".into())
}

/// `rowt-rs _status <rows|json> [--probe] --config-dir DIR` — the shell's door
/// to the same code, answered ahead of the preamble like `_escape-dns`.
pub fn helper(args: &[String]) -> i32 {
    let mut it = args.iter();
    let action = it.next().map(|s| s.as_str()).unwrap_or("");
    let mut dir: Option<String> = None;
    let mut probe = false;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--config-dir" => dir = it.next().cloned(),
            "--probe" => probe = true,
            _ => {}
        }
    }
    let (Some(dir), true) = (dir, matches!(action, "rows" | "json")) else {
        eprintln!("usage: rowt-rs _status <rows|json> [--probe] --config-dir DIR");
        return 2;
    };
    let ctx = Ctx::new(Path::new(&dir).to_path_buf());
    if action == "json" {
        println!("{}", json(&ctx, probe));
    } else {
        for l in rows(&ctx, probe) {
            println!("{l}");
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use rowt_core::latency::Row;

    fn row(median: Option<u64>) -> Row {
        Row { tag: "alpha".into(), median, ok: if median.is_some() { 3 } else { 0 }, at: "2100-01-01 00:05:00".into() }
    }

    fn m(last: Option<&Value>, row: Option<&Row>) -> Member {
        member_of("alpha".into(), "vless".into(), last, row)
    }

    #[test]
    fn a_member_is_read_from_the_api_history_first_then_the_latency_table() {
        let hist = json!({"time": "2100-01-01T00:00:00Z", "delay": 120});
        let x = m(Some(&hist), Some(&row(None)));
        assert_eq!((x.alive, x.last_delay_ms, x.last_test_at.as_deref()), (Some(true), Some(120), Some("2100-01-01 00:00:00")));
        assert_eq!(member_text(&x), "alpha 120 ms at 00:00:00");
        // sing-box dropped the history after a failed test: the table's empty
        // median says the server answered nothing, and when (#s86).
        let x = m(None, Some(&row(None)));
        assert_eq!((x.alive, x.last_delay_ms, x.last_test_at.as_deref()), (Some(false), None, Some("2100-01-01 00:05:00")));
        assert_eq!(member_text(&x), "alpha no answer at 00:05:00");
        let x = m(None, Some(&row(Some(83))));
        assert_eq!((x.alive, x.last_delay_ms), (Some(true), Some(83)));
        assert_eq!(member_text(&x), "alpha 83 ms at 00:05:00");
        // Nothing anywhere: untested, as before.
        let x = member_of("gamma".into(), "vless".into(), None, None);
        assert_eq!((x.alive, x.last_delay_ms, x.last_test_at.as_deref()), (None, None, None));
        assert_eq!(member_text(&x), "gamma untested");
        // A Clash Meta style failure entry (delay 0) still reads as no answer,
        // and a history entry without a delay still reads as untested.
        let fail = json!({"time": "2100-01-01T00:00:00Z", "delay": 0});
        assert_eq!(member_text(&m(Some(&fail), Some(&row(Some(83))))), "alpha no answer at 00:00:00");
        let odd = json!({"time": "2100-01-01T00:00:00Z"});
        assert_eq!(member_text(&m(Some(&odd), None)), "alpha untested");
    }
}
