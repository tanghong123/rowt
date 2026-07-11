//! Real adapters over the six data sources (README "Data provenance"):
//! clash API (`/traffic`, `/connections`, `/proxies`), `host.json`,
//! `state`/`servers.json`, `lane-*.log`, and host system facts.
//!
//! Every field degrades gracefully: if the clash API is unreachable the binary
//! still renders from config/state/logs; if there is no rowt config at all it
//! falls back to the demo fixture so `rowt-monitor` always shows *something*.

use std::collections::{HashMap, HashSet};

use crate::source::parse::BlockBuckets;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;

use crate::model::*;
use crate::source::parse::{self, ErrAgg, ErrEvent, RawConn};
use crate::source::{FixtureSource, Source};

/// Server latency results shared with the background prober: tag -> (delay ms
/// or None if the last test failed, when it was measured).
type Delays = Arc<Mutex<HashMap<String, (Option<u32>, Instant)>>>;

/// Lane logs (name, lane) aggregated into the errors pane.
const LANE_FILES: [(&str, Lane); 4] = [
    ("lane-escape.log", Lane::Escape),
    ("lane-corp.log", Lane::Corp),
    ("lane-direct.log", Lane::Direct),
    ("lane-block.log", Lane::Block),
];
/// On first look, seed from this much of each log's tail (recent history);
/// afterwards only newly-appended bytes are read.
const LANE_TAIL_BYTES: u64 = 512 * 1024;
/// Widest selectable window — events older than this are pruned from the buffer.
const MAX_WINDOW_SECS: i64 = 24 * 60 * 60;
/// Hard cap on buffered events (RAM safety net; ~24h is normally well under it).
const ERR_BUF_CAP: usize = 40_000;
/// Cap a single incremental read (e.g. a large first-look catch-up).
const MAX_READ_BYTES: u64 = 8 * 1024 * 1024;
/// Cap on the per-domain connection-byte history (bounds RAM and the number of
/// dormant rows); the smallest-total domains are evicted past this.
const CONN_HISTORY_CAP: usize = 200;

/// Per-lane read cursor for incremental log tailing.
struct LaneCursor {
    name: &'static str,
    lane: Lane,
    offset: u64,
    mtime: Option<SystemTime>,
    primed: bool,
}

pub struct LiveSource {
    cfg: PathBuf,
    clash_port: u16,
    proxy_port: u16,
    fallback: FixtureSource,

    // rate computation: previous cumulative byte counts per connection id
    prev: HashMap<String, (u64, u64)>,
    prev_at: Option<Instant>,

    // Per-domain cumulative bytes carried over from CLOSED connections, so the
    // table can total a domain across short-lived connections. `conn_last` is the
    // last-seen state of each live connection, used to detect closes.
    conn_history: HashMap<parse::ConnKey, parse::ConnHist>,
    conn_last: HashMap<String, (parse::ConnKey, u64, u64, String)>,

    // Errors pane: a bounded rolling buffer of parsed events for the sparse
    // non-block lanes, plus per-minute per-domain counts for the high-volume
    // block lane (compact regardless of how chatty it is).
    errors_buf: Vec<ErrEvent>,
    block_buckets: BlockBuckets,
    newest_secs: i64,
    // Local UTC offset (secs), so the wall clock can be expressed in the same
    // "local time as if UTC" civil-seconds frame the lane-log timestamps use —
    // this is what lets the errors window age entries out in real time.
    tz_offset_secs: i64,
    lane_cursors: Vec<LaneCursor>,
    err_agg: Option<(Window, Option<Lane>, ErrAgg)>,
    err_last_refresh: Option<Instant>,
    err_dirty: bool,

    // server-health probing (manual selector mode leaves /proxies history empty,
    // so we actively run clash delay tests off the UI thread, like `rowt ping`).
    escape_members: Vec<String>,
    prev_members: Vec<String>, // sorted; to detect pool changes -> auto re-probe
    host_cache: Option<(SystemTime, HostInfo)>, // re-parse host.json only on mtime change
    delays: Delays,
    prober_started: bool,
    probe_interval: Duration,
    probe_tx: Option<Sender<()>>, // signal the prober to run now
    prev_router_up: bool,         // detect router down->up (reload / net switch)
    last_force: Option<Instant>,  // throttle self-heal re-probes

    started: Instant,
    uptime_base: Option<u64>, // proxy process uptime (secs) sampled once

    // System-proxy state ("on"/"other"/"off"), refreshed on a background thread
    // (networksetup is ~100ms; the poll runs on the input loop, so we must not
    // block it). `rowt proxy on/off` shows up within ~2s.
    sysproxy: Arc<Mutex<String>>,
    sysproxy_started: bool,

    // Control layer: outcomes of `rowt` commands run off the UI thread, drained
    // by the app each tick into a footer toast (CONTROLS.md §4, §9.2).
    ctl_out: Arc<Mutex<Vec<crate::source::CtlOutcome>>>,
}

