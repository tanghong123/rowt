//! `rowt report` — the self-contained diagnostic you can run offline and hand
//! back. Secrets masked.
//!
//! Almost every line here is a probe of the live machine, so the report's VALUE
//! is exactly what a differential gate cannot check: a sandbox answers with
//! canned output, and comparing two implementations against the same canned
//! output proves the formatting agrees, not that the diagnosis is right. What
//! `cli-diff` does gate here is the shape — the section order and the argv of
//! every probe — which is what makes one report comparable to another.

use crate::lifecycle::{self, Ctx};
use crate::{env_or, pad, read};
use rowt_platform::{Mac, Platform};
use serde_json::Value;
use std::path::Path;
use std::process::{Command, Stdio};

fn out(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd).args(args).stderr(Stdio::null()).output().ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim_end().to_string())
        .unwrap_or_default()
}

/// `_run` — echo the command, then its output indented, never failing.
fn run_show(o: &mut Vec<String>, cmd: &str, args: &[&str]) {
    o.push(format!("$ {cmd} {}", args.join(" ")));
    let body = Command::new(cmd).args(args).output().ok()
        .map(|r| {
            let mut s = String::from_utf8_lossy(&r.stdout).into_owned();
            s.push_str(&String::from_utf8_lossy(&r.stderr));
            s
        })
        .unwrap_or_default();
    for l in body.lines() {
        o.push(format!("  {l}"));
    }
}

fn tail(p: &Path, n: usize, indent: &str, empty: &str) -> Vec<String> {
    if !p.is_file() {
        return vec![empty.to_string()];
    }
    let body = read(p);
    let lines: Vec<&str> = body.lines().collect();
    lines[lines.len().saturating_sub(n)..].iter().map(|l| format!("{indent}{l}")).collect()
}

/// `tcp_reaches` — did the TCP layer reach the server? Any TLS/HTTP result
/// counts; only "could not connect" (curl 7 refused / 28 timeout / 6 dns) is a
/// failure, because the question is reachability, not a working handshake.
pub fn tcp_reaches(iface: &str, ip: &str, port: &str) -> bool {
    let t = env_or("ROWT_PROBE_TIMEOUT", "5");
    let url = format!("https://{ip}:{port}/");
    let mut c = Command::new("curl");
    if !iface.is_empty() {
        c.args(["--interface", iface]);
    }
    // --noproxy so a proxy env var cannot hijack a test OF the server.
    let st = c.args(["--noproxy", "*", "-sS", "--connect-timeout", &t, "-m", &t, "-o", "/dev/null", &url])
        .stdout(Stdio::null()).stderr(Stdio::null()).status();
    !matches!(st.map(|s| s.code()), Ok(Some(7)) | Ok(Some(28)) | Ok(Some(6)))
}

