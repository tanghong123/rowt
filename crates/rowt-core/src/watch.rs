//! The watchdog tick, as a decision function.
//!
//! Reimplements `cmd_watch`'s `tick` from bin/rowt. Every side effect becomes an
//! `Action` the caller performs, so the whole decision table — including the
//! captive-portal machine of DESIGN.md §11 — is testable without toggling a real
//! system proxy.
//!
//! The shell's tick is not one step: it observes, sleeps to let the network
//! settle, runs `corp_sync`, and observes again, because both of those can take
//! the router down underneath it. So this models two phases the caller drives,
//! matching that structure exactly rather than pretending one snapshot suffices.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptiveState {
    Clear,
    Captive,
    Unknown,
}

impl CaptiveState {
    pub fn as_str(self) -> &'static str {
        match self {
            CaptiveState::Clear => "clear",
            CaptiveState::Captive => "captive",
            CaptiveState::Unknown => "unknown",
        }
    }
    pub fn parse(s: &str) -> CaptiveState {
        match s {
            "clear" => CaptiveState::Clear,
            "captive" => CaptiveState::Captive,
            _ => CaptiveState::Unknown,
        }
    }
}

/// Everything the tick reads from the world at one instant.
#[derive(Debug, Clone, Default)]
pub struct Observation {
    pub proxy_intent: String,
    pub captive: Option<CaptiveState>,
    /// `ROWT_CAPTIVE_CHECK=0` — portal detection is switched off, so `captive`
    /// is `unknown` unconditionally and carries no information about the
    /// network at all.
    ///
    /// It is a field rather than `captive: None` because `None` is what gates
    /// the DISCOVERY JOURNAL in `guard` (`if let Some(cap) … Action::Journal`),
    /// and switching portal detection off must not switch network discovery
    /// off. Named for the DISABLED state on purpose: `Observation` derives
    /// `Default`, so a new bool is `false`, and `false` here means "the check
    /// is on" — which is what every existing watch-case fixture describes. The
    /// opposite name would have every fixture silently claim the check was off
    /// and the recovery rule below would never fire in any of them.
    pub captive_check_disabled: bool,
    /// (b): does the default gateway answer an echo? A gateway that replies
    /// while the probe host does not says the local link is up and something
    /// UPSTREAM is gating us — a walled garden. Nothing answering at all is a
    /// dead uplink, and the two want opposite responses from the watchdog.
    pub gateway_ok: bool,
    /// On `captive`: the portal's login page, as the probe saw it — the
    /// redirect target when it was redirected, else the probe URL itself (a
    /// portal serving its page under that URL answers the browser the same
    /// way). None otherwise.
    pub portal_url: Option<String>,
    pub active_service: Option<String>,
    /// `_proxy_any_on` — is any protocol currently proxied?
    pub proxy_any_on: bool,
    pub host_running: bool,
    pub intent: String,
    /// `sget boot` equals the current boot id — i.e. no reboot since `up`.
    pub boot_matches: bool,
    /// `detect_iface`; None means offline (no interface with both IP and gateway).
    pub iface: Option<String>,
    pub proxy_pointing_ok: bool,
    pub proxy_bypass_ok: bool,
    /// The `bind_interface` currently baked into host.json.
    pub bound_iface: Option<String>,
    pub net_id: String,
    pub mode: String,
    /// `_watch_probe` — is the escape tunnel answering? Only read in netcheck.
    pub health_ok: bool,
    pub now: i64,
}

/// The bits of rowt's state the tick reads and writes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    pub captive_flag: bool,
    pub health_fails: u32,
    pub last_net_id: Option<String>,
    pub last_recovery: i64,
    /// (c): when the network last changed under us. A portal is likely just
    /// after joining a network; a wedged tunnel is not. Persisted beside
    /// `last_recovery` because "recent" has to outlive the tick — the health
    /// streak takes three ticks to build, so by the time recovery is even
    /// considered the move that caused the portal is already minutes old.
    pub last_net_change: i64,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    pub health_fails: u32,
    pub health_cooldown: i64,
}