impl LiveSource {
    pub fn new() -> Self {
        let cfg = config_dir();
        let clash_port = env_port("ROWT_CLASH_PORT", 9090);
        let proxy_port = env_port("ROWT_PORT", 7890);
        let uptime_base = proxy_uptime_secs();
        // Probe every 10 minutes by default (override with ROWT_MONITOR_PROBE_INTERVAL secs).
        let probe_interval = Duration::from_secs(env_port("ROWT_MONITOR_PROBE_INTERVAL", 600).max(5) as u64);
        LiveSource {
            cfg,
            clash_port,
            proxy_port,
            fallback: FixtureSource::new(),
            prev: HashMap::new(),
            prev_at: None,
            conn_history: HashMap::new(),
            conn_last: HashMap::new(),
            errors_buf: Vec::new(),
            block_buckets: BlockBuckets::new(),
            newest_secs: 0,
            tz_offset_secs: local_tz_offset_secs(),
            lane_cursors: LANE_FILES
                .iter()
                .map(|(name, lane)| LaneCursor { name, lane: *lane, offset: 0, mtime: None, primed: false })
                .collect(),
            err_agg: None,
            err_last_refresh: None,
            err_dirty: false,
            escape_members: Vec::new(),
            prev_members: Vec::new(),
            host_cache: None,
            delays: Arc::new(Mutex::new(HashMap::new())),
            prober_started: false,
            probe_interval,
            probe_tx: None,
            prev_router_up: true,
            last_force: None,
            started: Instant::now(),
            uptime_base,
            // Seed synchronously so the first frame is correct (no "off" flash).
            sysproxy: Arc::new(Mutex::new(read_system_proxy(proxy_port).to_string())),
            sysproxy_started: false,
            ctl_out: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Run a `rowt` command off the UI thread and queue its outcome for the
    /// footer toast. Uses `$ROWT_BIN` (exported by `rowt monitor`) so the control
    /// drives the exact CLI + config the monitor was launched from.
    fn spawn_rowt(&self, args: Vec<String>, ok: String) {
        let out = Arc::clone(&self.ctl_out);
        std::thread::Builder::new()
            .name("rowt-monitor-ctl".into())
            .spawn(move || {
                let bin = std::env::var("ROWT_BIN").unwrap_or_else(|_| "rowt".to_string());
                let outcome = match std::process::Command::new(&bin).args(&args).output() {
                    Ok(o) if o.status.success() => crate::source::CtlOutcome::Ok(ok),
                    Ok(o) => {
                        let err = String::from_utf8_lossy(&o.stderr);
                        let msg = err
                            .lines()
                            .map(str::trim)
                            .find(|l| !l.is_empty())
                            .map(|l| l.trim_start_matches("error:").trim().to_string())
                            .unwrap_or_else(|| format!("rowt {} failed", args.join(" ")));
                        crate::source::CtlOutcome::Err(msg)
                    }
                    Err(e) => crate::source::CtlOutcome::Err(format!("{bin}: {e}")),
                };
                if let Ok(mut v) = out.lock() {
                    v.push(outcome);
                }
            })
            .ok();
    }

    /// Refresh the system-proxy state off the input loop, so `rowt proxy on/off`
    /// (a per-service networksetup toggle) is reflected within ~2s without the
    /// ~100ms `networksetup` cost stalling keypress handling.
    fn ensure_sysproxy_watcher(&mut self) {
        if self.sysproxy_started {
            return;
        }
        self.sysproxy_started = true;
        let shared = Arc::clone(&self.sysproxy);
        let port = self.proxy_port;
        let interval = Duration::from_secs(2);
        std::thread::Builder::new()
            .name("rowt-monitor-sysproxy".into())
            .spawn(move || loop {
                std::thread::sleep(interval);
                let state = read_system_proxy(port);
                if let Ok(mut s) = shared.lock() {
                    if s.as_str() != state {
                        *s = state.to_string();
                    }
                }
            })
            .ok();
    }

    /// Start the background prober once the router is reachable. It runs clash
    /// delay tests (through the tunnel, like `rowt ping`) for the whole pool on
    /// a gentle interval, writing results into `self.delays` off the UI thread.
    fn ensure_prober(&mut self) {
        if self.prober_started || self.escape_members.is_empty() {
            return;
        }
        self.prober_started = true;
        let delays = Arc::clone(&self.delays);
        let cfg = self.cfg.clone();
        let port = self.clash_port;
        // Probe target: Google's generate_204 (blocked in CN when direct, so it
        // reaches 204 only *through* a working escape) — this tests real escape
        // reachability, matching rowt's auto-select urltest. Override via env.
        let url = std::env::var("ROWT_PING_URL").unwrap_or_else(|_| "https://www.gstatic.com/generate_204".to_string());
        let interval = self.probe_interval;
        let (tx, rx) = mpsc::channel::<()>();
        self.probe_tx = Some(tx);
        std::thread::Builder::new()
            .name("rowt-monitor-prober".into())
            .spawn(move || loop {
                // Re-read the pool and secret from config each round so added
                // servers / subscription updates / a rotated secret are picked
                // up automatically (immediate first round, then wait).
                let members = read_escape_members(&cfg);
                let secret = read_clash_secret(&cfg);
                let handles: Vec<_> = members
                    .into_iter()
                    .map(|tag| {
                        let (delays, secret, url) = (Arc::clone(&delays), secret.clone(), url.clone());
                        std::thread::spawn(move || {
                            let ms = clash_delay(port, secret.as_deref(), &tag, &url, 5000);
                            if let Ok(mut m) = delays.lock() {
                                m.insert(tag, (ms, Instant::now()));
                            }
                        })
                    })
                    .collect();
                for h in handles {
                    let _ = h.join();
                }
                // recv_timeout returns Ok on a force signal, Err(Timeout) after
                // the interval — either way we loop and probe again (timer reset).
                match rx.recv_timeout(interval) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            })
            .ok();
    }

    fn clash_secret(&self) -> Option<String> {
        let text = std::fs::read_to_string(self.cfg.join("state")).ok()?;
        parse::parse_state(&text).get("clash_secret").cloned()
    }

    fn clash_get(&self, path: &str) -> Option<Value> {
        let url = format!("http://127.0.0.1:{}{}", self.clash_port, path);
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_millis(400))
            .build();
        let mut req = agent.get(&url);
        if let Some(s) = self.clash_secret() {
            req = req.set("Authorization", &format!("Bearer {}", s));
        }
        req.call().ok()?.into_json().ok()
    }

    /// Parse-once view of host.json, re-read only when the file's mtime changes
    /// (stat is nearly free; the 19 KB JSON parse is not repeated every tick).
    fn host_info(&mut self) -> Option<HostInfo> {
        let path = self.cfg.join("host.json");
        let mtime = std::fs::metadata(&path).ok().and_then(|m| m.modified().ok());
        if let (Some(mt), Some((cmt, info))) = (mtime, self.host_cache.as_ref()) {
            if mt == *cmt {
                return Some(info.clone());
            }
        }
        let text = std::fs::read_to_string(&path).ok()?;
        let host: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let escape = escape_tags_of(&host);
        let info = HostInfo {
            config_ok: host.is_object(),
            members: escape.iter().filter(|t| *t != "escape").cloned().collect(),
            escape,
            iface: host_bind_iface(&host),
        };
        if let Some(mt) = mtime {
            self.host_cache = Some((mt, info.clone()));
        }
        Some(info)
    }

    /// Compute per-connection byte rates from the delta since the last poll.
    fn rates(&mut self, raw: &[RawConn]) -> HashMap<String, (f64, f64)> {
        let now = Instant::now();
        let dt = self.prev_at.map(|t| now.duration_since(t).as_secs_f64()).unwrap_or(0.0);
        let mut out = HashMap::new();
        let mut cur = HashMap::new();
        for c in raw {
            cur.insert(c.id.clone(), (c.up, c.down));
            if dt > 0.05 {
                if let Some(&(pu, pd)) = self.prev.get(&c.id) {
                    let u = c.up.saturating_sub(pu) as f64 / dt;
                    let d = c.down.saturating_sub(pd) as f64 / dt;
                    out.insert(c.id.clone(), (u, d));
                }
            }
        }
        self.prev = cur;
        self.prev_at = Some(now);
        out
    }

    /// Fold the final byte counts of connections that closed since the last poll
    /// into the per-domain history, so a domain's row keeps totalling its traffic
    /// after each short-lived connection drops out of the live set. Call once per
    /// poll *before* building rows so freshly-closed bytes appear this frame.
    fn accumulate_history(&mut self, raw: &[RawConn], escape: &HashSet<String>) {
        let mut current: HashMap<String, (parse::ConnKey, u64, u64, String)> = HashMap::with_capacity(raw.len());
        for c in raw {
            let lane = parse::classify_lane(&c.chains, escape);
            if lane == Lane::Block {
                continue; // block traffic is excluded from the connections view
            }
            current.insert(c.id.clone(), ((lane, c.host.clone(), c.port), c.up, c.down, c.rule.clone()));
        }
        // A connection present last tick but gone now has closed — add its
        // last-seen cumulative bytes to that domain's history.
        let closed: Vec<(parse::ConnKey, u64, u64, String)> =
            self.conn_last.iter().filter(|(id, _)| !current.contains_key(*id)).map(|(_, v)| v.clone()).collect();
        for (key, up, down, rule) in closed {
            let e = self.conn_history.entry(key).or_default();
            e.up += up;
            e.down += down;
            e.rule = rule;
        }
        self.conn_last = current;
        // Bound memory/rows: keep the top-N domains by total bytes.
        if self.conn_history.len() > CONN_HISTORY_CAP {
            let mut v: Vec<_> = std::mem::take(&mut self.conn_history).into_iter().collect();
            v.sort_by_key(|(_, h)| std::cmp::Reverse(h.up + h.down));
            v.truncate(CONN_HISTORY_CAP);
            self.conn_history = v.into_iter().collect();
        }
    }

    /// Incrementally read the newly-appended bytes of each lane log into the
    /// rolling event buffer. Reads bytes proportional to what was *added*, not
    /// the whole tail — so a 1 KB append costs a 1 KB read + a few lines parsed.
    /// mtime/size-gated (idle => nothing) and only the new region is parsed.
    fn refresh_errors_buf(&mut self) {
        let logdir = self.cfg.join("log");
        let mut fresh: Vec<ErrEvent> = Vec::new();
        for i in 0..self.lane_cursors.len() {
            let (name, lane, mut offset, primed, old_mtime) = {
                let c = &self.lane_cursors[i];
                (c.name, c.lane, c.offset, c.primed, c.mtime)
            };
            let is_block = lane == Lane::Block;
            let path = logdir.join(name);
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            let size = meta.len();
            let mtime = meta.modified().ok();

            let mut skip_leading = false;
            if !primed {
                // First look: start near the end so we get recent history
                // without reading the whole (possibly multi-MB) file.
                offset = size.saturating_sub(LANE_TAIL_BYTES);
                skip_leading = offset > 0; // first line is likely partial
            } else {
                if size == offset && mtime == old_mtime {
                    continue; // unchanged since last read
                }
                if size < offset {
                    offset = 0; // rotated/truncated -> read the new file from start
                }
            }

            if size > offset {
                if let Some((evs, new_off)) = read_lane_incremental(&path, offset, skip_leading, lane) {
                    for e in evs {
                        if e.secs > self.newest_secs {
                            self.newest_secs = e.secs;
                        }
                        if is_block {
                            // High-volume, low-cardinality: bucket by minute+domain.
                            *self
                                .block_buckets
                                .entry(parse::minute_of(e.secs))
                                .or_default()
                                .entry(e.domain)
                                .or_insert(0) += 1;
                            self.err_dirty = true;
                        } else {
                            fresh.push(e);
                        }
                    }
                    offset = new_off;
                }
            }
            let c = &mut self.lane_cursors[i];
            c.offset = offset;
            c.mtime = mtime;
            c.primed = true;
        }

        if !fresh.is_empty() {
            self.errors_buf.append(&mut fresh);
            self.err_dirty = true;
        }
        if self.err_dirty {
            self.prune_errors();
        }
    }

    /// Bound both stores to the widest window: drop old non-block events (with a
    /// hard count cap as a safety net) and drop block buckets older than 24h.
    fn prune_errors(&mut self) {
        let cutoff = self.newest_secs - MAX_WINDOW_SECS;
        self.errors_buf.retain(|e| e.secs >= cutoff);
        if self.errors_buf.len() > ERR_BUF_CAP {
            let drop = self.errors_buf.len() - ERR_BUF_CAP;
            self.errors_buf.drain(0..drop); // append order ~ chronological
        }
        let min_cutoff = parse::minute_of(cutoff);
        self.block_buckets.retain(|min, _| *min >= min_cutoff);
    }

    fn errors(&mut self, window: Window, lane: Option<Lane>) -> ErrAgg {
        // Check for new log data at most every 6s (fresh enough to spot new
        // blocked/failing domains, cheap enough to not amplify IO).
        let due = self.err_last_refresh.is_none_or(|t| t.elapsed() >= Duration::from_secs(6));
        if due {
            self.err_last_refresh = Some(Instant::now());
            self.refresh_errors_buf();
        }
        // Reference the rolling window to WALL-CLOCK now (in the lane logs' local
        // civil-seconds frame), not the newest event — otherwise a quiet spell
        // freezes the cutoff and stale entries never age out. `.max(newest)`
        // guards against clock skew hiding a just-arrived event.
        let now = wall_now_civil(self.tz_offset_secs).max(self.newest_secs);
        // Re-aggregate every poll: `now` advances each tick, so the boundary must
        // be recomputed even when no new events arrived (that's the whole fix).
        // Cheap — an in-memory filter/group over the bounded buffer.
        let agg = parse::aggregate_split(&self.errors_buf, &self.block_buckets, parse::window_secs(window), now, lane);
        self.err_agg = Some((window, lane, agg));
        self.err_dirty = false;
        self.err_agg.as_ref().unwrap().2.clone()
    }

    /// Derive the server-health strip from the background prober's results.
    /// A server is *up* if its most recent delay test succeeded, *down* if it
    /// failed, and simply not-yet-counted while its first probe is pending (so
    /// we never mislabel unprobed servers as down — the original bug).
    fn servers(&self, state: &HashMap<String, String>, router_up: bool) -> Health {
        let selected = state.get("selected").cloned().unwrap_or_default();
        let total = self.escape_members.len() as u32;
        if !router_up {
            // Can't probe through a down router; report the pool size only.
            return Health { total, up: 0, down: 0, active: selected, chips: Vec::new(), active_ms: None, active_ok: None };
        }
        let map = self.delays.lock().ok();
        // Results stay valid across a full probe interval (plus margin); only
        // treat them as stale — i.e. the prober likely died — after two missed
        // rounds. (Was a fixed 90s, which emptied the strip between 10-min
        // probes.)
        let max_age = self.probe_interval.saturating_mul(2) + Duration::from_secs(30);
        let fresh = |tag: &str| -> Option<Option<u32>> {
            let (ms, at) = map.as_ref()?.get(tag)?;
            if at.elapsed() < max_age {
                Some(*ms)
            } else {
                None // stale -> treat as pending, not down
            }
        };
        let mut up = 0u32;
        let mut down = 0u32;
        let mut chips = Vec::new();
        let mut active_ms = None;
        for tag in &self.escape_members {
            match fresh(tag) {
                Some(Some(ms)) => {
                    up += 1;
                    let active = *tag == selected;
                    if active {
                        active_ms = Some(ms);
                    }
                    // All up servers appear in the strip; the active one is marked.
                    chips.push(Server { name: tag.clone(), ms, active });
                }
                Some(None) => down += 1,
                None => {} // pending first probe — neither up nor down yet
            }
        }
        // Active first, then the rest by latency.
        chips.sort_by_key(|c| (!c.active, c.ms));

        // Active status. In auto mode there's no pinned server: it's healthy if
        // ANY server is reachable, ERROR if all probed ones failed. In manual
        // mode it's the selected server's own probe result. `None` = not yet
        // probed (don't alarm at startup).
        let probed = |up: u32, down: u32| if up > 0 { Some(true) } else if down > 0 { Some(false) } else { None };
        let (active_ms, active_ok) = if selected == "auto" {
            (chips.iter().map(|c| c.ms).min(), probed(up, down))
        } else {
            let ok = match fresh(&selected) {
                Some(Some(_)) => Some(true),
                Some(None) => Some(false),
                None => None,
            };
            (active_ms, ok)
        };
        Health { total, up, down, active: selected, chips, active_ms, active_ok }
    }
}

/// Cached, parse-once view of the fields we need from host.json.
#[derive(Clone)]
struct HostInfo {
    config_ok: bool,
    escape: HashSet<String>,
    members: Vec<String>,
    iface: Option<String>,
}

/// Server-health summary returned by `LiveSource::servers`.
struct Health {
    total: u32,
    up: u32,
    down: u32,
    active: String,
    chips: Vec<Server>,
    active_ms: Option<u32>,
    active_ok: Option<bool>,
}

impl Default for LiveSource {
    fn default() -> Self {
        Self::new()
    }
}

impl Source for LiveSource {
    fn label(&self) -> &str {
        "live"
    }

