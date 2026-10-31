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
    /// dead uplink. Both hold recovery (since 2026-10-26); the answer picks
    /// the words of the hold's log line.
    pub gateway_ok: bool,
    /// On `captive`: the portal's login page, as the probe saw it — the
    /// redirect target when it was redirected, else the probe URL itself (a
    /// portal serving its page under that URL answers the browser the same
    /// way). None otherwise.
    pub portal_url: Option<String>,
    /// On `captive`: the portal's OWN host — from the redirect target, or out
    /// of the body of a page that answered 200 inline. It is what
    /// `hotspot add` takes, and nothing recorded it before (see
    /// `_captive_body_url`). Empty or None when the probe could not name one.
    pub portal_host: Option<String>,
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
    /// The interface had no IPv4 address when the tunnel was about to be
    /// probed (`ipconfig getifaddr` empty), so nothing was probed: offline, the
    /// same as `iface == None`. Separate because `iface` is read earlier in the
    /// tick, and `ROWT_IFACE` names an interface without checking it. Shadowed
    /// as a failed probe, a Wi-Fi coming back from down (2026-10-20) counted a
    /// failure the shell never had.
    pub probe_offline: bool,
    /// The quick probe failed but a far more patient one answered: the tunnel
    /// is SLOW, not wedged. A link whose round trip sits on
    /// `ROWT_HEALTH_TIMEOUT` fails every probe by a hair, and three of those
    /// used to restart a router carrying traffic the whole time (measured
    /// 2026-09-27 on in-flight satellite: 0/20 at the 8s default, 20/20 at 25s,
    /// every RTT 7.6-8.8s). Only ever set when the quick probe failed, so a
    /// healthy tick pays nothing for it.
    pub health_slow: bool,
    /// FLOWING is not WEDGED: how many escape-lane connections opened in the
    /// last `FLOW_WINDOW_SECS` have received bytes through the tunnel. A wedged
    /// tunnel makes no new connections; a slow one keeps relaying responses on
    /// those it does open, however the probe host fares. Measured 2026-10-19 in
    /// flight: the patient probe failed too, and four reloads dropped every
    /// open connection on a router that was relaying the whole time. Download,
    /// not upload: a client's bytes are counted while the dial is still
    /// pending. Only ever read when the patient probe failed as well, so a
    /// healthy tick pays nothing for it.
    pub health_flows: u32,
    /// The escape selection when it is ONE concrete server (`use <tag>`) in
    /// host mode: the pinned tag. None under `auto` and `best`, which re-pick
    /// by themselves, and in local or vm mode, where there is no selector here.
    pub pinned: Option<String>,
    /// That server as the physical NIC would dial it, `ip:port` — the
    /// escape-hosts answer for a name, else the system resolver's — or "" when
    /// no address is known or the protocol is not carried by TCP (hysteria2,
    /// tuic), where a TCP connect says nothing.
    pub pinned_addr: String,
    /// A direct TCP connect from the NIC to `pinned_addr`, bypassing sing-box:
    /// Some(false) is the one piece of evidence that separates a dead SERVER
    /// from a wedged router, since the tunnel probe fails the same way for
    /// both. Measured only at the recovery decision, like `health_slow` and
    /// `health_flows`, so a healthy tick pays nothing; None = not measured, or
    /// nothing to measure (no `pinned_addr`), and recovery runs as before —
    /// never hold on ignorance (rowt #s36).
    pub pinned_reach: Option<bool>,
    /// `use <tag> --failover` is on (state `failover=1`): the follower already
    /// leaves a dead pin for any server that answers, so the hold's remedy is
    /// not "use auto" but the follower's own promise. The hold itself is the
    /// same — with every server dead the follower stays on the pin, and a
    /// reload onto it would be as futile as ever.
    pub failover: bool,
    /// `date -u '+%Y-%m-%d %H:%M:%S'` at this tick: the stamp a hold that
    /// begins now is written with. An input rather than derived from `now`,
    /// so both implementations stamp with the same program.
    pub hold_stamp: String,
    pub now: i64,
}