impl Default for Config {
    fn default() -> Self {
        Config { port: 7890, health_fails: 3, health_cooldown: 600 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Append to the discovery journal, if the signature changed.
    Journal(CaptiveState),
    Log(String),
    Audit(String),
    /// `_captive_proxy_off` — drop the proxy so a portal's login page can load.
    CaptiveProxyOff(String),
    /// `_captive_proxy_on` — put it back once the portal clears.
    CaptiveProxyOn(String),
    /// `_captive_open_portal` — hand the login page to the browser: the
    /// request the OS lost while the proxy was in the way.
    OpenPortal(String),
    /// A proxy left pointing at a dead router after a reboot.
    ClearStaleProxy(String),
    /// `_watch_recover <reason> cmd_reload`.
    Recover(String),
    CorpSync,
    WriteNetId(String),
    Reload(String),
}

/// What the caller should do after performing this phase's actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// Tick is over.
    Stop,
    /// Sleep to let the network settle, re-check the router, run corp_sync,
    /// re-check again, then call `netcheck` with a fresh observation.
    Settle,
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub actions: Vec<Action>,
    pub state: State,
    pub next: Next,
}

/// How far past the health threshold a hold may run before recovery is allowed
/// anyway. The escape hatch of (a): a probe host that has become permanently
/// unreachable must degrade back to the old behaviour rather than wedge
/// self-healing for good. At the defaults that is 9 failed ticks — about
/// eighteen minutes — before rowt stops believing the network story.
const HOLD_MULTIPLE: u32 = 3;

/// Should recovery be held because the NETWORK, not the tunnel, is the problem?
///
/// Recovery exists for a tunnel that is wedged while the network works — a
/// server-side connection death, a stuck UDP socket. That needs POSITIVE
/// evidence the physical path is fine, and a `clear` verdict is exactly that:
/// a direct, unproxied fetch of a known host succeeded. "not captive" is not
/// the same claim. `unknown` means we could not reach anything at all, and no
/// amount of restarting sing-box fixes a broken uplink.
///
/// `guard` stops the tick on a captive verdict, so what reaches here is
/// `clear`, `unknown`, or nothing.
/// (c): how long after a network change a portal is still the likelier story.
/// Generous next to the health streak — three failed probes at the default
/// interval is already six minutes, so a tighter window would expire before
/// recovery is ever considered and (c) would never fire at all.
const NET_CHANGE_WINDOW: i64 = 900;

/// (c): did the network change recently enough for a portal to be the likelier
/// explanation than a wedged tunnel?
fn moved_recently(obs: &Observation, st: &State) -> bool {
    st.last_net_change != 0 && obs.now - st.last_net_change < NET_CHANGE_WINDOW
}

fn hold_for_network(obs: &Observation, st: &State, fails: u32, cfg: &Config) -> bool {
    // (a), the floor. With the check off every verdict is `unknown` by
    // construction, so the rule would read "never recover" — silently
    // disabling self-healing for anyone who turned portal detection off, which
    // is a far worse failure than one wasted reload.
    if obs.captive_check_disabled {
        return false;
    }
    // The evidence recovery is waiting for.
    if obs.captive == Some(CaptiveState::Clear) {
        return false;
    }
    // (a), the escape hatch. Note this can only be reached while the tunnel is
    // ALSO failing — `health_ok` short-circuits before this branch — so a dead
    // probe host on a healthy tunnel never gets here at all.
    if fails >= cfg.health_fails.saturating_mul(HOLD_MULTIPLE) {
        return false;
    }
    // (b) + (c). `unknown` on its own is ambiguous: a garden that blocks DNS, a
    // dead uplink and a genuinely wedged tunnel all produce it, and only the
    // first wants recovery suppressed. So require corroboration before
    // suppressing — either the local link is demonstrably up while the probe
    // host is not (b), or we moved recently, which is when a portal is likely
    // and a wedge is not (c). With neither, the old behaviour stands and a
    // reload is as good a guess as any.
    if !obs.gateway_ok && !moved_recently(obs, st) {
        return false;
    }
    true
}

/// `_watch_recover`'s cooldown: too soon after the last one and it only says so.
fn recover_or_hold(
    actions: &mut Vec<Action>,
    st: &mut State,
    obs: &Observation,
    cfg: &Config,
    reason: &str,
) {
    let since = obs.now - st.last_recovery;
    if st.last_recovery != 0 && since < cfg.health_cooldown {
        actions.push(Action::Log(format!(
            "{reason} — last recovery {since}s ago (< {}s cooldown), holding off",
            cfg.health_cooldown
        )));
        return;
    }
    st.last_recovery = obs.now;
    st.health_fails = 0;
    // `_watch_recover` logs before it acts, so the decision is visible even if
    // the reload then fails. Without this the shell writes a line the plan does
    // not, and every real recovery would read as a divergence.
    actions.push(Action::Log(format!("{reason} — recovering (cmd_reload)")));
    actions.push(Action::Recover(reason.to_string()));
}

/// Phase one: intent, the captive machine, crash recovery, stale proxies.
pub fn guard(obs: &Observation, st: &State, cfg: &Config) -> Outcome {
    let mut a = Vec::new();
    let mut s = st.clone();

    // A deliberate proxy-off is a normal running state: hands off entirely, and
    // never re-enable a proxy the user turned off.
    if obs.proxy_intent == "off" {
        return Outcome { actions: a, state: s, next: Next::Stop };
    }

    if let Some(cap) = obs.captive {
        a.push(Action::Journal(cap));

        if cap == CaptiveState::Captive {
            // Drop once per episode. While a portal is in the way everything else
            // this tick would do is wrong: the walled garden blocks it, and a
            // re-asserted proxy hides the login page.
            if !s.captive_flag {
                s.captive_flag = true;
                // The login page to hand to the browser — the request the OS
                // made before the drop died on the proxy, and nothing retries
                // it. Once per episode (it sits inside the transition), and
                // only for a page: anything that is not http(s) is not one.
                // Announced with the drop's own lines, BEFORE the drop; the
                // `open` itself comes after it — the browser must not race a
                // proxy that is still on.
                let page = obs.portal_url.as_deref()
                    .filter(|u| u.starts_with("http://") || u.starts_with("https://"));
                fn announce(a: &mut Vec<Action>, page: Option<&str>) {
                    if let Some(u) = page {
                        a.push(Action::Log(format!("captive portal: opening its login page in the browser — {u}")));
                        a.push(Action::Audit(format!("watchdog: captive portal — opened {u} in the browser")));
                    }
                }
                match (&obs.active_service, obs.proxy_any_on) {
                    (Some(svc), true) => {
                        a.push(Action::Log(format!(
                            "captive portal detected — dropping system proxy on '{svc}' so the login page can load (auto-restores after login)"
                        )));
                        a.push(Action::Audit(format!(
                            "watchdog: captive portal — system proxy off on '{svc}' (intent stays on; auto-restore on clear)"
                        )));
                        announce(&mut a, page);
                        a.push(Action::CaptiveProxyOff(svc.clone()));
                    }
                    _ => {
                        a.push(Action::Log(
                            "captive portal detected — proxy already off; waiting for login".into(),
                        ));
                        announce(&mut a, page);
                    }
                }
                if let Some(u) = page {
                    a.push(Action::OpenPortal(u.to_string()));
                }
            }
            return Outcome { actions: a, state: s, next: Next::Stop };
        }

        if s.captive_flag {
            // Only a provably CLEAR probe ends the episode — `unknown` (a timeout,
            // a refused connection) keeps us hands-off rather than guessing.
            if cap != CaptiveState::Clear {
                return Outcome { actions: a, state: s, next: Next::Stop };
            }
            s.captive_flag = false;
            if let Some(svc) = &obs.active_service {
                if obs.host_running {
                    a.push(Action::Log(format!(
                        "captive portal cleared — restoring system proxy on '{svc}'"
                    )));
                    a.push(Action::Audit(format!(
                        "watchdog: captive portal cleared — system proxy restored on '{svc}'"
                    )));
                    a.push(Action::CaptiveProxyOn(svc.clone()));
                } else {
                    a.push(Action::Log(
                        "captive portal cleared but the router is down — leaving the proxy off for recovery to handle".into(),
                    ));
                }
            }
        }
    }

    if !obs.host_running {
        // Router down. Bring it back only if the user wants it up AND we are still
        // in the boot it was started in — a reboot or a deliberate `down` must not
        // resurrect it.
        if obs.intent == "up" && obs.boot_matches {
            if obs.iface.is_some() {
                recover_or_hold(&mut a, &mut s, obs, cfg, "router is DOWN but marked up (crashed?)");
            } else {
                a.push(Action::Log(
                    "router down + marked up, but OFFLINE — deferring recovery until the network returns".into(),
                ));
            }
            return Outcome { actions: a, state: s, next: Next::Stop };
        }
        // rowt has no auto-start, but the system proxy it set survives a reboot.
        // Clear it so the network is not stranded pointing at a dead port.
        if let Some(svc) = &obs.active_service {
            if obs.proxy_pointing_ok {
                a.push(Action::Log(format!(
                    "rowt not running but '{svc}' proxy still set to 127.0.0.1:{} — clearing (stale, no-limbo)",
                    cfg.port
                )));
                let intent = if obs.intent.is_empty() { "unset" } else { &obs.intent };
                a.push(Action::Audit(format!(
                    "watchdog: cleared stale system proxy on '{svc}' (router down, intent={intent})"
                )));
                a.push(Action::ClearStaleProxy(svc.clone()));
            }
        }
        return Outcome { actions: a, state: s, next: Next::Stop };
    }

    Outcome { actions: a, state: s, next: Next::Settle }
}

/// Phase two, after the settle and `corp_sync`: has the network moved, and is
/// the tunnel still carrying traffic?
pub fn netcheck(obs: &Observation, st: &State, cfg: &Config) -> Outcome {
    let mut a = vec![Action::CorpSync, Action::WriteNetId(obs.net_id.clone())];
    let mut s = st.clone();

    // (c): stamp the move BEFORE any branch reads it, so "recent" means the
    // same thing however this tick ends.
    if st.last_net_id.as_deref() != Some(obs.net_id.as_str()) {
        s.last_net_change = obs.now;
    }

    let iface_moved = matches!((&obs.iface, &obs.bound_iface), (Some(i), b) if Some(i) != b.as_ref());
    let proxy_wrong = obs.active_service.is_some() && !(obs.proxy_pointing_ok && obs.proxy_bypass_ok);
    let need = iface_moved || proxy_wrong;

    if !need {
        // Log a real network move (rare); stay silent for a bare timer tick so the
        // periodic poll does not fill the log.
        if let Some(last) = &st.last_net_id {
            if *last != obs.net_id {
                a.push(Action::Log(format!(
                    "moved network [{last} -> {}] but iface={} + proxy '{}' unchanged — no reload needed, skip",
                    obs.net_id,
                    obs.iface.clone().unwrap_or_default(),
                    obs.active_service.clone().unwrap_or_default()
                )));
            }
        }
        s.last_net_id = Some(obs.net_id.clone());

        // local mode has no tunnel to probe; leaving this on would fail every tick
        // and drive recovery into a permanent reload loop.
        if obs.mode != "local" {
            if obs.iface.is_none() {
                // Offline: a restart cannot help, and a returning network fires its
                // own tick. Reset the streak and wait.
                s.health_fails = 0;
            } else if obs.health_ok {
                s.health_fails = 0;
            } else {
                s.health_fails += 1;
                if s.health_fails < cfg.health_fails {
                    a.push(Action::Log(format!(
                        "escape tunnel probe failed ({}/{})",
                        s.health_fails, cfg.health_fails
                    )));
                } else {
                    let n = s.health_fails;
                    if hold_for_network(obs, &s, n, cfg) {
                        // The 2026-09-14 hotel, exactly: captive=unknown, three
                        // failed tunnel probes, a reload that could not possibly
                        // succeed because the machine had no path to anything —
                        // and it burned the 600s cooldown and logged a failure
                        // that reads as a tunnel problem.
                        a.push(Action::Log(format!(
                            "tunnel wedged ({n} consecutive probe failures) — but the captive probe reached nothing either \
                             (captive={}), so the NETWORK is the suspect and a reload cannot fix an uplink; holding off recovery",
                            obs.captive.map(|c| c.as_str()).unwrap_or("none")
                        )));
                    } else {
                        recover_or_hold(
                            &mut a,
                            &mut s,
                            obs,
                            cfg,
                            &format!("tunnel wedged ({n} consecutive probe failures)"),
                        );
                    }
                }
            }
        }
        return Outcome { actions: a, state: s, next: Next::Stop };
    }

    s.last_net_id = Some(obs.net_id.clone());
    let cur = obs.bound_iface.clone().unwrap_or_default();
    let ifc = obs.iface.clone().unwrap_or_default();
    let svc = obs.active_service.clone().unwrap_or_default();
    // Name the reason that actually fired. `need` has two independent causes and
    // this said "network change" for both, so a reload triggered by a re-pointed
    // system proxy logged `iface 'en0' -> 'en0'` — an interface that had not
    // moved, in a message about the interface moving. Four divergence bundles in
    // three weeks read that way, and the misreading cost a wrong bug report.
    let (why_log, why_audit, why_short) = if iface_moved {
        (
            format!("network change (iface '{cur}' -> '{ifc}', [{}], service '{svc}') — reloading", obs.net_id),
            format!("BEGIN watchdog reload — network change (iface '{cur}' -> '{ifc}', [{}])", obs.net_id),
            format!("network change '{cur}'->'{ifc}'"),
        )
    } else {
        (
            format!("system proxy on '{svc}' no longer points at rowt (iface '{ifc}' unchanged, [{}]) — reloading", obs.net_id),
            format!("BEGIN watchdog reload — system proxy on '{svc}' no longer points at rowt (iface '{ifc}', [{}])", obs.net_id),
            format!("proxy re-point on '{svc}'"),
        )
    };
    a.push(Action::Log(why_log));
    a.push(Action::Audit(why_audit));
    a.push(Action::Reload(why_short));
    Outcome { actions: a, state: s, next: Next::Stop }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running() -> Observation {
        Observation {
            proxy_intent: "on".into(),
            captive: Some(CaptiveState::Clear),
            captive_check_disabled: false,
            // A machine that is up normally has a gateway that answers; the
            // interesting cases set it false deliberately.
            gateway_ok: true,
            portal_url: None,
            active_service: Some("Wi-Fi".into()),
            proxy_any_on: true,
            host_running: true,
            intent: "up".into(),
            boot_matches: true,
            iface: Some("en0".into()),
            proxy_pointing_ok: true,
            proxy_bypass_ok: true,
            bound_iface: Some("en0".into()),
            net_id: "Net 192.0.2.5/192.0.2.1".into(),
            mode: "host".into(),
            health_ok: true,
            now: 1_000_000,
        }
    }

    fn logs(o: &Outcome) -> Vec<String> {
        o.actions
            .iter()
            .filter_map(|a| match a {
                Action::Log(s) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

    // ---- DESIGN.md §11: the captive decision table ----

    #[test]
    fn intent_off_means_completely_hands_off() {
        let mut o = running();
        o.proxy_intent = "off".into();
        o.captive = Some(CaptiveState::Captive);
        let r = guard(&o, &State::default(), &Config::default());
        assert!(r.actions.is_empty());
        assert_eq!(r.next, Next::Stop);
    }

    #[test]
    fn captive_with_the_proxy_on_drops_it_once() {
        let mut o = running();
        o.captive = Some(CaptiveState::Captive);
        let r = guard(&o, &State::default(), &Config::default());
        assert!(r.state.captive_flag);
        assert!(r.actions.contains(&Action::CaptiveProxyOff("Wi-Fi".into())));
        assert_eq!(r.next, Next::Stop);

        // second tick in the same episode: nothing further
        let r2 = guard(&o, &r.state, &Config::default());
        assert!(!r2.actions.iter().any(|a| matches!(a, Action::CaptiveProxyOff(_))));
        assert!(logs(&r2).is_empty());
    }

    #[test]
    fn captive_with_the_proxy_already_off_just_waits() {
        let mut o = running();
        o.captive = Some(CaptiveState::Captive);
        o.proxy_any_on = false;
        let r = guard(&o, &State::default(), &Config::default());
        assert!(!r.actions.iter().any(|a| matches!(a, Action::CaptiveProxyOff(_))));
        assert_eq!(logs(&r), vec!["captive portal detected — proxy already off; waiting for login"]);
        assert!(r.state.captive_flag);
        // With a page: announced after the detection line, opened last.
        o.portal_url = Some("http://portal.fake/login".into());
        let r = guard(&o, &State::default(), &Config::default());
        assert_eq!(logs(&r), vec![
            "captive portal detected — proxy already off; waiting for login",
            "captive portal: opening its login page in the browser — http://portal.fake/login",
        ]);
        assert_eq!(r.actions.last(), Some(&Action::OpenPortal("http://portal.fake/login".into())));
    }

    /// The portal page is opened on the drop — with the proxy on or already
    /// off — once per episode, and only when the probe named an http(s) URL.
    #[test]
    fn the_login_page_is_opened_once_per_episode_and_only_for_a_page() {
        let opens = |o: &Outcome| o.actions.iter().filter(|a| matches!(a, Action::OpenPortal(_))).count();
        let mut o = running();
        o.captive = Some(CaptiveState::Captive);
        o.portal_url = Some("http://portal.fake/login".into());
        let r = guard(&o, &State::default(), &Config::default());
        assert!(r.actions.contains(&Action::CaptiveProxyOff("Wi-Fi".into())));
        assert!(r.actions.contains(&Action::OpenPortal("http://portal.fake/login".into())));
        // The drop comes first: the browser must not race a proxy still on.
        let at = |k: &dyn Fn(&Action) -> bool| r.actions.iter().position(|a| k(a)).unwrap();
        assert!(at(&|a| matches!(a, Action::CaptiveProxyOff(_))) < at(&|a| matches!(a, Action::OpenPortal(_))));
        // second tick in the same episode: nothing further
        assert_eq!(opens(&guard(&o, &r.state, &Config::default())), 0);
        // proxy already off: still opened — the page is the point
        o.proxy_any_on = false;
        assert_eq!(opens(&guard(&o, &State::default(), &Config::default())), 1);
        // not a page
        o.portal_url = Some("javascript:alert(1)".into());
        assert_eq!(opens(&guard(&o, &State::default(), &Config::default())), 0);
        o.portal_url = None;
        assert_eq!(opens(&guard(&o, &State::default(), &Config::default())), 0);
    }

    #[test]
    fn unknown_never_reads_as_captive() {
        let mut o = running();
        o.captive = Some(CaptiveState::Unknown);
        let r = guard(&o, &State::default(), &Config::default());
        assert!(!r.state.captive_flag);
        assert_eq!(r.next, Next::Settle); // a timeout must not strand the tick
    }

    #[test]
    fn mid_episode_unknown_stays_hands_off() {
        let mut o = running();
        o.captive = Some(CaptiveState::Unknown);
        let st = State { captive_flag: true, ..Default::default() };
        let r = guard(&o, &st, &Config::default());
        assert!(r.state.captive_flag, "the episode must not end on a guess");
        assert_eq!(r.next, Next::Stop);
    }

    #[test]
    fn clear_after_an_episode_restores_the_proxy() {
        let o = running();
        let st = State { captive_flag: true, ..Default::default() };
        let r = guard(&o, &st, &Config::default());
        assert!(!r.state.captive_flag);
        assert!(r.actions.contains(&Action::CaptiveProxyOn("Wi-Fi".into())));
        assert_eq!(r.next, Next::Settle, "restore falls through to a normal tick");
    }

    #[test]
    fn clear_with_the_router_down_leaves_the_proxy_off() {
        let mut o = running();
        o.host_running = false;
        let st = State { captive_flag: true, ..Default::default() };
        let r = guard(&o, &st, &Config::default());
        assert!(!r.actions.iter().any(|a| matches!(a, Action::CaptiveProxyOn(_))));
        assert!(logs(&r).iter().any(|l| l.contains("leaving the proxy off for recovery")));
    }

    // ---- recovery and stale proxies ----

    #[test]
    fn a_crashed_router_is_recovered_only_within_the_same_boot() {
        let mut o = running();
        o.host_running = false;
        let r = guard(&o, &State::default(), &Config::default());
        assert!(r.actions.iter().any(|a| matches!(a, Action::Recover(_))));

        o.boot_matches = false;
        let r2 = guard(&o, &State::default(), &Config::default());
        assert!(!r2.actions.iter().any(|a| matches!(a, Action::Recover(_))));
    }

    #[test]
    fn offline_defers_recovery_instead_of_flailing() {
        let mut o = running();
        o.host_running = false;
        o.iface = None;
        let r = guard(&o, &State::default(), &Config::default());
        assert!(!r.actions.iter().any(|a| matches!(a, Action::Recover(_))));
        assert!(logs(&r)[0].contains("OFFLINE — deferring recovery"));
    }

    #[test]
    fn a_proxy_outliving_a_reboot_is_cleared() {
        let mut o = running();
        o.host_running = false;
        o.intent = "".into();
        let r = guard(&o, &State::default(), &Config::default());
        assert!(r.actions.contains(&Action::ClearStaleProxy("Wi-Fi".into())));
        assert!(r.actions.iter().any(|a| matches!(a, Action::Audit(s) if s.contains("intent=unset"))));
    }

    #[test]
    fn recovery_respects_the_cooldown() {
        let mut o = running();
        o.host_running = false;
        let st = State { last_recovery: o.now - 10, ..Default::default() };
        let r = guard(&o, &st, &Config::default());
        assert!(!r.actions.iter().any(|a| matches!(a, Action::Recover(_))));
        assert!(logs(&r)[0].contains("holding off"));
    }

    // ---- netcheck ----

    #[test]
    fn a_moved_interface_forces_a_reload() {
        let mut o = running();
        o.bound_iface = Some("en1".into());
        let r = netcheck(&o, &State::default(), &Config::default());
        assert!(r.actions.iter().any(|a| matches!(a, Action::Reload(_))));
    }

    #[test]
    fn a_reload_names_the_reason_that_actually_fired() {
        // The interface has NOT moved; the proxy is what is wrong. Saying
        // "network change (iface 'en0' -> 'en0')" here is how four real
        // divergence bundles came to be read as an interface problem.
        let mut o = running();
        o.proxy_pointing_ok = false;
        let st = State { last_net_id: Some(o.net_id.clone()), ..Default::default() };
        let r = netcheck(&o, &st, &Config::default());
        let l = &logs(&r)[0];
        assert!(l.starts_with("system proxy on 'Wi-Fi' no longer points at rowt"), "{l}");
        assert!(!l.contains("network change"), "{l}");
        assert!(r.actions.iter().any(|a| matches!(a, Action::Reload(s) if s == "proxy re-point on 'Wi-Fi'")));

        // …and a real interface move still says so, unchanged.
        let mut o2 = running();
        o2.bound_iface = Some("en1".into());
        let r2 = netcheck(&o2, &State::default(), &Config::default());
        assert!(logs(&r2)[0].starts_with("network change (iface 'en1' -> 'en0'"), "{}", logs(&r2)[0]);
    }

    #[test]
    fn a_new_ip_on_the_same_interface_only_logs() {
        let o = running();
        let st = State { last_net_id: Some("Old 10.0.0.2/10.0.0.1".into()), ..Default::default() };
        let r = netcheck(&o, &st, &Config::default());
        assert!(!r.actions.iter().any(|a| matches!(a, Action::Reload(_))));
        assert!(logs(&r)[0].starts_with("moved network ["));
    }

    #[test]
    fn a_quiet_timer_tick_says_nothing() {
        let o = running();
        let st = State { last_net_id: Some(o.net_id.clone()), ..Default::default() };
        assert!(logs(&netcheck(&o, &st, &Config::default())).is_empty());
    }

    #[test]
    fn a_wedged_tunnel_recovers_only_after_the_streak() {
        let mut o = running();
        o.health_ok = false;
        let cfg = Config::default();
        let mut st = State { last_net_id: Some(o.net_id.clone()), ..Default::default() };
        for expect in 1..cfg.health_fails {
            let r = netcheck(&o, &st, &cfg);
            assert!(logs(&r)[0].contains(&format!("({expect}/{})", cfg.health_fails)));
            st = r.state;
        }
        let r = netcheck(&o, &st, &cfg);
        assert!(r.actions.iter().any(|a| matches!(a, Action::Recover(s) if s.contains("wedged"))));
    }

    /// Reach the streak with the captive probe in whatever state, and report
    /// the tick that crosses the threshold.
    fn wedged(o: &Observation, cfg: &Config) -> Outcome {
        let mut st = State { last_net_id: Some(o.net_id.clone()), ..Default::default() };
        for _ in 1..cfg.health_fails {
            st = netcheck(o, &st, cfg).state;
        }
        netcheck(o, &st, cfg)
    }

    /// The 2026-09-14 hotel, in one test. Three failed tunnel probes — but the
    /// captive probe reached nothing either, so the machine had no path to
    /// anything and a reload could not possibly have succeeded. It fired
    /// anyway, burned the 600s cooldown, and logged a failure that reads as a
    /// tunnel problem.
    #[test]
    fn a_wedged_tunnel_holds_off_when_the_captive_probe_reached_nothing_either() {
        let mut o = running();
        o.health_ok = false;
        o.captive = Some(CaptiveState::Unknown);
        o.gateway_ok = true; // (b): the local link is up, something upstream gates us
        let r = wedged(&o, &Config::default());
        assert!(!r.actions.iter().any(|a| matches!(a, Action::Recover(_))),
                "a reload cannot fix an uplink");
        assert!(logs(&r).iter().any(|l| l.contains("NETWORK is the suspect")));
    }

    /// `unknown` is only evidence when someone actually looked. With
    /// `ROWT_CAPTIVE_CHECK=0` every verdict is `unknown` by construction, so
    /// keying the rule on the verdict alone would silently disable self-healing
    /// for anyone who turned portal detection off — far worse than one wasted
    /// reload, and invisible while it happened.
    #[test]
    fn switching_portal_detection_off_must_not_switch_self_healing_off() {
        let mut o = running();
        o.health_ok = false;
        o.captive = Some(CaptiveState::Unknown);
        o.gateway_ok = true;
        o.captive_check_disabled = true;
        let r = wedged(&o, &Config::default());
        assert!(r.actions.iter().any(|a| matches!(a, Action::Recover(_))));
    }

    /// (b) and (c) are what make `unknown` mean something. With neither — the
    /// gateway silent too, and no recent move — nothing distinguishes a garden
    /// from a dead uplink or a genuinely wedged tunnel, so the old behaviour
    /// stands and a reload is as good a guess as any.
    #[test]
    fn an_unknown_with_nothing_to_corroborate_it_still_recovers() {
        let mut o = running();
        o.health_ok = false;
        o.captive = Some(CaptiveState::Unknown);
        o.gateway_ok = false;
        let r = wedged(&o, &Config::default());
        assert!(r.actions.iter().any(|a| matches!(a, Action::Recover(_))));
    }

    /// (c) on its own is enough: the gateway may be silent because the portal
    /// blackholes everything, but a network we joined minutes ago is far likelier
    /// to be showing us a login page than to have wedged the tunnel.
    #[test]
    fn a_recent_move_corroborates_the_garden_on_its_own() {
        let mut o = running();
        o.health_ok = false;
        o.captive = Some(CaptiveState::Unknown);
        o.gateway_ok = false;
        o.now = 10_000;
        let cfg = Config::default();
        // A move one minute ago, with the net id already recorded so netcheck
        // does not re-stamp it this tick.
        let mut st = State {
            last_net_id: Some(o.net_id.clone()),
            last_net_change: o.now - 60,
            ..Default::default()
        };
        for _ in 1..cfg.health_fails {
            st = netcheck(&o, &st, &cfg).state;
        }
        let r = netcheck(&o, &st, &cfg);
        assert!(!r.actions.iter().any(|a| matches!(a, Action::Recover(_))));
        // …and the same move, long enough ago, stops corroborating anything.
        let mut stale = st.clone();
        stale.last_net_change = o.now - NET_CHANGE_WINDOW - 1;
        let r2 = netcheck(&o, &stale, &cfg);
        assert!(r2.actions.iter().any(|a| matches!(a, Action::Recover(_))));
    }

    /// And the floor under the floor: a probe host that has become permanently
    /// unreachable must degrade back to the old behaviour rather than wedge
    /// recovery for good.
    #[test]
    fn a_hold_gives_up_once_the_network_story_has_run_long_enough() {
        let mut o = running();
        o.health_ok = false;
        o.captive = Some(CaptiveState::Unknown);
        o.gateway_ok = true;
        let cfg = Config::default();
        let mut st = State { last_net_id: Some(o.net_id.clone()), ..Default::default() };
        let mut recovered = None;
        for tick in 1..=(cfg.health_fails * HOLD_MULTIPLE + 2) {
            let r = netcheck(&o, &st, &cfg);
            if r.actions.iter().any(|a| matches!(a, Action::Recover(_))) {
                recovered = Some(tick);
                break;
            }
            st = r.state;
        }
        assert_eq!(recovered, Some(cfg.health_fails * HOLD_MULTIPLE),
                   "recovery must resume at the multiple, neither sooner nor never");
    }

    #[test]
    fn local_mode_never_probes_a_tunnel_it_does_not_have() {
        let mut o = running();
        o.mode = "local".into();
        o.health_ok = false;
        let st = State { last_net_id: Some(o.net_id.clone()), ..Default::default() };
        let r = netcheck(&o, &st, &Config::default());
        assert_eq!(r.state.health_fails, 0);
        assert!(!r.actions.iter().any(|a| matches!(a, Action::Recover(_))));
    }

    #[test]
    fn a_real_cooldown_episode_from_the_watchdog_log() {
        // Replayed from watch.log, 2026-08-08 13:35:50 → 13:45:51: a recovery,
        // then a wedge 459s later that must hold off, then one at 601s that must
        // act. Real timestamps, so the arithmetic and the threshold are checked
        // against what actually happened rather than against a guess.
        let cfg = Config::default();
        let recovered_at = 1_000_000i64;
        let mut o = running();
        o.health_ok = false;
        o.now = recovered_at + 459;
        let st = State {
            health_fails: 2,
            last_recovery: recovered_at,
            last_net_id: Some(o.net_id.clone()),
            ..Default::default()
        };
        let held = netcheck(&o, &st, &cfg);
        let l = logs(&held);
        assert_eq!(
            l[0],
            "tunnel wedged (3 consecutive probe failures) — last recovery 459s ago (< 600s cooldown), holding off"
        );
        // holding off must NOT reset the streak — the next failure is the fourth
        assert_eq!(held.state.health_fails, 3);
        assert!(!held.actions.iter().any(|a| matches!(a, Action::Recover(_))));

        o.now = recovered_at + 601;
        let acted = netcheck(&o, &held.state, &cfg);
        assert!(logs(&acted)[0].starts_with("tunnel wedged (4 consecutive probe failures) — recovering"));
        assert!(acted.actions.iter().any(|a| matches!(a, Action::Recover(_))));
        assert_eq!(acted.state.health_fails, 0, "a recovery clears the streak");
    }

    #[test]
    fn a_healthy_probe_clears_the_streak() {
        let o = running();
        let st = State { health_fails: 2, last_net_id: Some(o.net_id.clone()), ..Default::default() };
        assert_eq!(netcheck(&o, &st, &Config::default()).state.health_fails, 0);
    }
}