    fn force_probe(&self) {
        // Wake the prober; ignore if it hasn't started or the channel is gone.
        if let Some(tx) = &self.probe_tx {
            let _ = tx.send(());
        }
    }

    fn use_server(&self, tag: &str) {
        self.spawn_rowt(vec!["use".into(), tag.into()], format!("escape → {tag}"));
    }

    fn route_lane(&self, domain: &str, lane: Lane) {
        let verb = lane.label(); // escape | corp | block | (direct never routed here)
        self.spawn_rowt(
            vec![verb.into(), "add".into(), domain.into(), "--no-reload".into()],
            format!("{domain} → {verb}"),
        );
    }

    fn unroute(&self, domain: &str) {
        // Single-lane invariant means the domain is in at most one lane, but we
        // don't track which from an errors row — remove from all three (a miss is
        // a harmless "not found"), so the result is always "back to direct".
        let out = Arc::clone(&self.ctl_out);
        let (bin_domain, ok) = (domain.to_string(), format!("{domain} → direct"));
        std::thread::Builder::new()
            .name("rowt-monitor-ctl".into())
            .spawn(move || {
                let bin = std::env::var("ROWT_BIN").unwrap_or_else(|_| "rowt".to_string());
                let mut err: Option<String> = None;
                for lane in ["escape", "corp", "block"] {
                    let r = std::process::Command::new(&bin)
                        .args([lane, "rm", &bin_domain, "--no-reload"])
                        .output();
                    if let Err(e) = r {
                        err = Some(format!("{bin}: {e}"));
                        break;
                    }
                }
                let outcome = match err {
                    None => crate::source::CtlOutcome::Ok(ok),
                    Some(m) => crate::source::CtlOutcome::Err(m),
                };
                if let Ok(mut v) = out.lock() {
                    v.push(outcome);
                }
            })
            .ok();
    }