/// How far back "a connection opened recently" reaches for `health_flows`:
/// long enough to span the three failed probes that bring a tick here, short
/// enough that a flow on it says the tunnel was making connections while the
/// probes were failing.
pub const FLOW_WINDOW_SECS: u32 = 120;

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
    /// rowt #s36: the watchdog is HOLDING instead of reloading, because the
    /// pinned server itself is dead and a reload would render the same config
    /// and restart the router onto the same dead server (2026-10-30: thirteen
    /// such reloads in half an hour, each cutting the corp and direct lanes
    /// too). Persisted as `watch.hold` ("<utc stamp>\t<tag>") so `rowt
    /// status` can say so and the warning is written once per hold, not once
    /// per tick. Retired when the probe answers, when the selection changes,
    /// and by any reload.
    pub hold: Option<Hold>,
}

/// A watchdog hold on a dead pinned server: when it began and which server.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hold {
    /// `date -u '+%Y-%m-%d %H:%M:%S'` when the hold began.
    pub since: String,
    pub tag: String,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    pub health_fails: u32,
    pub health_cooldown: i64,
    /// `ROWT_HEALTH_TIMEOUT`, seconds — named in the slow-probe message so the
    /// reader is told which knob to turn.
    pub health_timeout: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config { port: 7890, health_fails: 3, health_cooldown: 600, health_timeout: 8 }
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

/// The shape of a garden that WHITELISTS the captive probe host.
///
/// Many in-flight and lounge networks allow `captive.apple.com` through on
/// purpose, so the operating system's own captive popup stays quiet. rowt's
/// probe then fetches the genuine Success page and reports `clear` while every
/// real connection is still waiting for a login. Seen 2026-09-27 on a flight:
/// joined at 13:11:56, verdict `clear` at 13:12:44, and the escape probe failing
/// throughout — the user had to drop the proxy and log in by hand.
///
/// The tell is the combination: we only just joined, the captive probe says the
/// network is fine, and the tunnel cannot reach anything. `gateway_ok` is
/// deliberately NOT part of it — it is only measured when the verdict is not
/// clear (see `observe`) — and it would add nothing here, because a `clear`
/// verdict already proves a probe crossed the link and came back.
///
/// ADVISORY ONLY. It never drops the proxy and never changes the recovery
/// decision: DESIGN.md §11 admits only definite portal evidence for that, and a
/// heuristic this soft would eventually hold off a recovery someone needed.
fn suspect_whitelisted_portal(obs: &Observation, st: &State) -> bool {
    obs.captive == Some(CaptiveState::Clear) && !obs.health_ok && moved_recently(obs, st)
}

fn hold_for_network(obs: &Observation, fails: u32, cfg: &Config) -> bool {
    would_hold_for_network(obs.captive_check_disabled, obs.captive, fails, cfg)
}

/// `hold_for_network` on its three inputs alone, so the tick can ask it BEFORE
/// the observation is complete: the direct probe of a pinned server (rowt #s36)
/// is only worth making when this would not hold anyway — a dead gateway says
/// nothing about the server — and the shell applies the same condition, or the
/// two would disagree about the argv a tick produces.
pub fn would_hold_for_network(captive_check_disabled: bool, captive: Option<CaptiveState>, fails: u32, cfg: &Config) -> bool {
    // (a), the floor. With the check off every verdict is `unknown` by
    // construction, so the rule would read "never recover" — silently
    // disabling self-healing for anyone who turned portal detection off, which
    // is a far worse failure than one wasted reload.
    if captive_check_disabled {
        return false;
    }
    // The evidence recovery is waiting for.
    if captive == Some(CaptiveState::Clear) {
        return false;
    }
    // (a), the escape hatch. Note this can only be reached while the tunnel is
    // ALSO failing — `health_ok` short-circuits before this branch — so a dead
    // probe host on a healthy tunnel never gets here at all.
    if fails >= cfg.health_fails.saturating_mul(HOLD_MULTIPLE) {
        return false;
    }
    // Anything else the direct probe could not reach: hold. Until 2026-10-26
    // an `unknown` held only when corroborated — the gateway answering (b),
    // or a move within NET_CHANGE_WINDOW (c) — and with neither the tick
    // reloaded, "as good a guess as any". Then a corp Wi-Fi subnet with no
    // exit (gateway silent, the move long past) took eight reloads in 80
    // minutes, each dropping every open connection and each logged as a
    // tunnel fault. A silent gateway under a failed direct probe is not
    // ambiguous: neither probe passes through sing-box, so a wedged router
    // cannot produce that pair, and the next failed probes after the network
    // returns still catch a tunnel that is truly stuck. The hatch above bounds
    // the one case this delays (Hong, rowt #s21). `gateway_ok` now picks only
    // the words of the log line; (c) stays for `suspect_whitelisted_portal`.
    true
}

