//! `rowt-probe` — perform one platform operation, so the harness can compare the
//! argv it produces against the shell's. Not a user-facing tool.

use rowt_platform::{Mac, Platform};
use std::process::ExitCode;

fn main() -> ExitCode {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let p = Mac;
    let svc = |i: usize| a.get(i).cloned().unwrap_or_default();
    let port: u16 = std::env::var("ROWT_PORT").ok().and_then(|v| v.parse().ok()).unwrap_or(7890);
    match a.first().map(|s| s.as_str()) {
        Some("service") => println!("{}", p.active_service().unwrap_or_default()),
        Some("iface") => println!("{}", p.detect_iface().unwrap_or_default()),
        Some("boot-id") => println!("{}", p.boot_id().unwrap_or_default()),
        Some("gfw") => {
            let canaries: Vec<String> = std::env::var("ROWT_GFW_CANARIES")
                .unwrap_or_else(|_| "https://www.google.com/generate_204 https://www.youtube.com/generate_204".into())
                .split_whitespace().map(|s| s.to_string()).collect();
            let t: u32 = std::env::var("ROWT_GFW_TIMEOUT").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
            println!("{}", p.direct_reaches_escape(&canaries, t));
        }
        Some("proxy-any-on") => println!("{}", p.proxy_any_on(&svc(1))),
        Some("proxy-pointing-ok") => println!("{}", p.proxy_pointing_ok(&svc(1), port)),
        Some("proxy-on") => {
            let _ = p.proxy_set(&svc(1), port);
        }
        Some("captive-off") => {
            let _ = p.proxy_states_off(&svc(1), true);
        }
        Some("captive-on") => {
            let _ = p.proxy_states_on(&svc(1), true);
        }
        _ => {
            eprintln!("usage: rowt-probe <service|iface|boot-id|proxy-any-on|proxy-pointing-ok|proxy-on|captive-off|captive-on> [service]");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}
