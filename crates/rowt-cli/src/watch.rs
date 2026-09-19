//! `rowt watch` — the LaunchAgent, and the tick it fires.
//!
//! The DECISIONS are not here: `rowt_core::watch` holds the state machine, in
//! two phases (`guard`, then `netcheck` after a settle), and 17 unit tests
//! replay DESIGN.md §11 against it. What lives here is the half that cannot be
//! pure — building the observation the FSM judges, and carrying out the Actions
//! it returns.
//!
//! That split is the whole point of Phase 3. Before it, testing "does a captive
//! portal drop the proxy" meant toggling the real system proxy.

use crate::lifecycle::{self, Ctx};
use crate::{die, env_or, read, PROG};
use rowt_core::watch::{guard, netcheck, Action, CaptiveState, Config, Next, Observation, State};
use rowt_platform::{Mac, Platform};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const LABEL: &str = "club.annaslife.rowt.watch";
pub const SUDOERS: &str = "/etc/sudoers.d/rowt";

fn plist_path() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join(format!("Library/LaunchAgents/{LABEL}.plist"))
}

fn out(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd).args(args).stderr(Stdio::null()).output().ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

fn quiet(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd).args(args).stdout(Stdio::null()).stderr(Stdio::null())
        .status().map(|s| s.success()).unwrap_or(false)
}

fn cfg_of(ctx: &Ctx) -> Config {
    Config {
        port: ctx.port,
        health_fails: env_or("ROWT_HEALTH_FAILS", "3").parse().unwrap_or(3),
        health_cooldown: env_or("ROWT_HEALTH_COOLDOWN", "600").parse().unwrap_or(600),
    }
}

fn watch_log_path(ctx: &Ctx) -> PathBuf {
    ctx.logdir().join("watch.log")
}

/// WHY a probe came back the way it did — the shell's `_captive_log`.
///
/// Its own file on purpose, and the reasons are structural rather than
/// stylistic: `watch.log` is what watch-diff reconstructs bash's ACTIONS from,
/// so a line there would have to be modelled in the planner for what is pure
/// telemetry; and the discovery journal's line is hashed into `discovery_sig`,
/// so changing that schema re-journals every tick. `log/` is pruned from the
/// parity snapshot, so this file is outside every compared surface.
fn captive_log(logdir: &Path, msg: &str) {
    use std::io::Write;
    let line = format!("{}  {msg}\n", crate::sh_date("+%Y-%m-%d %H:%M:%S"));
    let _ = std::fs::create_dir_all(logdir);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(logdir.join("captive.log")) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// curl's exit status is the discriminator worth keeping; the same few names
/// the shell gives them, so the two files read identically.
fn curl_why(rc: i32) -> String {
    match rc {
        6 => "could not resolve host".into(),
        7 => "could not connect".into(),
        28 => "timed out".into(),
        35 => "TLS handshake failed".into(),
        _ => format!("curl exit {rc}"),
    }
}

fn watch_log(ctx: &Ctx, msg: &str) {
    use std::io::Write;
    let line = format!("{}  {msg}\n", crate::sh_date("+%Y-%m-%d %H:%M:%S"));
    // The quiet half of the shell's bug: `create(true)` creates the FILE, not
    // its directory, and the `if let Ok` then swallowed the failure — so on a
    // config tree with no log/ yet the shell shouted and this side silently
    // dropped the line. Both now make the directory first.
    let _ = std::fs::create_dir_all(ctx.logdir());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(watch_log_path(ctx)) {
        let _ = f.write_all(line.as_bytes());
    }
}

// ------------------------------------------------------------------ install

/// The agent inherits none of your shell environment, so PATH is baked in and
/// the config-location + tuning variables THIS rowt was configured with are
/// passed through — otherwise the agent would render something different from
/// what you get at the prompt.
fn plist_body(ctx: &Ctx, self_bin: &Path) -> String {
    let mut path = self_bin.parent().unwrap_or(Path::new("/usr/bin")).display().to_string();
    let brew = out("brew", &["--prefix"]).trim().to_string();
    for d in [format!("{brew}/bin"), "/opt/homebrew/bin".into(), "/usr/local/bin".into(),
              "/usr/bin".into(), "/bin".into(), "/usr/sbin".into(), "/sbin".into()] {
        if d.is_empty() || d == "/bin" && brew.is_empty() && false {
            continue;
        }
        if !path.split(':').any(|p| p == d) {
            path.push(':');
            path.push_str(&d);
        }
    }
    let mut envxml = format!("    <key>PATH</key><string>{path}</string>");
    for v in ["XDG_CONFIG_HOME", "ROWT_PORT", "ROWT_CLASH_PORT", "ROWT_FINAL", "ROWT_IFACE",
              "ROWT_DNS_DIRECT", "ROWT_DNS_LOCAL", "ROWT_GFW_CANARIES", "ROWT_GFW_TIMEOUT",
              "SINGBOX_VERSION", "ROWT_LOG_LEVEL", "ROWT_WATCH_INTERVAL", "ROWT_HEALTH_FAILS",
              "ROWT_HEALTH_COOLDOWN", "ROWT_HEALTH_TIMEOUT", "ROWT_HEALTH_URL",
              // The captive knobs reach the agent too. They did not until
              // 3.5.0, so `ROWT_CAPTIVE_CHECK=0` — which DESIGN.md §11 offers
              // as the way to switch portal handling off — did nothing unless
              // you hand-edited the plist.
              "ROWT_CAPTIVE_CHECK", "ROWT_CAPTIVE_URL", "ROWT_CAPTIVE_TIMEOUT", "ROWT_CAPTIVE_RETRY",
              "ROWT_CAPTIVE_FALLBACK",
              "ROWT_WATCH_SHADOW", "ROWT_RENDER_SHADOW"] {
        if let Ok(val) = std::env::var(v) {
            if !val.is_empty() {
                envxml.push_str(&format!("\n    <key>{v}</key><string>{val}</string>"));
            }
        }
    }
    let interval = env_or("ROWT_WATCH_INTERVAL", "120");
    format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}</string>
    <string>watch</string>
    <string>tick</string>
  </array>
  <key>WatchPaths</key>
  <array>
    <string>/etc/resolv.conf</string>
    <string>/var/run/resolv.conf</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
{envxml}
  </dict>
  <!-- CRITICAL: a watchdog tick that reloads launches sing-box as its child.
       Without this, launchd SIGKILLs the tick's whole process group when the tick
       exits — killing the router the reload just started (network switch / corp
       VPN up → router silently down). Abandon the group so sing-box outlives the tick. -->
  <key>AbandonProcessGroup</key><true/>
  <key>RunAtLoad</key><true/>  <!-- also fire once at login: clear a stale proxy rowt left set -->
  <key>StartInterval</key><integer>{interval}</integer>  <!-- periodic liveness poll (network changes come via WatchPaths, instantly) -->

  <key>ThrottleInterval</key><integer>5</integer>
  <key>StandardOutPath</key><string>{}</string>
  <key>StandardErrorPath</key><string>{}</string>
</dict>
</plist>
"#, self_bin.display(), watch_log_path(ctx).display(), watch_log_path(ctx).display())
}

/// The scoped passwordless rule: only the three proxy-state toggles, only for
/// this user. Broad NOPASSWD would be a far bigger grant than the watchdog needs.
fn sudoers_body() -> String {
    sudoers_for(out("id", &["-un"]).trim())
}

