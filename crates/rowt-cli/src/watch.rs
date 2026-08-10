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

fn watch_log(ctx: &Ctx, msg: &str) {
    use std::io::Write;
    let line = format!("{}  {msg}\n", crate::sh_date("+%Y-%m-%d %H:%M:%S"));
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
    let user = out("id", &["-un"]).trim().to_string();
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

/// `_captive_state` — is a walled garden in the way?
///
/// Apple's probe answers 200 with a body containing "Success". A 200 with
/// anything ELSE is a portal serving its login page under the real URL, and a
/// 3xx is a portal redirecting. Anything else — including no answer at all —
/// is `unknown`, and unknown means hands-off: acting on a guess here would
/// drop the proxy on a flaky network.
fn captive_state() -> CaptiveState {
    if env_or("ROWT_CAPTIVE_CHECK", "1") != "1" {
        return CaptiveState::Unknown;
    }
    let url = env_or("ROWT_CAPTIVE_URL", "http://captive.apple.com/hotspot-detect.html");
    let t = env_or("ROWT_CAPTIVE_TIMEOUT", "3");
    let o = Command::new("curl")
        .args(["-s", "--noproxy", "*", "--max-time", &t, "-w", "\n%{http_code}", &url])
        .stderr(Stdio::null()).output();
    let Ok(o) = o else { return CaptiveState::Unknown };
    if !o.status.success() {
        return CaptiveState::Unknown;
    }
    let body = String::from_utf8_lossy(&o.stdout);
    // `${out##*$'\n'}` / `${out%$'\n'*}` — split at the LAST newline, because
    // the body itself contains plenty.
    let (payload, code) = match body.rfind('\n') {
        Some(i) => (&body[..i], body[i + 1..].trim()),
        None => ("", body.trim()),
    };
    match code {
        "200" if payload.contains("Success") => CaptiveState::Clear,
        "200" => CaptiveState::Captive,
        c if c.len() == 3 && c.starts_with("30") => CaptiveState::Captive,
        _ => CaptiveState::Unknown,
    }
}

/// `net_id` — a signature for "which network am I on", so a home→hotspot move
/// on the SAME interface still reads as a move.
fn net_id(iface: &str) -> String {
    if iface.is_empty() {
        return String::new();
    }
    let ssid = out("networksetup", &["-getairportnetwork", iface]);
    let ssid = ssid.rsplit(": ").next().unwrap_or("").trim().to_string();
    let addr = out("ipconfig", &["getifaddr", iface]).trim().to_string();
    let router = out("ipconfig", &["getoption", iface, "router"]).trim().to_string();
    format!("{ssid} {addr}/{router}").trim().to_string()
}

fn observe(ctx: &Ctx, cap: Option<CaptiveState>, health_ok: bool) -> Observation {
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
        proxy_any_on: svc.as_ref().map(|s| p.proxy_any_on(s)).unwrap_or(false),
        proxy_pointing_ok: svc.as_ref().map(|s| p.proxy_pointing_ok(s, ctx.port)).unwrap_or(false),
        proxy_bypass_ok: svc.as_ref().map(|s| rowt_platform::bypass_ok(s)).unwrap_or(false),
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

fn load_state(ctx: &Ctx) -> State {
    State {
        captive_flag: ctx.sget("captive") == "1",
        health_fails: read(&ctx.cfg.join("watch.health")).trim().parse().unwrap_or(0),
        last_net_id: {
            let n = read(&ctx.cfg.join("watch.net")).trim().to_string();
            if n.is_empty() { None } else { Some(n) }
        },
        last_recovery: read(&ctx.cfg.join("watch.recovery")).trim().parse().unwrap_or(0),
    }
}

fn save_state(ctx: &Ctx, st: &State) {
    lifecycle::sset(ctx, "captive", if st.captive_flag { "1" } else { "" });
    let _ = std::fs::write(ctx.cfg.join("watch.health"), format!("{}\n", st.health_fails));
    let _ = std::fs::write(ctx.cfg.join("watch.recovery"), format!("{}\n", st.last_recovery));
}

// ------------------------------------------------------------------ effects

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
                crate::shell::audit(&ctx.cfg, "BEGIN watchdog reload — network change");
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

/// The discovery journal: what this network advertises, appended only when the
/// signature CHANGES. In steady state it writes nothing, which is what makes it
/// readable months later.
fn journal(ctx: &Ctx, cap: CaptiveState) {
    let iface = Mac.detect_iface().unwrap_or_default();
    let sig = format!("{} | {} | {}", net_id(&iface), cap.as_str(),
                      out("scutil", &["--dns"]).lines()
                          .filter(|l| l.contains("search domain") || l.contains("nameserver"))
                          .collect::<Vec<_>>().join(","));
    let p = ctx.logdir().join("discovery.log");
    let last = read(&p).lines().next_back().unwrap_or("").to_string();
    if last.ends_with(&sig) {
        return;
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
        let _ = writeln!(f, "{}  {sig}", crate::sh_date("+%Y-%m-%d %H:%M:%S"));
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
        // `jq -e '.delay // empty'` — `//` falls through on null and false
        // only, and `-e` fails on those two and on no output at all. A delay of
        // 0 is a number jq is perfectly happy with, so it counts as answering.
        if serde_json::from_str::<serde_json::Value>(&out).ok()
            .and_then(|v| v.get("delay").cloned())
            .is_some_and(|d| !matches!(d, serde_json::Value::Null | serde_json::Value::Bool(false)))
        {
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

    let cap = captive_state();
    let obs = observe(ctx, Some(cap), true);
    let g = guard(&obs, &st, &cfg);
    perform(ctx, &g.actions);
    st = g.state;
    save_state(ctx, &st);
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
    let _ = crate::corp::sync(ctx, true);
    if lifecycle::host_running(ctx).is_none() {
        let _ = std::fs::remove_dir(&lock);
        return;
    }
    let hb = ctx.mode() == "local" || health_ok(ctx);
    let obs2 = observe(ctx, Some(cap), hb);
    let n = netcheck(&obs2, &st, &cfg);
    perform(ctx, &n.actions);
    save_state(ctx, &n.state);
    let _ = std::fs::remove_dir(&lock);
}