    fn set_proxy(&self, on: bool) {
        let sub = if on { "on" } else { "off" };
        self.spawn_rowt(
            vec!["proxy".into(), sub.into()],
            format!("system proxy {sub}"),
        );
    }

    fn reload_router(&self) {
        // Lane edits were written with --no-reload; issue ONE reload now. We do
        // render + router restart (the lightweight lane-reload) rather than
        // `rowt reload`, which would also re-assert the system proxy and fight
        // the `o` toggle / captive-portal flow (CONTROLS.md §4.3 deviation).
        let out = Arc::clone(&self.ctl_out);
        std::thread::Builder::new()
            .name("rowt-monitor-ctl".into())
            .spawn(move || {
                let bin = std::env::var("ROWT_BIN").unwrap_or_else(|_| "rowt".to_string());
                let render = std::process::Command::new(&bin).arg("render").output();
                let outcome = match render {
                    Ok(o) if o.status.success() => {
                        match std::process::Command::new(&bin).args(["router", "restart"]).output() {
                            Ok(o2) if o2.status.success() => crate::source::CtlOutcome::Ok("router reloaded".into()),
                            Ok(o2) => crate::source::CtlOutcome::Err(first_err_line(&o2.stderr, "reload failed")),
                            Err(e) => crate::source::CtlOutcome::Err(format!("{bin}: {e}")),
                        }
                    }
                    Ok(o) => crate::source::CtlOutcome::Err(first_err_line(&o.stderr, "render failed")),
                    Err(e) => crate::source::CtlOutcome::Err(format!("{bin}: {e}")),
                };
                if let Ok(mut v) = out.lock() {
                    v.push(outcome);
                }
            })
            .ok();
    }