/// Split from the `id` call so the GRANT can be asserted without running as
/// somebody. This is a passwordless-root rule: the seven verbs it names, and
/// the fact that it names verbs rather than a whole binary, are the whole of
/// what keeps it narrow.
fn sudoers_for(user: &str) -> String {
    // One line, all seven verbs, exactly as the shell emits it — `visudo -cf`
    // validates this before it is installed, and a rule that differs from the
    // shell's is a rule that grants something different.
    format!("# Installed by 'rowt watch install'. Lets the rowt auto-reload LaunchAgent\n\
             # re-apply the macOS system proxy on a network change without a password prompt.\n\
             # Remove with:  rowt watch uninstall\n\
             {user} ALL=(root) NOPASSWD: /usr/sbin/networksetup -setsocksfirewallproxy *, /usr/sbin/networksetup -setsocksfirewallproxystate *, /usr/sbin/networksetup -setwebproxy *, /usr/sbin/networksetup -setwebproxystate *, /usr/sbin/networksetup -setsecurewebproxy *, /usr/sbin/networksetup -setsecurewebproxystate *, /usr/sbin/networksetup -setproxybypassdomains *\n")
}

fn uid() -> String {
    out("id", &["-u"]).trim().to_string()
}

// ------------------------------------------------------------------ observing

/// `_captive_state` — is a walled garden in the way, and where is its page?
///
/// Apple's probe answers 200 with a body containing "Success". A 200 with
/// anything ELSE is a portal serving its login page under the real URL, a
/// 3xx is a portal redirecting, and a 511 is one saying so in as many words
/// (RFC 6585). Anything else — including no answer at all — is `unknown`, and
/// unknown means hands-off: acting on a guess here would drop the proxy on a
/// flaky network. A 403 is deliberately unknown: a corp web filter answers the
/// probe with one too, and acting on it would hold the proxy off all day.
///
/// With `captive`, the second half is the portal's page: the redirect target
/// when there was one, else the probe URL (a portal serving its page under
/// that URL answers the browser the same way).
///
/// `iface` is the physical interface (None: offline); `moved` says the network
/// just changed (net_id differs from the one netcheck last wrote).
///
/// The probe host is resolved at the NIC's DHCP resolver — the hotspot's own
/// DNS, where the hijack lives; the system resolver is not that server once a
/// VPN is up or the user pinned one, and a probe resolved elsewhere sails past
/// the hijack and reads "clear" from inside a walled garden — and pinned with
/// `--resolve` (macOS curl has no --dns-servers). No resolver, no answer, or an
/// IP-literal probe URL means the plain probe, exactly as before.
///
/// Right after a network change an `unknown` — the link still settling, the
/// portal's DNS not yet answering — is re-probed after `ROWT_CAPTIVE_RETRY`
/// seconds each, so the drop lands on THIS tick and not on the next timer tick
/// two minutes later. Only then: mid-episode and in steady state one probe per
/// tick is the budget, and `unknown` there is the hands-off answer it always
/// was. Silent on purpose — a log line here would be an action the planner
/// never took.
fn captive_state(log: &Path, iface: Option<&str>, moved: bool) -> (CaptiveState, Option<String>) {
    if env_or("ROWT_CAPTIVE_CHECK", "1") != "1" {
        // Deliberately unlogged: this is a configuration fact, not a probe
        // outcome, and a line per tick would bury the ones that matter.
        return (CaptiveState::Unknown, None);
    }
    let url = env_or("ROWT_CAPTIVE_URL", "http://captive.apple.com/hotspot-detect.html");
    let t = env_or("ROWT_CAPTIVE_TIMEOUT", "6");
    let named = probe_host_port(&url);
    let pin = || -> Option<String> {
        let (host, port) = named.as_ref()?;
        let ns = Mac.dhcp_dns(iface.unwrap_or(""))?;
        let ip = Mac.resolve_at(&ns, host)?;
        Some(format!("{host}:{port}:{ip}"))
    };
    let mut resolve = pin();
    // `${ROWT_CAPTIVE_RETRY-5,10}`: unset means the default, set-but-empty
    // means no retry (the parity sandbox says so, or every unknown would cost
    // it fifteen seconds).
    let delays = if moved {
        retry_delays(&std::env::var("ROWT_CAPTIVE_RETRY").unwrap_or_else(|_| "5,10".into()))
    } else {
        Vec::new()
    };
    let mut r = probe_once(log, "first", &url, &t, resolve.as_deref());
    for d in delays {
        if r.0 != CaptiveState::Unknown {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(d));
        // The resolver may be what was not answering yet: a probe that could
        // not be pinned tries again to pin itself, or the burst would re-probe
        // via the system resolver — the case the pin exists for.
        if resolve.is_none() {
            resolve = pin();
        }
        r = probe_once(log, &format!("retry+{d}s"), &url, &t, resolve.as_deref());
    }
    // Still nothing, and the probe needed a name to get anywhere: ask again
    // without DNS. Only reached when the named probe failed, so a network where
    // names resolve pays nothing for this.
    if r.0 == CaptiveState::Unknown {
        if let Some((host, _)) = named.as_ref() {
            if let Some(fb) = probe_ip_fallback(log, host, &t) {
                r = fb;
            }
        }
    }
    r
}

/// curl's `--write-out` format, and it is a LITERAL backslash-n, not a newline.
///
/// The shell writes it inside single quotes (bin/rowt, `_captive_probe_once`),
/// so curl receives `\` `n` and expands the escape itself. Passing a real
/// newline here produces byte-identical OUTPUT — curl expands one and passes
/// the other through — but a different argv, and the parity harness compares
/// the argv the two implementations produce. Keep the bytes the shell sends.
const PROBE_W: &str = "\\n%{http_code}\\nredirect=%{redirect_url}";

/// The exact argv for one probe, so it can be asserted in a test rather than
/// only observed through the harness. `resolve` is curl's `host:port:ip`.
fn probe_args<'a>(url: &'a str, timeout: &'a str, resolve: Option<&'a str>) -> Vec<&'a str> {
    let mut args: Vec<&str> = vec!["-s", "--noproxy", "*", "--max-time", timeout];
    if let Some(r) = resolve {
        args.extend(["--resolve", r]);
    }
    args.extend(["-w", PROBE_W, url]);
    args
}

/// One probe. `resolve` is curl's `host:port:ip`, placed right before `-w`.
fn probe_once(log: &Path, label: &str, url: &str, timeout: &str, resolve: Option<&str>) -> (CaptiveState, Option<String>) {
    let args = probe_args(url, timeout, resolve);
    let rsv = resolve.map(|r| format!(" resolve={r}")).unwrap_or_default();
    let o = Command::new("curl").args(&args).stderr(Stdio::null()).output();
    let Ok(o) = o else {
        captive_log(log, &format!("unknown   {label}: could not run curl url={url}{rsv}"));
        return (CaptiveState::Unknown, None);
    };
    if !o.status.success() {
        // The status is the whole point: 6 is DNS, 7 a black hole, 28 a
        // timeout, and the shell used to throw all three away as `unknown`.
        let rc = o.status.code().unwrap_or(-1);
        captive_log(log, &format!("unknown   {label}: {} (rc={rc}) url={url}{rsv}", curl_why(rc)));
        return (CaptiveState::Unknown, None);
    }
    let v = probe_verdict(&String::from_utf8_lossy(&o.stdout), url);
    // A clear verdict needs no explaining; everything else is what a later
    // reader is trying to account for.
    if v.0 != CaptiveState::Clear {
        let (code, _) = split_probe(&String::from_utf8_lossy(&o.stdout));
        captive_log(log, &format!("{}   {label}: HTTP {code} url={url}{rsv}", v.0.as_str()));
    }
    v
}

