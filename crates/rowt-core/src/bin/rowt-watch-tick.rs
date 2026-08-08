//! `rowt-watch-tick` — decide what a watchdog tick should do, and print the plan.
//!
//!   rowt-watch-tick --obs FILE [--phase guard|netcheck]
//!
//! Takes an observation as JSON and prints one action per line. It performs
//! nothing: the effects belong to the platform layer, and keeping them out is
//! what makes the decision table testable without toggling a real system proxy.
//!
//! Output vocabulary (one per line):
//!   journal <state> | log <text> | audit <text>
//!   captive-proxy-off <svc> | captive-proxy-on <svc> | clear-stale-proxy <svc>
//!   recover <reason> | corp-sync | write-net-id <id> | reload <reason>
//!   next <stop|settle>

use rowt_core::watch::{guard, netcheck, Action, CaptiveState, Config, Next, Observation, State};
use serde_json::Value;
use std::process::ExitCode;

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}
fn b(v: &Value, k: &str) -> bool {
    v.get(k).and_then(|x| x.as_bool()).unwrap_or(false)
}
fn opt(v: &Value, k: &str) -> Option<String> {
    match v.get(k).and_then(|x| x.as_str()) {
        Some(x) if !x.is_empty() => Some(x.to_string()),
        _ => None,
    }
}

fn render(a: &Action) -> String {
    match a {
        Action::Journal(c) => format!("journal {}", c.as_str()),
        Action::Log(t) => format!("log {t}"),
        Action::Audit(t) => format!("audit {t}"),
        Action::CaptiveProxyOff(x) => format!("captive-proxy-off {x}"),
        Action::CaptiveProxyOn(x) => format!("captive-proxy-on {x}"),
        Action::ClearStaleProxy(x) => format!("clear-stale-proxy {x}"),
        Action::Recover(r) => format!("recover {r}"),
        Action::CorpSync => "corp-sync".to_string(),
        Action::WriteNetId(n) => format!("write-net-id {n}"),
        Action::Reload(r) => format!("reload {r}"),
    }
}

fn run() -> Result<String, String> {
    let mut args = std::env::args().skip(1);
    let (mut obs_path, mut phase) = (None::<String>, "guard".to_string());
    while let Some(a) = args.next() {
        match a.as_str() {
            "--obs" => obs_path = args.next(),
            "--phase" => phase = args.next().unwrap_or_else(|| "guard".into()),
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    let path = obs_path.ok_or("--obs FILE is required")?;
    let body = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
    let v: Value = serde_json::from_str(&body).map_err(|e| format!("{path}: {e}"))?;

    let o = v.get("obs").unwrap_or(&v);
    let obs = Observation {
        proxy_intent: s(o, "proxy_intent"),
        captive: o.get("captive").and_then(|c| c.as_str()).map(CaptiveState::parse),
        active_service: opt(o, "active_service"),
        proxy_any_on: b(o, "proxy_any_on"),
        host_running: b(o, "host_running"),
        intent: s(o, "intent"),
        boot_matches: b(o, "boot_matches"),
        iface: opt(o, "iface"),
        proxy_pointing_ok: b(o, "proxy_pointing_ok"),
        proxy_bypass_ok: b(o, "proxy_bypass_ok"),
        bound_iface: opt(o, "bound_iface"),
        net_id: s(o, "net_id"),
        mode: s(o, "mode"),
        health_ok: b(o, "health_ok"),
        now: o.get("now").and_then(|x| x.as_i64()).unwrap_or(0),
    };
    let empty = Value::Object(Default::default());
    let sv = v.get("state").unwrap_or(&empty);
    let st = State {
        captive_flag: b(sv, "captive_flag"),
        health_fails: sv.get("health_fails").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
        last_net_id: opt(sv, "last_net_id"),
        last_recovery: sv.get("last_recovery").and_then(|x| x.as_i64()).unwrap_or(0),
    };

    let cfg = Config::default();
    let out = match phase.as_str() {
        "guard" => guard(&obs, &st, &cfg),
        "netcheck" => netcheck(&obs, &st, &cfg),
        other => return Err(format!("unknown phase: {other}")),
    };
    let mut lines: Vec<String> = out.actions.iter().map(render).collect();
    lines.push(format!(
        "next {}",
        match out.next {
            Next::Stop => "stop",
            Next::Settle => "settle",
        }
    ));
    Ok(lines.join("\n"))
}

fn main() -> ExitCode {
    match run() {
        Ok(s) => {
            println!("{s}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("rowt-watch-tick: {e}");
            ExitCode::FAILURE
        }
    }
}