    fn drain_ctl(&self) -> Vec<crate::source::CtlOutcome> {
        self.ctl_out.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default()
    }

    fn poll(&mut self, window: Window, lane: Option<Lane>) -> Snapshot {
        // No rowt config at all -> demo fixture so we always render something.
        let Some(info) = self.host_info() else {
            return self.fallback.poll(window, lane);
        };
        let config_ok = info.config_ok;
        let escape = info.escape;
        self.escape_members = info.members;
        let state = std::fs::read_to_string(self.cfg.join("state"))
            .map(|t| parse::parse_state(&t))
            .unwrap_or_default();
        // Detect a pool change (server add/rm, sub update) so we can re-probe
        // immediately instead of waiting for the next interval.
        let mut sig = self.escape_members.clone();
        sig.sort();
        let pool_changed = !self.prev_members.is_empty() && sig != self.prev_members;
        self.prev_members = sig;

        // Connections + throughput from clash (empty if unreachable).
        let conns_json = self.clash_get("/connections");
        let router_up = conns_json.is_some();
        let raw = conns_json.as_ref().map(parse::parse_connections).unwrap_or_default();
        let rates = self.rates(&raw);
        self.accumulate_history(&raw, &escape);
        // Aggregate throughput is the sum of per-connection rates (deltas since
        // the last tick). We intentionally do NOT read the streaming /traffic
        // endpoint here — it blocks the UI thread up to ~1s per poll.
        let (conns, lanes, all) = parse::build_conn_rows(&raw, &rates, &escape, &self.conn_history);
        if router_up {
            self.ensure_prober();
            // Pool changed (server add/rm, sub update) -> probe now rather than
            // waiting up to the full interval.
            if pool_changed {
                self.force_probe();
            }
        }

        let (transient, persistent, blocked, errors) = self.errors(window, lane);
        let h = self.servers(&state, router_up);

        // Self-heal a stale ERROR: after a network switch the active server's
        // last (pre-switch) probe can read failed for up to the 10-min cycle,
        // even though it's reachable again. Re-probe when the router just came
        // back (reload / net change) or keep re-probing ~every 60s while the
        // active server is failing, so it clears within seconds instead.
        if router_up {
            let recovered = !self.prev_router_up;
            let erroring = h.active_ok == Some(false);
            let due = self.last_force.is_none_or(|t| t.elapsed() > Duration::from_secs(60));
            if recovered || (erroring && due) {
                self.force_probe();
                self.last_force = Some(Instant::now());
            }
        }
        self.prev_router_up = router_up;

        // Reserve header space for the longest server name so the ms column is
        // stable as the active server changes (bounded so it can't overrun).
        let name_reserve = self
            .escape_members
            .iter()
            .map(|t| t.chars().count() as u16)
            .max()
            .unwrap_or(8)
            // Bounded so name + ` NNN ms` stays clear of the `router` column at
            // x0+70 (name@47 + gap + 6-wide ms → reserve ≤ 15).
            .clamp(8, 15);

        let iface = info.iface.unwrap_or_else(|| "—".to_string());
        let mode = format!("{} · {}", state.get("mode").map(String::as_str).unwrap_or("host"), iface);
        let uptime = self
            .uptime_base
            .map(|b| fmt_uptime(b + self.started.elapsed().as_secs()))
            .unwrap_or_else(|| "—".to_string());
        let router = if router_up {
            format!("running · :{}", self.proxy_port)
        } else {
            "down".to_string()
        };
        // System-proxy state only (on/other/off) — NOT the router; the interface
        // is already shown in `mode`, so no suffix here. Refreshed on a background
        // thread so a `rowt proxy on/off` shows up within ~2s.
        self.ensure_sysproxy_watcher();
        let proxy = self.sysproxy.lock().map(|s| s.clone()).unwrap_or_else(|_| "off".to_string());

        Snapshot {
            identity: Identity {
                mode,
                uptime,
                server_name: if h.active.is_empty() { "—".into() } else { h.active.clone() },
                server_ms: h.active_ms,
                router,
                router_up,
                active_ok: h.active_ok,
                proxy,
                config: if config_ok { "host.json OK".into() } else { "host.json ERR".into() },
                name_reserve,
            },
            all,
            lanes,
            conns,
            transient,
            persistent,
            blocked,
            errors,
            servers_total: h.total,
            servers_up: h.up,
            servers_down: h.down,
            active_server: h.active,
            chips: h.chips,
        }
    }
}

// ---------------- helpers ----------------

/// Run one clash delay test for a server through the tunnel (like `rowt ping`):
/// `GET /proxies/{tag}/delay`. Returns the RTT in ms, or None on failure.
fn clash_delay(port: u16, secret: Option<&str>, tag: &str, url: &str, timeout_ms: u32) -> Option<u32> {
    let enc = url_encode(url);
    let path = format!("http://127.0.0.1:{}/proxies/{}/delay?timeout={}&url={}", port, tag, timeout_ms, enc);
    // The HTTP call must outlast clash's own delay timeout.
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(timeout_ms as u64 + 3000))
        .build();
    let mut req = agent.get(&path);
    if let Some(s) = secret {
        req = req.set("Authorization", &format!("Bearer {}", s));
    }
    let v: Value = req.call().ok()?.into_json().ok()?;
    let d = v.get("delay")?.as_u64()? as u32;
    (d > 0).then_some(d)
}