/// The probe URL's `host` and `port` for `--resolve` — None when there is
/// nothing to resolve: no host, an IPv6 literal, or a dotted-quad literal
/// (the sandbox's `127.0.0.1:8099`, and any `ROWT_CAPTIVE_URL` pinned by IP).
/// The shell: `${url#*://}` up to the first `/`, split at the colon, else
/// 443 for https and 80 otherwise; a name is `*[!0-9.]*`.
fn probe_host_port(url: &str) -> Option<(String, String)> {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let hp = rest.split('/').next().unwrap_or("");
    let (host, port) = match hp.split_once(':') {
        Some((h, p)) => (h.to_string(), p.rsplit(':').next().unwrap_or("").to_string()),
        None => (hp.to_string(), if url.starts_with("https://") { "443" } else { "80" }.to_string()),
    };
    if host.is_empty() || host.starts_with('[') || host.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    Some((host, port))
}

/// The DNS-free probe, for the garden that will not resolve anything until you
/// log in. A portal's gateway intercepts port 80 for an unauthenticated client
/// WHATEVER the destination, so this needs no name lookup at all — which is the
/// entire point. Observed 2026-09-14: a hotel refused every lookup pre-login,
/// so the named probe never sent a request and the verdict sat at `unknown` for
/// six minutes while the portal was ready to redirect.
///
/// The target is TEST-NET-1 (RFC 5737), reserved for documentation and never
/// routed, so on a healthy network NOTHING can answer it — that is what makes
/// it safe to act on. And only a REDIRECT counts: without DNS a 200 from an
/// unidentified box is evidence of nothing, and acting on it would drop the
/// system proxy on a network that was merely slow.
fn probe_ip_fallback(log: &Path, host: &str, timeout: &str) -> Option<(CaptiveState, Option<String>)> {
    let url = env_or("ROWT_CAPTIVE_FALLBACK", "http://192.0.2.1/");
    let hostarg = format!("Host: {host}");
    let mut args: Vec<&str> = vec!["-s", "--noproxy", "*", "--max-time", timeout];
    if !host.is_empty() {
        args.extend(["-H", hostarg.as_str()]);
    }
    args.extend(["-w", PROBE_W, &url]);
    let o = Command::new("curl").args(&args).stderr(Stdio::null()).output().ok()?;
    if !o.status.success() {
        let rc = o.status.code().unwrap_or(-1);
        captive_log(log, &format!("unknown   dns-free: {} (rc={rc}) url={url}", curl_why(rc)));
        return None;
    }
    let (code, redir) = split_probe(&String::from_utf8_lossy(&o.stdout));
    let portal = code == "511" || (code.len() == 3 && code.starts_with("30"));
    if !portal {
        // "the fallback ran and declined" and "the fallback never ran" look the
        // same from the outside and mean very different things.
        captive_log(log, &format!("unknown   dns-free: HTTP {code} — not a redirect, verdict unchanged url={url}"));
        return None;
    }
    // Only a real redirect target is worth opening: the fallback URL itself is
    // an address nobody can reach, so the browser would get a blank tab.
    Some((CaptiveState::Captive, (!redir.is_empty()).then(|| redir)))
}

/// `ROWT_CAPTIVE_RETRY`: seconds, comma- (or space-) separated. A token that
/// is not a whole number is skipped, as the shell's `case` skips it — never
/// a `sleep` that fails and takes the tick with it.
fn retry_delays(spec: &str) -> Vec<u64> {
    spec.split(|c: char| c == ',' || c.is_whitespace()).filter_map(|t| t.parse().ok()).collect()
}

/// `jq -e '.delay // empty'` on the clash delay-test's answer.
///
/// `//` falls through on `null` and `false` ONLY, and `-e` fails on those two
/// and on no output at all. A delay of 0 is a number jq is perfectly happy
/// with, so it counts as answering — which matters, because the alternative
/// reading (0 is falsy, as in C or Python) would call a perfectly live tunnel
/// wedged and trigger a recovery every tick.
fn delay_answered(out: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(out).ok()
        .and_then(|v| v.get("delay").cloned())
        .is_some_and(|d| !matches!(d, serde_json::Value::Null | serde_json::Value::Bool(false)))
}

/// The verdict and the page, given what `curl -w '\n%{http_code}\nredirect=%{redirect_url}'`
/// wrote for `url`.
///
/// Split from the call so it can be tested: this is the decision that drops
/// the system proxy, and the alternative to a test is toggling a real portal.
/// Peel curl's `-w` tail off the body: the redirect line, then the code. Shared
/// so the fallback cannot drift from the main probe's parsing.
fn split_probe(body: &str) -> (String, String) {
    let body = body.trim_end_matches('\n');
    let split = |s: &str| -> (String, String) {
        match s.rfind('\n') {
            Some(i) => (s[..i].to_string(), s[i + 1..].to_string()),
            None => (String::new(), s.to_string()),
        }
    };
    let (rest, redir) = split(body);
    let redir = redir.strip_prefix("redirect=").map(str::to_string).unwrap_or(redir);
    let (_payload, code) = split(&rest);
    (code.trim().to_string(), redir)
}

fn probe_verdict(body: &str, url: &str) -> (CaptiveState, Option<String>) {
    // `$(…)` strips trailing newlines, then `${out##*$'\n'}` / `${out%$'\n'*}`
    // peel the LAST line twice — the redirect, then the code — because the
    // body itself contains plenty of newlines. The redirect line carries a
    // prefix so that an empty redirect is still a line and not a trailing
    // newline the substitution would have eaten.
    let (code, redir) = split_probe(body);
    // The body matters only for the 200 case, and only to tell Apple's Success
    // page from a portal serving its own under the same URL.
    let payload = body;
    let code = code.as_str();
    let verdict = match code {
        "200" if payload.contains("Success") => CaptiveState::Clear,
        "200" => CaptiveState::Captive,
        c if c.len() == 3 && c.starts_with("30") => CaptiveState::Captive,
        "511" => CaptiveState::Captive,
        _ => CaptiveState::Unknown,
    };
    let page = (verdict == CaptiveState::Captive)
        .then(|| if redir.is_empty() { url.to_string() } else { redir });
    (verdict, page)
}

/// `net_id` — a signature for "which network am I on", so a home→hotspot move
/// on the SAME interface still reads as a move.
fn net_id(iface: &str) -> String {
    if iface.is_empty() {
        return String::new();
    }
    let addr = out("ipconfig", &["getifaddr", iface]).trim().to_string();
    let router = out("ipconfig", &["getoption", iface, "router"]).trim().to_string();
    let ssid = out("networksetup", &["-getairportnetwork", iface]);
    let ssid = ssid.rsplit(": ").next().unwrap_or("").trim().to_string();
    format!("{ssid} {addr}/{router}").trim().to_string()
}