pub fn body(ctx: &Ctx, here: &Path) -> String {
    let cfg = &ctx.cfg;
    let mut o: Vec<String> = Vec::new();
    let p = Mac;
    let sb = ctx.sb();
    // NOT hoisted. The shell writes `$(detect_iface)` at four separate points
    // and each substitution re-probes the network — route + two ipconfigs, four
    // times over. Caching it here would be an obvious improvement and a trace
    // difference, and `cli-diff` compares the trace. §6.7: the shell-side fix
    // is its own commit.

    o.push("===== rowt diag =====".into());
    o.push(format!("when:      {}", crate::sh_date("+%Y-%m-%d %H:%M:%S %z")));
    o.push(format!("rowt:      {}   ({}/bin/{})", env!("ROWT_SHELL_VERSION"), here.display(), crate::PROG));
    o.push(format!("macOS:     {}  {}  shell={}",
        out("sw_vers", &["-productVersion"]), out("uname", &["-m"]),
        login_shell()));
    o.push(format!("config:    {}", cfg.display()));
    o.push(String::new());

    o.push("----- dependencies -----".into());
    if sb.is_file() {
        let v = out(&sb.to_string_lossy(), &["version"]);
        let v = v.lines().next().unwrap_or("").split_whitespace().nth(2).unwrap_or("");
        o.push(format!("  sing-box   {v}  ({})", sb.display()));
    } else {
        o.push("  sing-box   MISSING (auto-installs on first render)".into());
    }
    for t in ["jq", "python3", "curl", "limactl"] {
        let w = out("command", &["-v", t]);
        let w = if w.is_empty() { which_path(t) } else { w };
        o.push(format!("  {} {}", pad(t, 10), if w.is_empty() { "MISSING".into() } else { w }));
    }
    o.push(String::new());

    o.push("----- state -----".into());
    o.push(format!("  mode:     {}      selected: {}      final: {}",
        ctx.sget("mode"), ctx.sget("selected"), env_or("ROWT_FINAL", "direct")));
    o.push(format!("  ports:    proxy={}  clash={}   iface(detected): {}",
        ctx.port, ctx.clash_port, p.detect_iface().unwrap_or_else(|| "none".into())));
    o.push(format!("  vm_ip:    {}", ctx.sget("vm_ip")));
    o.push(String::new());

    o.push("----- servers (secrets masked) -----".into());
    let servers: Vec<Value> =
        serde_json::from_str(&read(&cfg.join("servers.json"))).unwrap_or_default();
    if servers.is_empty() {
        o.push("  (none)".into());
    } else {
        for s in &servers {
            let g = |k: &str| s.get(k).map(|v| match v {
                Value::String(x) => x.clone(), other => other.to_string(),
            }).unwrap_or_default();
            // The jq filter emits its own two leading spaces INSIDE the field
            // (`"  \(.tag)\t…"`), and `read` with IFS=tab does not strip them —
            // so they are part of what `%-20s` pads. Four spaces of indent, and
            // a tag column two narrower than it looks.
            o.push(format!("  {} {} {}:{}",
                pad(&format!("  {}", g("tag")), 20), pad(&g("type"), 7), g("server"), g("server_port")));
        }
    }
    let subs = read(&cfg.join("subs.txt"));
    let active: Vec<&str> = subs.lines()
        .filter(|l| { let t = l.trim(); !t.is_empty() && !t.starts_with('#') }).collect();
    o.push(format!("  subscriptions: {}", active.len()));
    // A subscription URL carries a token — the first 10 characters after the
    // scheme are enough to recognise which one it is, and the rest is a secret.
    for l in &active {
        o.push(mask_url(l));
    }
    let esc = read(&cfg.join("escape-domains.txt"));
    let corp = read(&cfg.join("corp-domains.txt"));
    let blk = read(&cfg.join("block-domains.txt"));
    let dir = read(&cfg.join("direct-domains.txt"));
    use rowt_core::render::{geosites_of, parse_list, Filter};
    let ads = cfg.join("cache/geosite-category-ads-all.srs").is_file();
    let mut geo: Vec<String> = geosites_of(&esc);
    geo.extend(geosites_of(&blk));
    geo.sort();
    geo.dedup();
    o.push(format!("  buckets: escape={} corp={} block={}{}{} direct={}",
        parse_list(&esc, Filter::All).len() + parse_list(&esc, Filter::Exact).len(),
        parse_list(&corp, Filter::Domain).len() + parse_list(&corp, Filter::Cidr).len()
            + parse_list(&corp, Filter::Exact).len(),
        parse_list(&blk, Filter::All).len() + parse_list(&blk, Filter::Exact).len(),
        if ads { " +geosite-ads" } else { "" },
        if geo.is_empty() { String::new() } else { format!(" +geosite:{}", geo.join(",")) },
        parse_list(&dir, Filter::Domain).len() + parse_list(&dir, Filter::Exact).len()));
    o.push(String::new());

    o.push("----- generated configs -----".into());
    if sb.is_file() {
        for (name, path, pad_to) in [("host.json", ctx.host_cfg(), "host.json:"), ("vm.json", ctx.vm_cfg(), "vm.json:  ")] {
            if !path.is_file() {
                o.push(format!("  {name}: not rendered"));
                continue;
            }
            let ok = Command::new(&sb).arg("check").arg("-c").arg(&path)
                .stdout(Stdio::null()).stderr(Stdio::null()).status()
                .map(|s| s.success()).unwrap_or(false);
            if ok {
                o.push(format!("  {pad_to} OK"));
            } else {
                o.push(format!("  {pad_to} INVALID"));
                if name == "host.json" {
                    let e = Command::new(&sb).arg("check").arg("-c").arg(&path).output().ok()
                        .map(|r| String::from_utf8_lossy(&r.stderr).into_owned()).unwrap_or_default();
                    for l in e.lines() {
                        o.push(format!("    {l}"));
                    }
                }
            }
        }
    } else {
        o.push("  (sing-box not installed yet)".into());
    }
    o.push(String::new());

    o.push("----- network context -----".into());
    run_show(&mut o, "route", &["-n", "get", "default"]);
    o.push("  VPN services (scutil):".into());
    for l in out("scutil", &["--nc", "list"]).lines() {
        o.push(format!("    {l}"));
    }
    // `detect_iface >/dev/null && ipconfig getifaddr "$(detect_iface)"` — two
    // more probes, and the second only when the first succeeded.
    let addr = match p.detect_iface() {
        Some(_) => p.detect_iface().map(|i| out("ipconfig", &["getifaddr", &i])).unwrap_or_default(),
        None => String::new(),
    };
    o.push(format!("  physical iface addr: {}", if addr.is_empty() { "n/a".into() } else { addr }));
    o.push(String::new());

    o.push("----- runtime -----".into());
    match lifecycle::host_running(ctx) {
        Some(pid) => o.push(format!("  router: RUNNING (pid {pid}) on 127.0.0.1:{}", ctx.port)),
        None => o.push("  router: stopped".into()),
    }
    if let Some(svc) = p.active_service() {
        let b = rowt_platform::read_proxy(&svc, "-getsecurewebproxy");
        let en = b.lines().find(|l| l.contains("Enabled"))
            .and_then(|l| l.split_whitespace().nth(1)).unwrap_or("");
        o.push(format!("  system proxy on '{svc}': {en}"));
    }
    o.push(String::new());

    o.push("----- reachability: each server via default route AND physical NIC -----".into());
    o.push("  (default ok + iface FAIL => corp enforces via packet filter => use mode vm)".into());
    let iface = p.detect_iface().unwrap_or_default();
    if servers.is_empty() || iface.is_empty() {
        o.push("  (no servers or no interface)".into());
    } else {
        for s in &servers {
            let g = |k: &str| s.get(k).map(|v| match v {
                Value::String(x) => x.clone(), other => other.to_string(),
            }).unwrap_or_default();
            let (tag, host, port) = (g("tag"), g("server"), g("server_port"));
            let ip = rowt_platform::resolve_ip(&host);
            if ip.is_empty() {
                o.push(format!("  {} DNS-FAIL ({host})", pad(&tag, 20)));
                continue;
            }
            let d = if tcp_reaches("", &ip, &port) { "ok" } else { "FAIL" };
            let b = if tcp_reaches(&iface, &ip, &port) { "ok" } else { "FAIL" };
            o.push(format!("  {} {} default:{d}  {iface}:{b}",
                           pad(&tag, 20), pad(&format!("{ip}:{port}"), 21)));
        }
    }
    o.push(String::new());

    o.push("----- DNS checks -----".into());
    let dns = env_or("ROWT_DNS_DIRECT", "223.5.5.5");
    run_show(&mut o, "bash", &["-c", &format!("dig +short +time=3 @{dns} www.baidu.com A | head -3")]);
    let cd = corp.lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with('#') && !l.rsplit('/').next().unwrap_or("").chars().all(|c| c.is_ascii_digit()));
    if let Some(cd) = cd {
        o.push(format!("  corp name '{cd}' via system resolver:"));
        run_show(&mut o, "bash", &["-c", &format!("dscacheutil -q host -a name {cd} | grep ip_address | head -3")]);
    }
    o.push(String::new());

    o.push("----- through-proxy tests (only meaningful if router is running) -----".into());
    if lifecycle::host_running(ctx).is_some() {
        let px = format!("socks5h://127.0.0.1:{}", ctx.port);
        o.push(format!("  direct bucket  (baidu.com):  HTTP {}",
                       curl_code_t(&px, "https://www.baidu.com/", "10")));
        o.push(format!("  escape bucket  (google.com): HTTP {}  (000 = no answer: the tunnel is down, or slower than 12s)",
                       curl_code_t(&px, "https://www.google.com/", "12")));
        o.push(format!("  escape live     {}   (clash API {})",
                       lifecycle::clash_selected(ctx).unwrap_or_else(|| "?".into()), ctx.controller()));
    } else {
        o.push(format!("  (router stopped — start it: {} router up, then re-run diag)", crate::PROG));
    }
    o.push(String::new());

    o.push("----- router log (last 25 lines) -----".into());
    o.extend(tail(&ctx.host_log(), 25, "  ", "  (no log yet)"));
    o.push(String::new());
    o.push("----- audit trail: mutations, CLI + watchdog (last 30) -----".into());
    o.extend(tail(&cfg.join("log/audit.log"), 30, "  ", "  (no audit log yet)"));
    o.push(String::new());
    o.push("----- watch log (last 15) -----".into());
    o.extend(tail(&cfg.join("log/watch.log"), 15, "  ", "  (no watch log yet)"));
    o.push(String::new());
    o.push("===== end rowt diag =====".into());
    o.join("\n")
}