/// Percent-encode a URL for use as a query-string value (no extra deps).
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// The `escape` selector's member tags (the server pool), plus the tag itself.
fn escape_tags_of(host: &Value) -> HashSet<String> {
    let mut tags: HashSet<String> = HashSet::new();
    tags.insert("escape".to_string());
    if let Some(obs) = host.get("outbounds").and_then(|o| o.as_array()) {
        for o in obs {
            if o.get("tag").and_then(|t| t.as_str()) == Some("escape") {
                if let Some(m) = o.get("outbounds").and_then(|m| m.as_array()) {
                    tags.extend(m.iter().filter_map(|s| s.as_str().map(str::to_string)));
                }
            }
        }
    }
    tags
}

/// Read the current server pool straight from `host.json` (used by the prober
/// each round so it tracks config changes without a restart).
fn read_escape_members(cfg: &std::path::Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(cfg.join("host.json")) else {
        return Vec::new();
    };
    let host: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    escape_tags_of(&host).into_iter().filter(|t| t != "escape").collect()
}

fn read_clash_secret(cfg: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(cfg.join("state")).ok()?;
    parse::parse_state(&text).get("clash_secret").cloned()
}

/// System-proxy state for the identity band, mirroring exactly what
/// `rowt proxy status` reports (so the two never disagree): "on" if a proxy on
/// the **active network service** points at 127.0.0.1:port (rowt), "other" if
/// some other proxy is enabled there, else "off".
///
/// We deliberately query `networksetup` on the active service rather than
/// `scutil --proxy`, because `scutil` reports the *primary* service's merged
/// view — which can show an unrelated app's proxy (e.g. Surge on :6152) instead
/// of the per-service setting `rowt proxy on/off` actually toggles.
fn read_system_proxy(port: u16) -> &'static str {
    let Some(svc) = active_service() else { return "off" };
    let port_s = port.to_string();
    let mut any = false;
    // rowt sets all three (socks/web/securewebproxy); checking securewebproxy +
    // socks is enough to classify (and to distinguish rowt from another proxy).
    for flag in ["-getsecurewebproxy", "-getsocksfirewallproxy"] {
        let Ok(out) = std::process::Command::new("networksetup").arg(flag).arg(&svc).output() else {
            continue;
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let (mut en, mut server, mut pt) = (false, "", "");
        for line in text.lines() {
            if let Some(v) = line.strip_prefix("Enabled: ") {
                en = v.trim() == "Yes";
            } else if let Some(v) = line.strip_prefix("Server: ") {
                server = v.trim();
            } else if let Some(v) = line.strip_prefix("Port: ") {
                pt = v.trim();
            }
        }
        if en {
            any = true;
            if server == "127.0.0.1" && pt == port_s {
                return "on";
            }
        }
    }
    if any {
        "other"
    } else {
        "off"
    }
}