fn observe(ctx: &Ctx, cap: Option<CaptiveState>, portal: Option<String>, health_ok: bool) -> Observation {
    let p = Mac;
    let svc = p.active_service();
    let iface = p.detect_iface();
    let bound = serde_json::from_str::<serde_json::Value>(&read(&ctx.host_cfg())).ok()
        .and_then(|v| v.get("outbounds").and_then(|o| o.as_array()).map(|a| a.iter()
            .find(|o| o.get("tag").and_then(|t| t.as_str()) == Some("direct"))
            .and_then(|o| o.get("bind_interface").and_then(|b| b.as_str()).map(|s| s.to_string()))))
        .flatten();
    Observation {
        proxy_intent: ctx.sget("proxy_intent"),
        captive: cap,
        // With the check off every verdict is `unknown` by construction, so the
        // planner must be told the difference between "we looked and could not
        // reach anything" and "we never looked".
        captive_check_disabled: env_or("ROWT_CAPTIVE_CHECK", "1") != "1",
        // Only probed when it could matter: `hold_for_network` short-circuits
        // on a clear verdict, so a healthy tick pays no ping for this. The
        // shell applies the same condition, or the two would disagree about
        // the argv a tick produces.
        gateway_ok: !health_ok
            && cap != Some(CaptiveState::Clear)
            && iface.as_deref().and_then(|i| p.gateway(i)).map(|gw| p.gateway_alive(&gw)).unwrap_or(false),
        portal_url: portal,
        proxy_any_on: svc.as_ref().map(|s| p.proxy_any_on(s)).unwrap_or(false),
        proxy_pointing_ok: svc.as_ref().map(|s| p.proxy_pointing_ok(s, ctx.port)).unwrap_or(false),
        proxy_bypass_ok: svc.as_ref().map(|s| rowt_platform::bypass_ok(s, &crate::hotspot_bypass(&ctx.cfg))).unwrap_or(false),
        active_service: svc,
        host_running: lifecycle::host_running(ctx).is_some(),
        intent: ctx.sget("intent"),
        boot_matches: ctx.sget("boot") == p.boot_id().unwrap_or_default(),
        net_id: net_id(iface.as_deref().unwrap_or("")),
        iface,
        bound_iface: bound,
        mode: ctx.mode(),
        health_ok,
        now: crate::sh_date("+%s").parse().unwrap_or(0),
    }
}

/// The recovery-cooldown stamp. `watch.restart` is the name the SHELL uses
/// (`WATCH_RESTART`); this read `watch.recovery` for three releases, a file
/// bin/rowt never writes — so a native tick always loaded 0 and the
/// `ROWT_HEALTH_COOLDOWN` gate could never hold it back.
/// (c)'s timestamp, beside `watch.restart` and for the same reason: "recent"
/// has to outlive the tick that observed it.
fn netchange_file(ctx: &Ctx) -> PathBuf {
    ctx.cfg.join("watch.netchange")
}

fn restart_file(ctx: &Ctx) -> PathBuf {
    ctx.cfg.join("watch.restart")
}
fn health_file(ctx: &Ctx) -> PathBuf {
    ctx.cfg.join("watch.health")
}

fn load_state(ctx: &Ctx) -> State {
    State {
        captive_flag: ctx.sget("captive") == "1",
        health_fails: read(&health_file(ctx)).trim().parse().unwrap_or(0),
        last_net_id: {
            let n = read(&ctx.cfg.join("watch.net")).trim().to_string();
            if n.is_empty() { None } else { Some(n) }
        },
        last_recovery: read(&restart_file(ctx)).trim().parse().unwrap_or(0),
        last_net_change: read(&netchange_file(ctx)).trim().parse().unwrap_or(0),
    }
}

/// Persist the two counters and the captive flag — but only where they CHANGED
/// since `disk` was read, because that is what the shell does and the config
/// tree is compared byte for byte.
///
/// The shell has no `save_state`: it writes each value at the site that changes
/// it, so a tick that changes nothing leaves no trace. Writing unconditionally
/// looks harmless and is not. `sset` appends the key at the end of `state`, so
/// re-writing an unchanged value MOVES it and the file differs; a `0` streak
/// written where the shell `rm`s the file leaves a file the shell never
/// creates; and, worst, `Action::Reload` deletes `watch.health` to clear the
/// streak — an unconditional save immediately wrote the pre-reload count back,
/// so the next single failure re-entered recovery instead of counting 1/3.
///
/// Lossless by construction: `load_state` rebuilds every field from disk at the
/// top of each tick, so a field that matches what is already there has nothing
/// to save. `disk` tracks what this tick has written, since the tick saves
/// twice (after the guard, then after netcheck).
fn save_state(ctx: &Ctx, disk: &mut State, new: &State) {
    if new.captive_flag != disk.captive_flag {
        lifecycle::sset(ctx, "captive", if new.captive_flag { "1" } else { "" });
        disk.captive_flag = new.captive_flag;
    }
    if new.health_fails != disk.health_fails {
        // `rm -f "$WATCH_HEALTH"` (bin/rowt) — a cleared streak is an ABSENT
        // file, not a zero.
        if new.health_fails == 0 {
            let _ = std::fs::remove_file(health_file(ctx));
        } else {
            let _ = std::fs::write(health_file(ctx), format!("{}\n", new.health_fails));
        }
        disk.health_fails = new.health_fails;
    }
    if new.last_net_change != disk.last_net_change {
        let _ = std::fs::write(netchange_file(ctx), format!("{}\n", new.last_net_change));
        disk.last_net_change = new.last_net_change;
    }
    if new.last_recovery != disk.last_recovery {
        let _ = std::fs::write(restart_file(ctx), format!("{}\n", new.last_recovery));
        disk.last_recovery = new.last_recovery;
    }
}

// ------------------------------------------------------------------ effects

/// `perform`, minus the two actions the tick has already carried out at the
/// instant the shell carries them out: the discovery journal (before the
/// observation) and corp_sync (before the netcheck observation). Both stay in
/// the plan — the shadow compares plans — so the skip lives here rather than
/// in the FSM.
fn perform_planned(ctx: &Ctx, actions: &[Action]) {
    for a in actions {
        if matches!(a, Action::Journal(_) | Action::CorpSync) {
            continue;
        }
        perform(ctx, std::slice::from_ref(a));
    }
}