/// `$SHELL`, with bash's own fallback: "if it is not set when the shell starts,
/// bash assigns to it the full pathname of the current user's login shell". The
/// sandbox runs under `env -i`, so without this the report would print an empty
/// field where the shell prints /bin/zsh — and which rc file matters is exactly
/// what a reader of this report wants to know.
pub fn login_shell() -> String {
    if let Ok(s) = std::env::var("SHELL") {
        if !s.is_empty() {
            return s;
        }
    }
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() || (*pw).pw_shell.is_null() {
            return String::new();
        }
        std::ffi::CStr::from_ptr((*pw).pw_shell).to_string_lossy().into_owned()
    }
}

fn which_path(t: &str) -> String {
    std::env::var("PATH").unwrap_or_default().split(':')
        .map(|d| Path::new(d).join(t))
        .find(|c| c.is_file())
        .map(|c| c.display().to_string())
        .unwrap_or_default()
}

fn curl_code_t(proxy: &str, url: &str, t: &str) -> String {
    Command::new("curl")
        .args(["-sS", "-m", t, "-x", proxy, "-o", "/dev/null", "-w", "%{http_code}", url])
        .stderr(Stdio::null()).output().ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// `sed -E 's#(https?://.{10}).*#  \1…(masked)#'` — keep enough to recognise
/// which subscription it is, drop the token.
pub fn mask_url(l: &str) -> String {
    for scheme in ["https://", "http://"] {
        if let Some(i) = l.find(scheme) {
            let start = i + scheme.len();
            if l.len() >= start + 10 {
                return format!("  {}…(masked)", &l[..start + 10]);
            }
        }
    }
    l.to_string()
}

/// `via proxy: HTTP 000` names the symptom; this names whose fault it is — the
/// tunnel, a login page, or the network itself — because the fixes point three
/// ways and only the first is a rowt command. `_uplink_verdict`, word for word.
///
/// 2026-10-26, corp Wi-Fi: the Mac joined a subnet whose internet exit was
/// broken. Eight watchdog reloads and two hands-on restarts each brought the
/// router up and changed nothing — `restart` recreates sing-box and the system
/// proxy and leaves the Wi-Fi association, the lease and the routes, the parts
/// that were broken — and every line on the status screen said "tunnel".
///
/// Only run once the proxy check has failed. The two measurements are the
/// watchdog's own, with the router out of the path: the captive probe URL
/// fetched unproxied and bound to the physical NIC (6 s), then one echo at that
/// NIC's gateway — the same argv, so watch.log and this line describe one thing.
pub fn uplink_verdict(p: &impl Platform, mode: &str) -> String {
    let Some(ifc) = p.detect_iface() else {
        return "no physical interface is up — offline; the router has nothing to ride on".into();
    };
    let url = env_or("ROWT_CAPTIVE_URL", "http://captive.apple.com/hotspot-detect.html");
    let host = crate::watch::url_host(&url).unwrap_or_default();
    let out = Command::new("curl")
        .args(["-s", "--noproxy", "*", "--interface", &ifc, "--max-time", "6", "-o", "/dev/null", "-w", "%{http_code}", &url])
        .stderr(Stdio::null()).output();
    let (code, rc) = match out {
        Ok(o) => (String::from_utf8_lossy(&o.stdout).trim().to_string(), o.status.code()),
        Err(_) => (String::new(), None),
    };
    let reached = rc == Some(0) && !code.is_empty() && code != "000";
    // The gateway is only asked when the probe reached nothing — the shell
    // does the same, or the two would disagree about the argv.
    let gateway = if reached { None } else { Some(p.gateway(&ifc).map(|g| p.gateway_alive(&g))) };
    uplink_words(&ifc, &host, mode, reached.then_some(code.as_str()), rc, gateway)
}

/// The verdict itself, from what the probes said. `code` is the HTTP status
/// when the direct fetch reached something; `gateway` is `None` when it was not
/// asked, `Some(None)` when the NIC has no default route, `Some(Some(alive))`
/// otherwise. Pure, so the words are testable.
fn uplink_words(ifc: &str, host: &str, mode: &str, code: Option<&str>, rc: Option<i32>, gateway: Option<Option<bool>>) -> String {
    const PROG: &str = crate::PROG;
    if let Some(code) = code {
        return if (code.len() == 3 && code.starts_with("30") && code.as_bytes()[2].is_ascii_digit()) || code == "511" {
            format!("{ifc} reaches a login page, not the internet ({host} answered HTTP {code}) — a portal is in the way: {PROG} proxy off, log in, then {PROG} proxy on")
        } else if mode == "local" {
            format!("{ifc} reaches the internet on its own ({host} answered HTTP {code}); only google is unreachable direct from here (a firewall in the way, or google itself) — {PROG} up host puts a tunnel in the path")
        } else {
            format!("{ifc} reaches the internet on its own ({host} answered HTTP {code}), so the fault is the tunnel: {PROG} restart, or another server ({PROG} use best)")
        };
    }
    let why = match rc {
        Some(0) => "no answer".to_string(),
        Some(rc) => crate::watch::curl_why(rc),
        // The shell cannot fail to run curl at all; the nearest thing it
        // would report is the exit status it never got.
        None => crate::watch::curl_why(127),
    };
    match gateway {
        Some(None) => format!("{ifc} has no default route — the network is not up yet; nothing a {PROG} restart can change"),
        Some(Some(true)) => format!("{ifc} reaches its gateway but nothing beyond it ({host}: {why}) — the network's exit is broken, not the router; a {PROG} restart cannot fix this: rejoin the Wi-Fi or switch networks"),
        _ => format!("nothing answers over {ifc}, not even its gateway — the NETWORK is down, not the router; a {PROG} restart cannot fix this: rejoin the Wi-Fi or switch networks"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_subscription_token_never_reaches_the_report() {
        let u = "https://sub.example.com/link/SECRETTOKEN123456?target=clash";
        let m = mask_url(u);
        assert!(!m.contains("SECRETTOKEN"), "masked form still leaked the token: {m}");
        assert!(m.starts_with("  https://sub.e"), "{m}");
        assert!(m.ends_with("…(masked)"));
    }

    #[test]
    fn a_url_too_short_to_mask_is_left_alone() {
        // The sed only rewrites when 10 characters follow the scheme.
        assert_eq!(mask_url("https://a.b"), "https://a.b");
    }

    #[test]
    fn a_dead_first_hop_blames_the_network_not_the_router() {
        // 2026-10-26: curl timed out, netstat had a gateway, ping got nothing.
        let w = uplink_words("en0", "captive.apple.com", "host", None, Some(28), Some(Some(false)));
        assert!(w.starts_with("nothing answers over en0, not even its gateway — the NETWORK is down, not the router"), "{w}");
        assert!(w.contains("rowt restart cannot fix this: rejoin the Wi-Fi or switch networks"), "{w}");
    }

    #[test]
    fn a_live_gateway_with_no_exit_names_the_exit_and_the_curl_reason() {
        let w = uplink_words("en0", "captive.apple.com", "host", None, Some(28), Some(Some(true)));
        assert!(w.starts_with("en0 reaches its gateway but nothing beyond it (captive.apple.com: timed out)"), "{w}");
        assert!(w.contains("not the router"), "{w}");
    }

    #[test]
    fn a_reachable_internet_blames_the_tunnel_unless_there_is_none() {
        let w = uplink_words("en0", "captive.apple.com", "host", Some("200"), Some(0), None);
        assert!(w.contains("so the fault is the tunnel: rowt restart, or another server (rowt use best)"), "{w}");
        let l = uplink_words("en0", "captive.apple.com", "local", Some("200"), Some(0), None);
        assert!(l.contains("only google is unreachable direct from here"), "{l}");
    }

    #[test]
    fn a_redirect_or_511_is_a_portal() {
        for code in ["302", "307", "511"] {
            let w = uplink_words("en0", "captive.apple.com", "host", Some(code), Some(0), None);
            assert!(w.contains("a portal is in the way: rowt proxy off, log in, then rowt proxy on"), "{code}: {w}");
        }
        // 403 is a corp web filter as often as a portal: the internet answered.
        let w = uplink_words("en0", "captive.apple.com", "host", Some("403"), Some(0), None);
        assert!(w.contains("reaches the internet on its own"), "{w}");
    }

    #[test]
    fn no_default_route_is_a_network_still_coming_up() {
        let w = uplink_words("en0", "captive.apple.com", "host", None, Some(7), Some(None));
        assert_eq!(w, "en0 has no default route — the network is not up yet; nothing a rowt restart can change");
    }
}