/// The network-service name (e.g. "Wi-Fi") carrying the default route, matching
/// rowt's `active_service`. Honours ROWT_IFACE like rowt's `detect_iface`.
fn active_service() -> Option<String> {
    let iface = std::env::var("ROWT_IFACE").ok().or_else(default_route_iface)?;
    let out = std::process::Command::new("networksetup").arg("-listnetworkserviceorder").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    // Blocks look like: "(Hardware Port: Wi-Fi, Device: en0)".
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("(Hardware Port: ") {
            if let Some((port, dev)) = rest.split_once(", Device: ") {
                if dev.trim_end_matches(')') == iface {
                    return Some(port.to_string());
                }
            }
        }
    }
    None
}

/// Interface carrying the default route (`route -n get default`).
fn default_route_iface() -> Option<String> {
    let out = std::process::Command::new("route").args(["-n", "get", "default"]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().find_map(|l| l.trim().strip_prefix("interface: ").map(|s| s.trim().to_string()))
}

/// Local UTC offset in seconds, from `date +%z` (e.g. `+0800` → 28800). Used to
/// express the system clock in the same local civil-seconds frame the lane-log
/// timestamps use (they're written in local time with no offset). 0 on failure
/// (falls back to the newest-event reference — the pre-fix behavior).
fn local_tz_offset_secs() -> i64 {
    let Ok(out) = std::process::Command::new("date").arg("+%z").output() else { return 0 };
    let s = String::from_utf8_lossy(&out.stdout);
    let s = s.trim();
    // [+-]HHMM
    if s.len() >= 5 && s.is_char_boundary(1) {
        let sign = if s.starts_with('-') { -1 } else { 1 };
        let d = &s[1..];
        if let (Ok(hh), Ok(mm)) = (d[0..2].parse::<i64>(), d[2..4].parse::<i64>()) {
            return sign * (hh * 3600 + mm * 60);
        }
    }
    0
}

/// Wall-clock now in the lane logs' local civil-seconds frame.
fn wall_now_civil(offset_secs: i64) -> i64 {
    match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64 + offset_secs,
        Err(_) => 0,
    }
}