/// rowt #s36: is the pinned server itself dead? Only a measured `Some(false)`
/// says so: the tag it names and the NIC the connect was made from, for the
/// words of the hold. None of the other states — not pinned, no address, a UDP
/// server, a connect that reached it — hold anything.
fn pinned_dead(obs: &Observation) -> Option<(&str, &str)> {
    match (&obs.pinned, obs.pinned_reach) {
        (Some(tag), Some(false)) if !obs.pinned_addr.is_empty() => {
            Some((tag.as_str(), obs.iface.as_deref().unwrap_or("the NIC")))
        }
        _ => None,
    }
}

/// What the hold tells the reader to do: the short form (the "still dead"
/// line and the audit line) and the long one (the first warning). Under
/// `use <tag> --failover` the follower is already the remedy. Mirrors the
/// shell's `rem`/`rem2` in `_watch_health`, word for word.
fn hold_remedy(failover: bool, tag: &str) -> (String, String) {
    if failover {
        // Not "the follower found nothing": with ROWT_AUTO_FOLLOW=0, or a
        // router started before there was a follower, there is none to have
        // looked. Say what the flag promises and where to check.
        let r = format!("with --failover the follower moves off {tag} as soon as another server answers, and back when {tag} does; 'rowt status' says whether it is running");
        (r.clone(), r)
    } else {
        ("'rowt use auto' fails over by itself".into(),
         "'rowt use auto' fails over by itself, or pin another server with 'rowt use <other>'".into())
    }
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
    // A reload is a fresh start for the hold as well (`_watch_recover` removes
    // `watch.hold` beside `watch.health`): if the server is still dead, the
    // next streak says so again with a new stamp.
    st.hold = None;
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
                // The log keeps the address, not the identifiers: a portal's
                // query string carries the client's MAC and IP. `open` still
                // gets the URL whole.
                let host = obs.portal_host.as_deref().filter(|h| !h.is_empty());
                fn announce(a: &mut Vec<Action>, page: Option<&str>, host: Option<&str>) {
                    if let Some(u) = page {
                        let u = u.split('?').next().unwrap_or(u);
                        a.push(Action::Log(format!("captive portal: opening its login page in the browser — {u}")));
                        a.push(Action::Audit(format!("watchdog: captive portal — opened {u} in the browser")));
                        if let Some(h) = host {
                            a.push(Action::Log(format!(
                                "captive portal host: {h} — 'rowt hotspot add {h}' makes its page load with the proxy on"
                            )));
                        }
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
                        announce(&mut a, page, host);
                        a.push(Action::CaptiveProxyOff(svc.clone()));
                    }
                    _ => {
                        a.push(Action::Log(
                            "captive portal detected — proxy already off; waiting for login".into(),
                        ));
                        announce(&mut a, page, host);
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
            // A hold names the server it is about. Pinned elsewhere now, or on
            // `auto`/`best`: that hold is over, whatever this tick finds.
            if s.hold.as_ref().is_some_and(|h| obs.pinned.as_deref() != Some(h.tag.as_str())) {
                s.hold = None;
            }
            if obs.iface.is_none() || obs.probe_offline {
                // Offline: a restart cannot help, and a returning network fires its
                // own tick. Reset the streak and wait.
                s.health_fails = 0;
            } else if obs.health_ok {
                s.health_fails = 0;
                // The server answers again: the hold is over.
                s.hold = None;
            } else {
                s.health_fails += 1;
                if s.health_fails < cfg.health_fails {
                    a.push(Action::Log(format!(
                        "escape tunnel probe failed ({}/{})",
                        s.health_fails, cfg.health_fails
                    )));
                } else {
                    let n = s.health_fails;
                    if obs.health_slow {
                        // Slow is not dead. Recovery is expensive and rare, so
                        // one patient probe is cheap insurance against
                        // restarting a tunnel that was carrying traffic.
                        let t = cfg.health_timeout;
                        a.push(Action::Log(format!(
                            "escape tunnel answered a slower probe ({t}s → {}s) — slow, not wedged; not restarting. \
                             Raise ROWT_HEALTH_TIMEOUT if this link stays this slow (ROWT_HEALTH_TIMEOUT=20 rowt watch install bakes it into the agent)",
                            t * 3
                        )));
                        s.health_fails = 0;
                    } else if obs.health_flows > 0 {
                        // Flowing is not wedged. Positive evidence, like the
                        // slower probe answering, so the streak clears — and no
                        // escape hatch, because a tunnel relaying responses on
                        // connections it opened two minutes ago is working
                        // whatever the probe host says. What this leaves to a
                        // `rowt reload` by hand: some destinations flow while
                        // the probe host never does.
                        a.push(Action::Log(format!(
                            "escape tunnel is slow, not wedged: {} connection(s) opened in the last {FLOW_WINDOW_SECS}s \
                             received bytes through it; not restarting",
                            obs.health_flows
                        )));
                        s.health_fails = 0;
                    } else if hold_for_network(obs, n, cfg) {
                        // The 2026-09-14 hotel and the 2026-10-26 subnet with
                        // no exit: a reload that could not possibly succeed
                        // because the machine had no path to anything — and it
                        // burned the 600s cooldown and logged a failure that
                        // reads as a tunnel problem. Say what the two direct
                        // probes saw, in the words `rowt status` uses for the
                        // same measurement (`diag::uplink_verdict`). Mirrors
                        // `_watch_health`, word for word.
                        let nic = obs.iface.as_deref().unwrap_or("the NIC");
                        let cap = obs.captive.map(|c| c.as_str()).unwrap_or("none");
                        a.push(Action::Log(if obs.gateway_ok {
                            format!("{nic} reaches its gateway but nothing beyond it ({n} consecutive probe failures, captive={cap}) \
                                     — the network's exit is gating us, not the tunnel; a reload cannot fix this, holding off recovery")
                        } else {
                            format!("nothing answers over {nic}, not even its gateway ({n} consecutive probe failures, captive={cap}) \
                                     — the network is down or walled, not the tunnel; a reload cannot fix this, holding off recovery")
                        }));
                    } else if let Some((tag, nic)) = pinned_dead(obs) {
                        // rowt #s36 (Hong, #s35 option 1): the pinned server
                        // ITSELF is dead — a direct TCP connect from the NIC,
                        // which never passes through sing-box, cannot reach it
                        // — so a reload would render the same config (nothing
                        // else moved: `need` is false, and every command that
                        // changes a render input renders) and restart the
                        // router onto the same dead server, cutting the corp
                        // and direct lanes for nothing. Manual stays manual:
                        // hold, and say what to do instead. Said in full once
                        // per hold, with an audit line; a short line per tick
                        // after that, like the cooldown hold. A server that
                        // DOES accept TCP while the tunnel probe fails is a
                        // wedged router, and recovers below as it always did.
                        let addr = &obs.pinned_addr;
                        let (rem, rem_long) = hold_remedy(obs.failover, tag);
                        if s.hold.as_ref().is_some_and(|h| h.tag == tag) {
                            a.push(Action::Log(format!(
                                "pinned server {tag} still dead ({addr} does not accept a TCP connection over {nic}, \
                                 {n} consecutive probe failures) — holding, not reloading; {rem}"
                            )));
                        } else {
                            a.push(Action::Log(format!(
                                "pinned server {tag} is dead: {addr} does not accept a TCP connection over {nic} \
                                 ({n} consecutive probe failures) — a reload would render the same config and restart \
                                 the router onto the same dead server, so the watchdog holds instead of reloading; {rem_long}"
                            )));
                            a.push(Action::Audit(format!(
                                "watchdog hold: pinned server {tag} is dead ({addr} does not accept a TCP connection over {nic}) \
                                 — not reloading; {rem}"
                            )));
                            s.hold = Some(Hold { since: obs.hold_stamp.clone(), tag: tag.to_string() });
                        }
                    } else {
                        // Once per streak, not once per tick: the streak only
                        // grows from here while a cooldown holds recovery off.
                        if n == cfg.health_fails && suspect_whitelisted_portal(obs, &s) {
                            a.push(Action::Log(
                                "…on a network joined moments ago whose captive probe says clear — some venues \
                                 allow the probe host through, so a login page may be waiting: \
                                 rowt proxy off, log in, then rowt proxy on"
                                    .into(),
                            ));
                        }
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
            portal_host: None,
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
            probe_offline: false,
            health_slow: false,
            health_flows: 0,
            pinned: None,
            pinned_addr: String::new(),
            pinned_reach: None,
            failover: false,
            hold_stamp: "2026-10-30 09:00:00".into(),
            now: 1_000_000,
        }
    }

    /// Under `use <tag> --failover` the hold is the same decision with the
    /// follower named as the remedy instead of `use auto`.
    #[test]
    fn under_failover_the_hold_names_the_follower_as_the_remedy() {
        let cfg = Config::default();
        let st = State { health_fails: cfg.health_fails - 1, ..State::default() };
        let mut o = pinned_dead_tick();
        o.failover = true;
        let r = netcheck(&o, &st, &cfg);
        assert!(!r.actions.iter().any(|a| matches!(a, Action::Recover(_))), "{:?}", r.actions);
        let l = logs(&r);
        assert!(l.iter().any(|m| m.ends_with("so the watchdog holds instead of reloading; with --failover the follower moves off alpha as soon as another server answers, and back when alpha does; 'rowt status' says whether it is running")), "{l:?}");
        assert!(!l.iter().any(|m| m.contains("use auto")), "{l:?}");
        assert!(r.actions.iter().any(|a| matches!(a, Action::Audit(m) if m.ends_with("— not reloading; with --failover the follower moves off alpha as soon as another server answers, and back when alpha does; 'rowt status' says whether it is running"))), "{:?}", r.actions);
        assert!(r.state.hold.is_some());
    }

    /// rowt #s36: `use <tag>` on a server that is dead, as the 2026-10-30
    /// outage: the tunnel probe fails, the slow probe and the flows say
    /// nothing, the network is clear, and a direct TCP connect from the NIC
    /// cannot reach the server either.
    fn pinned_dead_tick() -> Observation {
        let mut o = running();
        o.health_ok = false;
        o.health_slow = false;
        o.health_flows = 0;
        o.pinned = Some("alpha".into());
        o.pinned_addr = "203.0.113.10:443".into();
        o.pinned_reach = Some(false);
        o
    }

    /// The hold replaces the reload: no `Recover`, the warning names the
    /// server, the address, the NIC and the remedy, the audit log gets one
    /// line, and the state carries the hold with this tick's stamp.
    #[test]
    fn a_dead_pinned_server_is_held_not_reloaded() {
        let cfg = Config::default();
        let st = State { health_fails: cfg.health_fails - 1, ..State::default() };
        let r = netcheck(&pinned_dead_tick(), &st, &cfg);
        assert!(!r.actions.iter().any(|a| matches!(a, Action::Recover(_))), "{:?}", r.actions);
        let l = logs(&r);
        assert!(l.iter().any(|m| m.starts_with("pinned server alpha is dead: 203.0.113.10:443 does not accept a TCP connection over en0 (3 consecutive probe failures)")
                                  && m.contains("holds instead of reloading") && m.contains("'rowt use auto' fails over by itself")), "{l:?}");
        assert!(r.actions.iter().any(|a| matches!(a, Action::Audit(m) if m.starts_with("watchdog hold: pinned server alpha is dead"))), "{:?}", r.actions);
        assert_eq!(r.state.hold, Some(Hold { since: "2026-10-30 09:00:00".into(), tag: "alpha".into() }));
        // The streak keeps counting: this is not positive evidence of health.
        assert_eq!(r.state.health_fails, 3);
        assert_eq!(r.state.last_recovery, 0, "no recovery stamp: nothing was reloaded");
    }

    /// Once held, a tick says so in one short line and changes nothing: no
    /// second warning, no audit line, the stamp as it was.
    #[test]
    fn a_hold_in_progress_logs_one_short_line_and_keeps_its_stamp() {
        let cfg = Config::default();
        let hold = Hold { since: "2026-10-30 08:50:00".into(), tag: "alpha".into() };
        let st = State { health_fails: 7, hold: Some(hold.clone()), ..State::default() };
        let r = netcheck(&pinned_dead_tick(), &st, &cfg);
        assert!(!r.actions.iter().any(|a| matches!(a, Action::Recover(_) | Action::Audit(_))), "{:?}", r.actions);
        let l = logs(&r);
        assert_eq!(l.len(), 1, "{l:?}");
        assert!(l[0].starts_with("pinned server alpha still dead (203.0.113.10:443 does not accept a TCP connection over en0, 8 consecutive probe failures) — holding, not reloading"), "{l:?}");
        assert_eq!(r.state.hold, Some(hold));
    }

    /// The hold needs the evidence, not the pin: a server that accepts a TCP
    /// connection while the tunnel probe fails is a wedged router and is
    /// reloaded as before; so is one nothing could be measured about (a UDP
    /// server, no address), and so is `auto`, which the follower owns.
    #[test]
    fn without_evidence_that_the_server_is_dead_recovery_runs_as_before() {
        let cfg = Config::default();
        let st = State { health_fails: cfg.health_fails - 1, ..State::default() };
        let mut alive = pinned_dead_tick();
        alive.pinned_reach = Some(true);
        let mut unknown = pinned_dead_tick();
        unknown.pinned_reach = None;
        unknown.pinned_addr = String::new();
        let mut auto = pinned_dead_tick();
        auto.pinned = None;
        auto.pinned_addr = String::new();
        auto.pinned_reach = None;
        for (name, o) in [("alive", alive), ("unknown", unknown), ("auto", auto)] {
            let r = netcheck(&o, &st, &cfg);
            assert!(r.actions.iter().any(|a| matches!(a, Action::Recover(_))), "{name}: {:?}", r.actions);
            assert!(!logs(&r).iter().any(|m| m.contains("pinned server")), "{name}: {:?}", logs(&r));
            assert_eq!(r.state.hold, None, "{name}");
        }
    }

    /// A reload — the cooldown having passed — retires the hold along with the
    /// streak, and a hold left from before survives a cooldown tick untouched.
    #[test]
    fn a_recovery_retires_the_hold() {
        let cfg = Config::default();
        let hold = Hold { since: "2026-10-30 08:50:00".into(), tag: "alpha".into() };
        let mut alive = pinned_dead_tick();
        alive.pinned_reach = Some(true);
        let st = State { health_fails: 7, hold: Some(hold.clone()), ..State::default() };
        let r = netcheck(&alive, &st, &cfg);
        assert!(r.actions.iter().any(|a| matches!(a, Action::Recover(_))), "{:?}", r.actions);
        assert_eq!(r.state.hold, None);
        let cooling = State { health_fails: 7, hold: Some(hold.clone()), last_recovery: alive.now - 10, ..State::default() };
        let r = netcheck(&alive, &cooling, &cfg);
        assert!(logs(&r).iter().any(|m| m.contains("holding off")), "{:?}", logs(&r));
        assert_eq!(r.state.hold, Some(hold));
    }

    /// The hold is about one server: it ends when that server answers, and
    /// when the selection moves off it — to another pin or to `auto` — even
    /// on a tick that decides nothing else.
    #[test]
    fn the_hold_ends_when_the_server_answers_or_the_selection_moves() {
        let cfg = Config::default();
        let hold = Hold { since: "2026-10-30 08:50:00".into(), tag: "alpha".into() };
        let st = State { health_fails: 7, hold: Some(hold.clone()), ..State::default() };
        let mut answers = pinned_dead_tick();
        answers.health_ok = true;
        answers.pinned_reach = None;
        assert_eq!(netcheck(&answers, &st, &cfg).state.hold, None, "answered");
        let mut moved = pinned_dead_tick();
        moved.pinned = Some("beta".into());
        moved.pinned_addr = "203.0.113.11:8443".into();
        let r = netcheck(&moved, &st, &cfg);
        assert_eq!(r.state.hold, Some(Hold { since: "2026-10-30 09:00:00".into(), tag: "beta".into() }), "a new hold on the new pin, {:?}", r.actions);
        let mut auto = pinned_dead_tick();
        auto.pinned = None;
        auto.pinned_addr = String::new();
        auto.pinned_reach = None;
        let early = State { health_fails: 0, hold: Some(hold), ..State::default() };
        let r = netcheck(&auto, &early, &cfg);
        assert_eq!(r.state.hold, None, "moved to auto: {:?}", r.actions);
        assert!(!r.actions.iter().any(|a| matches!(a, Action::Recover(_))), "a first failure under auto only counts: {:?}", r.actions);
    }

    /// #48: a link whose round trip sits ON the probe timeout fails every
    /// probe by a hair. Three of those used to restart a router that was
    /// carrying traffic — measured 2026-09-27 on in-flight satellite.
    #[test]
    fn a_tunnel_that_is_merely_slow_is_not_restarted() {
        let cfg = Config::default();
        let mut o = running();
        o.health_ok = false;   // the quick probe failed…
        o.health_slow = true;  // …but a patient one answered
        let st = State { health_fails: cfg.health_fails - 1, ..State::default() };
        let r = netcheck(&o, &st, &cfg);
        assert!(!r.actions.iter().any(|a| matches!(a, Action::Recover { .. })),
                "a slow tunnel must not be restarted: {:?}", r.actions);
        assert!(logs(&r).iter().any(|l| l.contains("slow, not wedged")), "{:?}", logs(&r));
        assert!(logs(&r).iter().any(|l| l.contains("ROWT_HEALTH_TIMEOUT")), "name the knob");
        // The streak is cleared, so the next failure starts a fresh count
        // rather than recovering immediately.
        assert_eq!(r.state.health_fails, 0);
    }

    /// 2026-10-19, in flight: the link was slow enough that the patient probe
    /// failed too, and the watchdog reloaded a router that was relaying
    /// responses the whole time. Connections opened minutes ago that are
    /// receiving bytes are positive evidence the tunnel works.
    #[test]
    fn a_tunnel_still_relaying_on_fresh_connections_is_not_restarted() {
        let cfg = Config::default();
        let mut o = running();
        o.health_ok = false;
        o.health_slow = false; // the patient probe failed too…
        o.health_flows = 3;    // …but connections opened minutes ago are receiving bytes
        let st = State { health_fails: cfg.health_fails - 1, ..State::default() };
        let r = netcheck(&o, &st, &cfg);
        assert!(!r.actions.iter().any(|a| matches!(a, Action::Recover { .. })),
                "a relaying tunnel must not be restarted: {:?}", r.actions);
        assert!(logs(&r).iter().any(|l| l.contains("slow, not wedged") && l.contains("3 connection(s)")), "{:?}", logs(&r));
        // Positive evidence clears the streak, exactly like the slower probe.
        assert_eq!(r.state.health_fails, 0);
    }

    /// …and a genuinely dead tunnel is still recovered exactly as before. The
    /// confirming probe is insurance, not a veto on self-healing.
    #[test]
    fn a_dead_tunnel_is_still_recovered_promptly() {
        let cfg = Config::default();
        let mut o = running();
        o.health_ok = false;
        o.health_slow = false; // the patient probe failed too
        let st = State { health_fails: cfg.health_fails - 1, ..State::default() };
        let r = netcheck(&o, &st, &cfg);
        assert!(r.actions.iter().any(|a| matches!(a, Action::Recover { .. })),
                "a dead tunnel must still recover: {:?}", r.actions);
    }

    /// #47: a garden that WHITELISTS the captive probe host. The probe tells
    /// the truth about itself and lies about the network, so `clear` arrives
    /// while nothing else can get out. Seen 2026-09-27 in flight.
    #[test]
    fn a_whitelisted_probe_host_earns_a_hint_but_never_an_action() {
        let mut o = running();
        o.health_ok = false; // the tunnel reaches nothing
        o.captive = Some(CaptiveState::Clear); // …yet the probe says fine
        let st = State {
            health_fails: Config::default().health_fails - 1, // this tick crosses it
            last_net_change: o.now - 60,                      // joined a minute ago
            last_net_id: Some(o.net_id.clone()),
            ..State::default()
        };
        let r = netcheck(&o, &st, &Config::default());
        let hint = logs(&r).iter().any(|l| l.contains("some venues allow the probe host through"));
        assert!(hint, "expected the hint, got {:?}", logs(&r));
        // ADVISORY: the proxy is never dropped, and recovery still happens.
        assert!(!r.actions.iter().any(|a| matches!(a, Action::CaptiveProxyOff(_))));
        assert!(r.actions.iter().any(|a| matches!(a, Action::Recover { .. })),
                "the recovery decision must be unchanged: {:?}", r.actions);
    }

    #[test]
    fn the_hint_stays_quiet_when_the_shape_does_not_match() {
        let cfg = Config::default();
        let base = State {
            health_fails: cfg.health_fails - 1,
            last_net_change: 1_000_000 - 60,
            last_net_id: Some("Net 192.0.2.5/192.0.2.1".into()),
            ..State::default()
        };
        let hinted = |o: &Observation, st: &State| {
            logs(&netcheck(o, st, &cfg)).iter().any(|l| l.contains("some venues allow"))
        };
        // A tunnel failing on a network we have been on for ages is just wedged.
        let mut o = running();
        o.health_ok = false;
        let old = State { last_net_change: 1, ..base.clone() };
        assert!(!hinted(&o, &old), "an old network is not a fresh garden");
        // A verdict that is not `clear` is the case hold_for_network owns.
        let mut o2 = running();
        o2.health_ok = false;
        o2.captive = Some(CaptiveState::Unknown);
        assert!(!hinted(&o2, &base));
        // And a healthy tunnel says nothing at all.
        assert!(!hinted(&running(), &base));
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

    /// The 2026-10-20 divergence: `iface` was read while the Wi-Fi had an
    /// address, and by the probe it had none. The shell returns before its
    /// probe and resets the streak; so must the plan, and say nothing.
    #[test]
    fn no_address_at_the_probe_resets_the_streak_like_no_interface() {
        let mut o = running();
        o.health_ok = false;
        o.probe_offline = true;
        let st = State { last_net_id: Some(o.net_id.clone()), health_fails: 2, ..Default::default() };
        let r = netcheck(&o, &st, &Config::default());
        assert_eq!(r.state.health_fails, 0);
        assert!(logs(&r).is_empty(), "{:?}", logs(&r));
        assert!(!r.actions.iter().any(|a| matches!(a, Action::Recover(_))));
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
        assert!(logs(&r).iter().any(|l| l.starts_with("en0 reaches its gateway but nothing beyond it (3 consecutive probe failures, captive=unknown)")),
                "{:?}", logs(&r));
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

    /// The 2026-10-26 subnet with no exit: the direct probe reached nothing and
    /// the gateway did not answer either, long after the last move. Until then
    /// this was "nothing to corroborate the unknown, reload anyway", and eight
    /// reloads in 80 minutes fixed nothing. Neither probe passes through
    /// sing-box, so a wedged router cannot produce that pair: hold, and say
    /// what the NIC and its gateway answered.
    #[test]
    fn a_silent_gateway_under_a_failed_direct_probe_holds_too() {
        let mut o = running();
        o.health_ok = false;
        o.captive = Some(CaptiveState::Unknown);
        o.gateway_ok = false;
        let r = wedged(&o, &Config::default());
        assert!(!r.actions.iter().any(|a| matches!(a, Action::Recover(_))),
                "a reload cannot fix a dead first hop");
        assert!(logs(&r).iter().any(|l| l.starts_with("nothing answers over en0, not even its gateway (3 consecutive probe failures, captive=unknown)")
                                       && l.contains("not the tunnel")),
                "{:?}", logs(&r));
    }

    /// A move no longer decides the hold (the verdict does); it only ever
    /// corroborated a silent gateway, which now holds on its own. Recent or
    /// stale, the outcome is the same hold.
    #[test]
    fn a_move_recent_or_stale_no_longer_changes_the_hold() {
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
        // …and the same move, long enough ago, holds just the same now.
        let mut stale = st.clone();
        stale.last_net_change = o.now - NET_CHANGE_WINDOW - 1;
        let r2 = netcheck(&o, &stale, &cfg);
        assert!(!r2.actions.iter().any(|a| matches!(a, Action::Recover(_))));
        assert!(logs(&r2).iter().any(|l| l.starts_with("nothing answers over en0")), "{:?}", logs(&r2));
    }

    /// And the floor under the floor: a probe host that has become permanently
    /// unreachable must degrade back to the old behaviour rather than wedge
    /// recovery for good.
    #[test]
    fn a_hold_gives_up_once_the_network_story_has_run_long_enough() {
        // Gateway answering or silent alike: the hatch is what bounds both.
        for gateway_ok in [true, false] {
            let mut o = running();
            o.health_ok = false;
            o.captive = Some(CaptiveState::Unknown);
            o.gateway_ok = gateway_ok;
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
                       "gateway_ok={gateway_ok}: recovery must resume at the multiple, neither sooner nor never");
        }
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