fn perform(ctx: &Ctx, actions: &[Action]) {
    for a in actions {
        match a {
            Action::Log(m) => watch_log(ctx, m),
            Action::Audit(m) => crate::shell::audit(&ctx.cfg, m),
            Action::Journal(cap) => journal(ctx, *cap),
            Action::CaptiveProxyOff(svc) => {
                if Mac.proxy_states_off(svc, true).is_err() {
                    watch_log(ctx, &format!("captive: could not drop the proxy (sudoers missing?) — log in after a manual '{PROG} proxy off'"));
                }
            }
            Action::CaptiveProxyOn(svc) => {
                let _ = Mac.proxy_states_on(svc, true);
            }
            // `open` lands in the user's session (the LaunchAgent is an Aqua
            // agent), so the browser gets the page the OS never got to ask for.
            Action::OpenPortal(u) => {
                let ok = Command::new("open").arg(u).stdout(Stdio::null()).stderr(Stdio::null())
                    .status().map(|s| s.success()).unwrap_or(false);
                if !ok {
                    watch_log(ctx, &format!("captive: could not open the browser for {u}"));
                }
            }
            Action::ClearStaleProxy(svc) => {
                let _ = Mac.proxy_states_off(svc, true);
            }
            // Both of these are the real `cmd_reload`, with its output
            // appended to the watch log the way the shell redirects it. Not an
            // inlined render-stop-start: the watchdog's whole job is to leave
            // the machine in the state a hands-on reload would, and that
            // includes the guard it refuses on, the vm branch, and the three
            // state stamps (`proxy_intent`, `intent`, `boot`) the NEXT tick
            // reads to decide whether any of this was deliberate.
            Action::Recover(reason) => {
                // The audit lines bracket the reload and name the watchdog as
                // the actor: "what changed the system, when, and who did it" is
                // the question the audit log exists to answer, and a recovery
                // that looks like a hands-on reload is the exact ambiguity that
                // made the last incident hard to read.
                crate::shell::audit(&ctx.cfg, &format!("BEGIN watchdog recover: cmd_reload — {reason}"));
                let _ = crate::redirected(&watch_log_path(ctx), || lifecycle::cmd_reload(ctx, &crate::here_dir()));
                // Then ASK, rather than believe the return value: a reload can
                // report success and still leave a tunnel that does not carry
                // traffic, which is the failure this whole path exists for.
                std::thread::sleep(std::time::Duration::from_secs(2));
                let up = lifecycle::host_running(ctx).is_some();
                if up && health_ok(ctx) {
                    watch_log(ctx, "recovery ok — escape tunnel answering");
                    crate::shell::audit(&ctx.cfg, "END   watchdog recover: cmd_reload — ok (tunnel answering)");
                } else {
                    let cool = env_or("ROWT_HEALTH_COOLDOWN", "600");
                    watch_log(ctx, &format!(
                        "recovery INCOMPLETE — router {}, tunnel still not answering (retry after {cool}s)",
                        if up { "up" } else { "DOWN" }));
                    crate::shell::audit(&ctx.cfg, &format!(
                        "END   watchdog recover: cmd_reload — INCOMPLETE (router {})",
                        if up { "up" } else { "down" }));
                }
            }
            Action::Reload(_) => {
                // No BEGIN here: the planner already emitted it as its own
                // `Action::Audit`, so writing one would log the line twice — and
                // this copy hardcoded "network change", which is now not even the
                // only reason a reload happens.
                let r = crate::redirected(&watch_log_path(ctx), || lifecycle::cmd_reload(ctx, &crate::here_dir()));
                if r.is_ok() {
                    watch_log(ctx, "reload ok");
                    let _ = std::fs::remove_file(ctx.cfg.join("watch.health"));
                    crate::shell::audit(&ctx.cfg, "END   watchdog reload — ok");
                } else {
                    watch_log(ctx, "reload FAILED");
                    crate::shell::audit(&ctx.cfg, "END   watchdog reload — FAILED");
                }
            }
            Action::CorpSync => {
                let _ = crate::corp::sync(ctx, true);
            }
            Action::WriteNetId(n) => {
                let _ = std::fs::write(ctx.cfg.join("watch.net"), format!("{n}\n"));
            }
        }
    }
}

/// The discovery journal (`_discovery_journal`): one JSONL line in
/// log/discovery.log each time what the network and VPN advertise CHANGES.
/// In steady state it writes nothing, which is what makes it readable months
/// later.
///
/// This used to be a different journal rather than a port of that one — a
/// pipe-separated line built from raw `scutil --dns` greps, timestamped
/// without the zone, deduped against the last line of the file instead of the
/// `discovery_sig` state key, and with no `vpn_iface` field at all. Two
/// implementations were writing one log in two schemas, and nothing could read
/// the result. The shape below is the shell's, field for field:
///
///   {"net":…,"vpn_iface":…,"captive":…,"dhcp":[…],"scoped":[…],"ns":[…]}
///
/// `dhcp` is what DHCP advertised (`physical_search`), `scoped` is the internal
/// domains that did NOT come from DHCP — jq's `(.internal_domains // []) -
/// (.physical_search // [])`, which preserves the left order — and `ns` is the
/// corp nameservers. Dedupe is a POSIX `cksum` of those bytes, compared against
/// `discovery_sig` in `state`, so a rotated or truncated log does not make the
/// next tick re-journal a network that has not changed.
fn journal(ctx: &Ctx, cap: CaptiveState) {
    if env_or("ROWT_DISCOVERY_LOG", "1") != "1" {
        return;
    }
    let d = rowt_core::netdetect::parse(&out("scutil", &["--dns"]));
    let nid = net_id(&Mac.detect_iface().unwrap_or_default());
    let vpn = crate::corp::vpn_iface();
    let scoped: Vec<&String> = d.internal_domains.iter().filter(|x| !d.physical_search.contains(x)).collect();
    let line = serde_json::json!({
        "net": nid,
        "vpn_iface": vpn,
        "captive": cap.as_str(),
        "dhcp": d.physical_search,
        "scoped": scoped,
        "ns": d.corp_nameservers,
    })
    .to_string();
    let sig = rowt_core::cksum::posix(line.as_bytes()).to_string();
    if ctx.sget("discovery_sig") == sig {
        return;
    }
    lifecycle::sset(ctx, "discovery_sig", &sig);
    let dir = ctx.logdir();
    let _ = std::fs::create_dir_all(&dir);
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("discovery.log")) {
        let _ = writeln!(f, "{} {line}", crate::sh_date("+%Y-%m-%dT%H:%M:%S%z"));
    }
}