/// First non-empty stderr line (sans a leading `error:`), for a control toast.
fn first_err_line(stderr: &[u8], fallback: &str) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.trim_start_matches("error:").trim().to_string())
        .unwrap_or_else(|| fallback.to_string())
}

fn config_dir() -> PathBuf {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(x).join("rowt")
    } else if let Some(h) = std::env::var_os("HOME") {
        PathBuf::from(h).join(".config").join("rowt")
    } else {
        PathBuf::from(".config/rowt")
    }
}

fn env_port(key: &str, default: u16) -> u16 {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn host_bind_iface(host: &Value) -> Option<String> {
    let obs = host.get("outbounds")?.as_array()?;
    for o in obs {
        if let Some(b) = o.get("bind_interface").and_then(|b| b.as_str()) {
            return Some(b.to_string());
        }
    }
    None
}

/// Read the region `[start, EOF)` (capped) of a lane log and parse its complete
/// lines. Returns the events and the new offset (advanced only past the last
/// newline, so a partial trailing line is re-read next time). `skip_leading`
/// drops a partial first line (used on the first, tail-seeked read).
fn read_lane_incremental(path: &std::path::Path, start: u64, skip_leading: bool, lane: Lane) -> Option<(Vec<ErrEvent>, u64)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut data = Vec::new();
    f.take(MAX_READ_BYTES).read_to_end(&mut data).ok()?;
    let text = String::from_utf8_lossy(&data);
    let begin = if skip_leading {
        text.find('\n').map(|i| i + 1).unwrap_or_else(|| text.len())
    } else {
        0
    };
    let end = text.rfind('\n').map(|i| i + 1).unwrap_or(begin);
    let mut evs = Vec::new();
    if end > begin {
        for line in text[begin..end].lines() {
            if let Some(e) = parse::parse_lane_line(line, lane) {
                evs.push(e);
            }
        }
    }
    Some((evs, start + end as u64))
}

fn fmt_uptime(secs: u64) -> String {
    let d = secs / 86400;
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    if d > 0 {
        format!("{}d {}h", d, h)
    } else if h > 0 {
        format!("{}h {}m", h, m)
    } else {
        format!("{}m", m)
    }
}

/// Best-effort proxy process uptime (seconds), via `ps` on the sing-box process.
fn proxy_uptime_secs() -> Option<u64> {
    let out = std::process::Command::new("pgrep").arg("-f").arg("sing-box").output().ok()?;
    let pid = String::from_utf8_lossy(&out.stdout).lines().next()?.trim().to_string();
    if pid.is_empty() {
        return None;
    }
    let out = std::process::Command::new("ps").args(["-o", "etime=", "-p", &pid]).output().ok()?;
    parse_etime(String::from_utf8_lossy(&out.stdout).trim())
}

/// Parse `ps` etime `[[dd-]hh:]mm:ss` into seconds.
fn parse_etime(s: &str) -> Option<u64> {
    let (days, hms) = match s.split_once('-') {
        Some((d, rest)) => (d.parse::<u64>().ok()?, rest),
        None => (0, s),
    };
    let parts: Vec<u64> = hms.split(':').map(|p| p.parse().unwrap_or(0)).collect();
    let (h, m, sec) = match parts.as_slice() {
        [h, m, s] => (*h, *m, *s),
        [m, s] => (0, *m, *s),
        _ => return None,
    };
    Some(days * 86400 + h * 3600 + m * 60 + sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etime_parse() {
        assert_eq!(parse_etime("05:10"), Some(310));
        assert_eq!(parse_etime("03:15:20"), Some(3 * 3600 + 15 * 60 + 20));
        assert_eq!(parse_etime("2-03:15:20"), Some(2 * 86400 + 3 * 3600 + 15 * 60 + 20));
    }

    #[test]
    fn uptime_format() {
        assert_eq!(fmt_uptime(3 * 3600 + 15 * 60), "3h 15m");
        assert_eq!(fmt_uptime(45 * 60), "45m");
        assert_eq!(fmt_uptime(2 * 86400 + 3 * 3600), "2d 3h");
    }
}