/// `_watch_probe` — is the ESCAPE tunnel actually carrying traffic? The
/// self-heal for a wedge that does not move the network: a server-side
/// connection death, a stuck UDP socket. A change-triggered reload cannot see
/// those.
///
/// Through the clash API's delay test (local → selected escape server →
/// target), NOT an HTTP request through the mixed proxy. The mixed-proxy probe
/// is the obvious implementation and the wrong one: its target is routed by the
/// normal rules, so on a censored network it usually goes DIRECT, and a flaky
/// direct-to-CDN path then reads as a wedged tunnel and triggers a recovery
/// that fixes nothing. The delay test forces the traffic through the escape
/// server regardless of routing. Two tries, so one dropped packet is not a
/// verdict.
fn health_ok(ctx: &Ctx) -> bool {
    let Some(ep) = lifecycle::controller(ctx) else { return false };   // API gone = wedged
    let secret = lifecycle::clash_secret(ctx);
    let sel = { let s = ctx.sget("selected"); if s.is_empty() { "auto".into() } else { s } };
    let url = env_or("ROWT_HEALTH_URL", "https://www.gstatic.com/generate_204");
    let t: u32 = env_or("ROWT_HEALTH_TIMEOUT", "8").parse().unwrap_or(8);
    // `python3 -c 'urllib.parse.quote(u, safe="")'` — in-process, same rules.
    let enc = rowt_core::pyurl::quote(&url, "");
    for try_n in 1..=2 {
        // curl's max-time must exceed the clash delay timeout or it cuts the
        // test short and a slow-but-live tunnel reads as dead.
        let out = Command::new("curl")
            .args(["--noproxy", "*", "-sS", "-m", &(t + 3).to_string(),
                   "-H", &format!("Authorization: Bearer {secret}"),
                   &format!("http://{ep}/proxies/{sel}/delay?timeout={}000&url={enc}", t)])
            .stderr(Stdio::null()).output().ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        if delay_answered(&out) {
            return true;
        }
        if try_n == 1 {
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    }
    false
}

// ------------------------------------------------------------------ command

pub fn cmd(ctx: &Ctx, self_bin: &Path, action: &str) -> Result<String, String> {
    let cfg = &ctx.cfg;
    let plist = plist_path();
    match action {
        "install" | "refresh" => {
            let refresh = action == "refresh";
            // An explicitly-set shadow choice is remembered in state, because
            // the post-upgrade `watch refresh` runs with an EMPTY environment —
            // an env-only flag would quietly end a shadow window at the next
            // brew upgrade, and the silence would look like agreement.
            for (env, key) in [("ROWT_WATCH_SHADOW", "shadow_watch"), ("ROWT_RENDER_SHADOW", "shadow_render")] {
                if let Ok(v) = std::env::var(env) {
                    if !v.is_empty() {
                        lifecycle::sset(ctx, key, &v);
                    }
                }
            }
            if refresh && !plist.is_file() {
                eprintln!("==> watch not installed — nothing to refresh");
                return Ok(String::new());
            }
            if let Some(d) = plist.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            std::fs::write(&plist, plist_body(ctx, self_bin)).map_err(|e| e.to_string())?;
            if !refresh {
                let tmp = std::env::temp_dir().join(format!("rowt-sudoers-{}", std::process::id()));
                std::fs::write(&tmp, sudoers_body()).map_err(|e| e.to_string())?;
                if quiet("sudo", &["visudo", "-cf", &tmp.to_string_lossy()]) {
                    eprintln!("==> installing scoped passwordless-sudo rule ({SUDOERS}; needs admin once)");
                    if !quiet("sudo", &["install", "-m", "440", "-o", "root", "-g", "wheel",
                                        &tmp.to_string_lossy(), SUDOERS]) {
                        eprintln!("error: could not install {SUDOERS} — a Wi-Fi<->Ethernet switch may prompt for a password");
                    }
                } else {
                    eprintln!("error: generated sudoers failed validation — skipping it (service-change reloads may prompt)");
                }
                let _ = std::fs::remove_file(&tmp);
            }
            let u = uid();
            let _ = quiet("launchctl", &["bootout", &format!("gui/{u}/{LABEL}")]);
            if !quiet("launchctl", &["bootstrap", &format!("gui/{u}"), &plist.to_string_lossy()])
                && !quiet("launchctl", &["load", "-w", &plist.to_string_lossy()])
            {
                die(cfg, &format!("could not load the LaunchAgent — try: launchctl bootstrap gui/{u} {}", plist.display()));
            }
            if refresh {
                eprintln!("==> watch refreshed for rowt {} (agent reloaded).", env!("ROWT_SHELL_VERSION"));
                return Ok(String::new());
            }
            eprintln!("==> watch installed — auto-reload on network changes + liveness watchdog (recovers a wedged OR crashed tunnel).");
            let c = cfg_of(ctx);
            Ok(format!("  agent:  {}\n  probe:  escape delay-test every {}s; auto-recover after {} failures (>= {}s apart)\n  log:    {}\n  status: {PROG} watch status   ·   remove: {PROG} watch uninstall",
                plist.display(), env_or("ROWT_WATCH_INTERVAL", "120"), c.health_fails, c.health_cooldown,
                watch_log_path(ctx).display()))
        }
        "uninstall" => {
            let u = uid();
            if !quiet("launchctl", &["bootout", &format!("gui/{u}/{LABEL}")]) {
                let _ = quiet("launchctl", &["unload", &plist.to_string_lossy()]);
            }
            let _ = std::fs::remove_file(&plist);
            if Path::new(SUDOERS).is_file() {
                eprintln!("==> removing {SUDOERS} (needs admin)");
                if !quiet("sudo", &["rm", "-f", SUDOERS]) {
                    eprintln!("error: could not remove {SUDOERS} — delete it by hand");
                }
            }
            eprintln!("==> watch uninstalled.");
            Ok(String::new())
        }
        "status" => {
            let mut o = Vec::new();
            o.push(if quiet("launchctl", &["list", LABEL]) {
                format!("watch: LOADED ({LABEL})")
            } else if plist.is_file() {
                format!("watch: installed but NOT loaded — '{PROG} watch install' to (re)load")
            } else {
                format!("watch: not installed — '{PROG} watch install' to enable auto-reload")
            });
            o.push(format!("  agent:   {}", if plist.is_file() { plist.display().to_string() } else { "(none)".into() }));
            o.push(format!("  sudoers: {}", if Path::new(SUDOERS).is_file() {
                format!("{SUDOERS} (passwordless proxy toggles)")
            } else { "(none — service-change reloads may prompt)".into() }));
            let wl = watch_log_path(ctx);
            if wl.is_file() {
                o.push("  recent:".into());
                let b = read(&wl);
                let lines: Vec<&str> = b.lines().collect();
                for l in &lines[lines.len().saturating_sub(5)..] {
                    o.push(format!("    {l}"));
                }
                return Ok(o.join("\n"));
            }
            // The shell's last statement is `[ -f "$WATCH_LOG" ] && { … }`, so
            // with no log yet the TEST becomes the function's return value and
            // `watch status` exits 1 — on a machine where nothing is wrong. A
            // reporting command that fails for having nothing to report is a
            // bug, but it is the shell's behavior; §6.7 says the fix lands
            // separately, on the shell side, with the gate updated in that commit.
            println!("{}", o.join("\n"));
            std::process::exit(1);
        }
        "tick" => {
            tick(ctx);
            Ok(String::new())
        }
        _ => die(cfg, &format!("usage: {PROG} watch [install | uninstall | status]")),
    }
}

fn tick(ctx: &Ctx) {
    // The debounce lock is taken UP FRONT, so the crash-recovery, stale-proxy
    // and reload paths can never overlap a concurrent tick — a StartInterval
    // timer tick and a WatchPaths network tick can fire moments apart.
    let lock = ctx.cfg.join("watch.lock");
    if std::fs::create_dir(&lock).is_err() {
        // …unless it is stale. A tick finishes in seconds, so a lock older than
        // a minute means a previous one died without cleaning up. Reclaim it: a
        // leaked lock must never brick the watchdog permanently. (It did once.)
        let stale = std::fs::metadata(&lock).ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .map(|e| e.as_secs() > 60)
            .unwrap_or(false);
        if !stale {
            return;
        }
        let _ = std::fs::remove_dir(&lock);
        if std::fs::create_dir(&lock).is_err() {
            return;
        }
    }
    let cfg = cfg_of(ctx);
    let mut st = load_state(ctx);
    let mut disk = st.clone();

    // A deliberately-off proxy is a normal running state, and the shell exits
    // here — before it reads anything about the machine. Read it first, or the
    // tick probes a captive portal and resolves a hostname on behalf of a user
    // who asked rowt to keep its hands off. The FSM agrees (guard returns an
    // empty plan for this), so exiting early changes no decision.
    if ctx.sget("proxy_intent") == "off" {
        let _ = std::fs::remove_dir(&lock);
        return;
    }

    // The interface and whether the network MOVED (net_id vs watch.net, which
    // netcheck wrote on the last tick that got that far) are read here, before
    // the probe: the resolver comes from the interface, and the re-probe burst
    // is only for a network that just changed.
    let iface = Mac.detect_iface();
    let moved = iface.as_deref().is_some_and(|i| st.last_net_id.as_deref() != Some(net_id(i).as_str()));
    let (cap, portal) = captive_state(&ctx.logdir(), iface.as_deref(), moved);
    // The re-probe burst is the one place a healthy tick spends tens of
    // seconds (≈33 s worst case). Restart the stale-lock clock after it, so
    // "older than a minute" keeps measuring the tick the reclaim comment
    // describes and a concurrent tick cannot reclaim a lock that is merely
    // busy.
    if let Ok(f) = std::fs::File::open(&lock) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
    // Journal HERE, where the shell journals — right after the verdict and
    // before anything is read about the proxy. It stays in the plan as
    // `Action::Journal` because that is what the shadow comparison matches
    // against; `perform` skips it below rather than running it twice.
    journal(ctx, cap);
    let obs = observe(ctx, Some(cap), portal.clone(), true);
    let g = guard(&obs, &st, &cfg);
    perform_planned(ctx, &g.actions);
    st = g.state;
    save_state(ctx, &mut disk, &st);
    if g.next == Next::Stop {
        let _ = std::fs::remove_dir(&lock);
        return;
    }

    // Settle, then re-observe: corp_sync and the settle itself can both take the
    // router down, so a single snapshot would be judging a machine that no
    // longer exists.
    std::thread::sleep(std::time::Duration::from_secs(2));
    if lifecycle::host_running(ctx).is_none() {
        let _ = std::fs::remove_dir(&lock);
        return;
    }
    // Run corp_sync where the shell runs it: BEFORE the netcheck observation,
    // because it can take the router down and the observation has to judge the
    // machine that exists afterwards. netcheck emits `Action::CorpSync` to
    // record that it happened — `perform_planned` skips it, or every tick
    // would sync twice.
    let _ = crate::corp::sync(ctx, true);
    if lifecycle::host_running(ctx).is_none() {
        let _ = std::fs::remove_dir(&lock);
        return;
    }
    let hb = ctx.mode() == "local" || health_ok(ctx);
    let obs2 = observe(ctx, Some(cap), portal, hb);
    let n = netcheck(&obs2, &st, &cfg);
    perform_planned(ctx, &n.actions);
    save_state(ctx, &mut disk, &n.state);
    let _ = std::fs::remove_dir(&lock);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The captive verdict decides whether the watchdog DROPS the system
    /// proxy. Getting it wrong on a flaky network strands the machine, and
    /// the only other way to exercise it is a real portal.
    #[test]
    fn a_portal_is_told_apart_from_a_working_network() {
        let v = |s: &str| format!("{:?}", probe_verdict(s, "http://probe.example/x").0);
        // Apple's probe: 200 with "Success" in the body and nothing else.
        assert_eq!(v("<HTML><HEAD><TITLE>Success</TITLE></HEAD></HTML>\n\n200\nredirect="), "Clear");
        // A portal serving its login page under the real URL — 200, wrong body.
        assert_eq!(v("<html>Please sign in to the hotel wifi</html>\n302 moved\n200\nredirect="), "Captive");
        // …one that redirects instead, and one that says so (RFC 6585).
        assert_eq!(v("\n302\nredirect=http://portal.fake/login"), "Captive");
        assert_eq!(v("\n307\nredirect="), "Captive");
        assert_eq!(v("<html>Network Authentication Required</html>\n511\nredirect="), "Captive");
        // Anything else is UNKNOWN, and unknown means hands off. A 404 or a
        // timeout is not evidence of a portal, and acting on it would drop the
        // proxy because the network was briefly unreachable. 403 is a corp web
        // filter as often as a portal, so it stays here on purpose.
        assert_eq!(v("\n404\nredirect="), "Unknown");
        assert_eq!(v("\n403\nredirect="), "Unknown");
        assert_eq!(v("\n500\nredirect="), "Unknown");
        assert_eq!(v(""), "Unknown");
        assert_eq!(v("\n"), "Unknown");
        // `3` alone is not `30x`: the shell tests three characters.
        assert_eq!(v("\n3\nredirect="), "Unknown");
    }

    /// The body is split at the LAST newline, not the first — Apple's page is
    /// multi-line, and splitting at the first would read HTML as a status code.
    /// And a trailing newline (the fixtures have one; `$()` eats it) is not a line.
    #[test]
    fn the_status_code_is_the_last_line_not_the_first() {
        assert_eq!(format!("{:?}", probe_verdict("line one\nline two\nSuccess\n200\nredirect=", "u").0), "Clear");
        assert_eq!(format!("{:?}", probe_verdict("line one\nline two\nSuccess\n200\nredirect=\n", "u").0), "Clear");
    }

    /// The page to open: the redirect when the portal named one, else the
    /// probe URL a portal answered under — and nothing when there is no portal.
    #[test]
    fn the_portal_page_is_the_redirect_or_else_the_probe_url() {
        let p = |s: &str| probe_verdict(s, "http://probe.example/x").1;
        assert_eq!(p("\n302\nredirect=http://portal.fake/login"), Some("http://portal.fake/login".into()));
        assert_eq!(p("<html>sign in</html>\n200\nredirect="), Some("http://probe.example/x".into()));
        assert_eq!(p("<html>Success</html>\n200\nredirect="), None);
        assert_eq!(p("\n404\nredirect="), None);
    }

    /// What `--resolve` pins: the probe URL's host and port — and nothing for
    /// an IP-literal URL, which is every sandbox case and any user pin by
    /// address. Resolving those would add a lookup to every trace for a
    /// hostname that is not one.
    #[test]
    fn only_a_named_probe_host_gets_resolved() {
        let hp = |u: &str| probe_host_port(u).map(|(h, p)| format!("{h}:{p}"));
        assert_eq!(hp("http://captive.apple.com/hotspot-detect.html").as_deref(), Some("captive.apple.com:80"));
        assert_eq!(hp("https://connectivitycheck.example/generate_204").as_deref(), Some("connectivitycheck.example:443"));
        assert_eq!(hp("http://portal.example:8080/x").as_deref(), Some("portal.example:8080"));
        assert_eq!(hp("http://portal.example").as_deref(), Some("portal.example:80"));
        assert_eq!(hp("http://127.0.0.1:8099/portal"), None);
        assert_eq!(hp("http://203.0.113.9/"), None);
        assert_eq!(hp("http://[::1]:8099/x"), None);
        assert_eq!(hp("http:///x"), None);
        assert_eq!(hp(""), None);
    }

    /// The probe's argv, byte for byte — because the harness compares argv and
    /// because the `-w` value is a LITERAL backslash-n, the way the shell's
    /// single quotes deliver it. curl expands the escape itself, so a real
    /// newline here produces the same OUTPUT and a different command line; that
    /// mismatch is what kept a `watch tick` case out of cli-cases.txt.
    #[test]
    fn the_probe_command_line_is_the_one_the_shell_writes() {
        assert_eq!(PROBE_W.as_bytes()[0], b'\\', "the -w format starts with a literal backslash");
        assert!(!PROBE_W.contains('\n'), "a real newline here is a different argv than the shell's");
        assert_eq!(PROBE_W, "\\n%{http_code}\\nredirect=%{redirect_url}");
        assert_eq!(
            probe_args("http://127.0.0.1:8099/portal", "6", None),
            ["-s", "--noproxy", "*", "--max-time", "6", "-w", PROBE_W, "http://127.0.0.1:8099/portal"]
        );
        // `--resolve` sits immediately before `-w`, as it does in the shell.
        assert_eq!(
            probe_args("http://captive.apple.com/x", "6", Some("captive.apple.com:80:203.0.113.5")),
            ["-s", "--noproxy", "*", "--max-time", "6", "--resolve", "captive.apple.com:80:203.0.113.5",
             "-w", PROBE_W, "http://captive.apple.com/x"]
        );
    }

    /// The DNS-free fallback's rule, which is deliberately stricter than the
    /// named probe's: ONLY a redirect counts. Without DNS a 200 could be a
    /// router admin page or a hotel TV, and treating that as a portal would
    /// drop the system proxy on a network that was merely slow. The page to
    /// open is the redirect target or nothing — never the fallback URL, which
    /// is an address nobody can reach.
    /// `log/captive.log` is written by BOTH implementations and read by one
    /// person grepping one file, so the vocabulary has to be the same on both
    /// sides — these are the words `_curl_why` prints in bin/rowt. The numbers
    /// are the ones that actually separate one dead network from another: a
    /// name that would not resolve, a SYN into a black hole, and a timeout.
    #[test]
    fn the_probe_log_names_a_failure_the_way_the_shell_names_it() {
        assert_eq!(curl_why(6), "could not resolve host");
        assert_eq!(curl_why(7), "could not connect");
        assert_eq!(curl_why(28), "timed out");
        assert_eq!(curl_why(35), "TLS handshake failed");
        // Anything else still says which status it was, rather than swallowing it.
        assert_eq!(curl_why(52), "curl exit 52");
    }

    #[test]
    fn the_dns_free_fallback_acts_only_on_a_redirect() {
        let v = |body: &str| {
            let (code, redir) = split_probe(body);
            let portal = code == "511" || (code.len() == 3 && code.starts_with("30"));
            portal.then(|| (!redir.is_empty()).then(|| redir))
        };
        // A portal redirecting an intercepted request: act, and open its target.
        assert_eq!(v("\n302\nredirect=http://portal.example/login"), Some(Some("http://portal.example/login".into())));
        assert_eq!(v("\n511\nredirect="), Some(None));
        // Everything else is NOT evidence without a name behind it.
        assert_eq!(v("<html>router admin</html>\n200\nredirect="), None);
        assert_eq!(v("<html>Success</html>\n200\nredirect="), None);
        assert_eq!(v("\n404\nredirect="), None);
        assert_eq!(v("\n403\nredirect="), None);
        assert_eq!(v(""), None);
    }

    /// One parser for both probes: a drift here would let the fallback read a
    /// code the main probe would not.
    #[test]
    fn both_probes_peel_the_write_out_tail_the_same_way() {
        assert_eq!(split_probe("body\n200\nredirect="), ("200".into(), "".into()));
        assert_eq!(split_probe("a\nb\n302\nredirect=http://x/y"), ("302".into(), "http://x/y".into()));
        // A trailing newline is not a line.
        assert_eq!(split_probe("body\n200\nredirect=\n"), ("200".into(), "".into()));
    }

    /// The burst schedule, read the way the shell reads it: `tr ',' ' '`
    /// then word-split, non-numbers skipped. Empty means no retry — that is
    /// what the sandbox sets, and a default there would cost every unknown
    /// tick fifteen seconds.
    #[test]
    fn the_retry_schedule_is_seconds_and_junk_is_skipped() {
        assert_eq!(retry_delays("5,10"), vec![5, 10]);
        assert_eq!(retry_delays("0,0"), vec![0, 0]);
        assert_eq!(retry_delays("3 6 9"), vec![3, 6, 9]);
        assert_eq!(retry_delays("5,,10, x ,2"), vec![5, 10, 2]);
        assert_eq!(retry_delays(""), Vec::<u64>::new());
        assert_eq!(retry_delays("soon"), Vec::<u64>::new());
    }

    /// `jq -e '.delay // empty'`, which is not the same as "delay is truthy".
    #[test]
    fn a_zero_delay_still_counts_as_an_answer() {
        // The one that matters: jq's `//` falls through on null and false
        // ONLY. Reading 0 as falsy would call a live tunnel wedged and fire a
        // recovery on every tick.
        assert!(delay_answered(r#"{"delay":0}"#));
        assert!(delay_answered(r#"{"delay":142}"#));
        assert!(!delay_answered(r#"{"delay":null}"#));
        assert!(!delay_answered(r#"{"delay":false}"#));
        // The clash API's shape when the outbound cannot be reached.
        assert!(!delay_answered(r#"{"message":"An error occurred in the delay test"}"#));
        assert!(!delay_answered("{}"));
        assert!(!delay_answered(""));
        assert!(!delay_answered("not json at all"));
    }

    /// A passwordless-root grant. What keeps it narrow is that it names seven
    /// VERBS rather than the binary — `networksetup *` would let the watchdog
    /// rename network services or read every stored password-protected setting.
    #[test]
    fn the_sudoers_rule_grants_exactly_the_seven_proxy_verbs() {
        let body = sudoers_for("someone");
        let rule = body.lines().find(|l| l.contains("NOPASSWD")).expect("a NOPASSWD line");
        assert!(rule.starts_with("someone ALL=(root) NOPASSWD: "), "scoped to the user: {rule}");
        for verb in ["-setsocksfirewallproxy", "-setsocksfirewallproxystate",
                     "-setwebproxy", "-setwebproxystate",
                     "-setsecurewebproxy", "-setsecurewebproxystate",
                     "-setproxybypassdomains"] {
            assert!(rule.contains(&format!("/usr/sbin/networksetup {verb} *")),
                    "missing {verb} in: {rule}");
        }
        // Seven commas' worth of verbs and no bare binary.
        assert_eq!(rule.matches("/usr/sbin/networksetup").count(), 7);
        assert!(!rule.contains("networksetup *"), "a bare wildcard would grant everything");
        // And it says how to remove itself, since it outlives the process.
        assert!(body.contains("rowt watch uninstall"));
    }

    /// The LaunchAgent. `AbandonProcessGroup` is the load-bearing key: without
    /// it launchd SIGKILLs the tick's whole process group when the tick exits,
    /// which kills the sing-box a reload just started — the router silently
    /// going down on a network switch.
    #[test]
    fn the_launch_agent_abandons_the_process_group_it_starts() {
        let ctx = Ctx::new(std::path::PathBuf::from("/tmp/rowt-test-cfg"));
        let body = plist_body(&ctx, Path::new("/opt/homebrew/bin/rowt"));
        assert!(body.contains("<key>AbandonProcessGroup</key><true/>"));
        // The two triggers: a resolver rewrite (every network transition) and
        // the periodic liveness poll.
        assert!(body.contains("/etc/resolv.conf"));
        assert!(body.contains("/var/run/resolv.conf"));
        assert!(body.contains("<key>StartInterval</key>"));
        // It must invoke `watch tick`, not `watch`, and name the binary it was
        // installed from rather than whatever `rowt` resolves to later.
        assert!(body.contains("<string>/opt/homebrew/bin/rowt</string>"));
        assert!(body.contains("<string>watch</string>\n    <string>tick</string>"));
        assert!(body.contains(&format!("<key>Label</key><string>{LABEL}</string>")));
        // Both streams land in watch.log, which is where a recovery's story is.
        assert!(body.contains("/tmp/rowt-test-cfg/log/watch.log"));
    }
}
