//! Real adapters over the six data sources (README "Data provenance"):
//! clash API (`/traffic`, `/connections`, `/proxies`), `host.json`,
//! `state`/`servers.json`, `lane-*.log`, and host system facts.
//!
//! Every field degrades gracefully: if the clash API is unreachable the binary
//! still renders from config/state/logs; if there is no rowt config at all it
//! falls back to the demo fixture so `rowt-monitor` always shows *something*.

use std::collections::{HashMap, HashSet};

use crate::source::parse::BlockBuckets;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;

use crate::model::*;
use crate::source::parse::{self, ErrAgg, ErrEvent, RawConn};
use crate::source::{FixtureSource, Source};

/// Server latency results shared with the background prober: tag -> (delay ms,
/// or None when no sample answered, the slow one included; when the samples
/// completed).
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
    /// See `monitor_stale()`: checked once a minute, not thirty times.
    stale_checked_at: Option<Instant>,
    monitor_stale: Option<String>,

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
    prev_pool_mtime: Option<SystemTime>,
    pending_pool_probe: bool,
    sub_result_mtime: Arc<Mutex<Option<SystemTime>>>,
    host_cache: Option<(SystemTime, HostInfo)>, // re-parse host.json only on mtime change
    delays: Delays,
    last_probe: Arc<Mutex<Option<Instant>>>,
    prober_started: bool,
    probe_interval: Duration,
    probe_tx: Option<SyncSender<()>>, // wake an idle prober without queueing requests
    prev_router_up: bool,         // detect router down->up (reload / net switch)
    api_fail_streak: u32,         // consecutive ticks on which neither /connections nor /version answered
    last_conns_json: Option<Value>, // the last /connections answer, shown again through a slow tick
    last_force: Option<Instant>,  // throttle self-heal re-probes
    force_retries: u32,           // forced rounds in a row while the active server's probe keeps failing
    last_active_traffic: Option<Instant>, // last tick the escape lane moved bytes (health override)

    // System-proxy state ("on"/"other"/"off"), refreshed on a background thread
    // (networksetup is ~100ms; the poll runs on the input loop, so we must not
    // block it). `rowt proxy on/off` shows up within ~2s.
    sysproxy: Arc<Mutex<String>>,
    sysproxy_started: bool,

    // Control layer: outcomes of `rowt` commands run off the UI thread, drained
    // by the app each tick into a footer toast (CONTROLS.md §4, §9.2).
    ctl_out: Arc<Mutex<Vec<crate::source::CtlOutcome>>>,
    // Controls that may restart the router (`use`, the batched reload) still
    // running — see `restart_in_flight`.
    restarts: Arc<AtomicUsize>,
}

/// One count in `LiveSource::restarts`, held by the command's thread for as long
/// as the command runs. The count drops however that ends — the thread returns,
/// panics, or never starts (a failed spawn drops the closure, and this with it).
struct InFlight(Arc<AtomicUsize>);

impl InFlight {
    fn new(count: &Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        InFlight(Arc::clone(count))
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl LiveSource {
    pub fn new() -> Self {
        let cfg = config_dir();
        let clash_port = env_port("ROWT_CLASH_PORT", 9090);
        let proxy_port = env_port("ROWT_PORT", 7890);
        // Probe every 10 minutes by default (override with ROWT_MONITOR_PROBE_INTERVAL secs).
        let probe_interval = Duration::from_secs(env_port("ROWT_MONITOR_PROBE_INTERVAL", 600).max(5) as u64);
        LiveSource {
            prev_pool_mtime: modified(&cfg.join("servers.json")),
            sub_result_mtime: Arc::new(Mutex::new(modified(&cfg.join("sub-update.result")))),
            pending_pool_probe: false,
            cfg,
            clash_port,
            proxy_port,
            fallback: FixtureSource::new(),
            prev: HashMap::new(),
            prev_at: None,
            stale_checked_at: None,
            monitor_stale: None,
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
            last_probe: Arc::new(Mutex::new(None)),
            prober_started: false,
            probe_interval,
            probe_tx: None,
            prev_router_up: true,
            api_fail_streak: 0,
            last_conns_json: None,
            last_force: None,
            force_retries: 0,
            last_active_traffic: None,
            // Seed synchronously so the first frame is correct (no "off" flash).
            sysproxy: Arc::new(Mutex::new(read_system_proxy(proxy_port).to_string())),
            sysproxy_started: false,
            ctl_out: Arc::new(Mutex::new(Vec::new())),
            restarts: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// A monitor that has outlived its install (see `detect_stale_monitor`).
    /// Once a minute: a readlink is cheap, but there is no reason to do it on
    /// every 2s data tick.
    fn monitor_stale(&mut self) -> Option<String> {
        if self.stale_checked_at.is_some_and(|t| t.elapsed() < Duration::from_secs(60)) {
            return self.monitor_stale.clone();
        }
        self.stale_checked_at = Some(Instant::now());
        self.monitor_stale = detect_stale_monitor();
        self.monitor_stale.clone()
    }

    /// Run a `rowt` command off the UI thread and queue its outcome for the
    /// footer toast. Uses `$ROWT_BIN` (exported by `rowt monitor`) so the control
    /// drives the exact CLI + config the monitor was launched from. `restarts`:
    /// the command may restart the router, so it counts as in flight until it exits.
    fn spawn_rowt(&self, args: Vec<String>, ok: String, restarts: bool) {
        let out = Arc::clone(&self.ctl_out);
        let subscriptions = args == ["sub", "update"];
        let sub_result = self.cfg.join("sub-update.result");
        let sub_result_mtime = Arc::clone(&self.sub_result_mtime);
        let in_flight = restarts.then(|| InFlight::new(&self.restarts));
        std::thread::Builder::new()
            .name("rowt-monitor-ctl".into())
            .spawn(move || {
                let _in_flight = in_flight;
                let bin = std::env::var("ROWT_BIN").unwrap_or_else(|_| "rowt".to_string());
                let outcome = match std::process::Command::new(&bin).args(&args).output() {
                    Ok(o) if o.status.success() => crate::source::CtlOutcome::Ok(if subscriptions {
                        subscription_message(&String::from_utf8_lossy(&o.stdout)).unwrap_or(ok)
                    } else { ok }),
                    Ok(o) => {
                        let err = String::from_utf8_lossy(&o.stderr);
                        let mut lines = err.lines().map(str::trim).filter(|l| !l.is_empty());
                        let line = if subscriptions { lines.next_back() } else { lines.next() };
                        let msg = line
                            .map(|l| l.trim_start_matches("error:").trim().to_string())
                            .unwrap_or_else(|| format!("rowt {} failed", args.join(" ")));
                        crate::source::CtlOutcome::Err(msg)
                    }
                    Err(e) => crate::source::CtlOutcome::Err(format!("{bin}: {e}")),
                };
                if subscriptions {
                    if let Ok(mut stamp) = sub_result_mtime.lock() { *stamp = modified(&sub_result); }
                }
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
        let last_probe = Arc::clone(&self.last_probe);
        let cfg = self.cfg.clone();
        let port = self.clash_port;
        // Probe target: Google's generate_204 (blocked in CN when direct, so it
        // reaches 204 only *through* a working escape) — this tests real escape
        // reachability, matching rowt's auto-select urltest. Override via env.
        let url = std::env::var("ROWT_PING_URL").unwrap_or_else(|_| "https://www.gstatic.com/generate_204".to_string());
        let interval = self.probe_interval;
        let (tx, rx) = mpsc::sync_channel::<()>(0);
        self.probe_tx = Some(tx);
        std::thread::Builder::new()
            .name("rowt-monitor-prober".into())
            .spawn(move || loop {
                // Re-read the pool and secret from config each round so added
                // servers / subscription updates / a rotated secret are picked
                // up automatically (immediate first round, then wait).
                let members = read_escape_members(&cfg);
                let secret = read_clash_secret(&cfg);
                let (finished, round) = probe_round_rows(&members, &delays,
                    |tag, budget_ms| clash_delay(port, secret.as_deref(), tag, &url, budget_ms));
                write_latency_table(&cfg, &round);
                if let Ok(mut at) = last_probe.lock() { *at = finished; }
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
            // The server pool only — `escape` keeps the selector and `auto` for
            // lane classification (see `is_pool_member`).
            members: escape.iter().filter(|t| is_pool_member(t)).cloned().collect(),
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
    fn servers(&self, state: &HashMap<String, String>, router_up: bool, auto_now: Option<String>) -> Health {
        let selected = state.get("selected").cloned().unwrap_or_default();
        if !router_up {
            // The pool is still useful when the router cannot provide readings.
            return health_view(&self.escape_members, selected, None, |_| None);
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
        health_view(&self.escape_members, selected, auto_now, fresh)
    }

    /// urltest's live pick: `GET /proxies/auto` → `now`, kept only when it names
    /// a pool member (the group's `now` can briefly be empty right after a start).
    fn auto_pick(&self) -> Option<String> {
        let v = self.clash_get(&format!("/proxies/{AUTO_GROUP}"))?;
        let now = v.get("now")?.as_str()?;
        self.escape_members.iter().any(|m| m == now).then(|| now.to_string())
    }

    /// `best`'s live pick: `GET /proxies/escape` → `now`, kept only when it names
    /// a pool member.
    fn escape_pick(&self) -> Option<String> {
        let v = self.clash_get("/proxies/escape")?;
        let now = v.get("now")?.as_str()?;
        self.escape_members.iter().any(|m| m == now).then(|| now.to_string())
    }
}

/// Derive the strip and the active server's health from the pool and each
/// member's probe verdict — `fresh(tag)`: `Some(Some(ms))` up, `Some(None)`
/// down, `None` pending (or stale). Pure, so the rules are testable.
///
/// The *active* server is the one carrying escape traffic: the pinned tag in
/// manual mode, urltest's live pick (`auto_now`) in auto mode. It is marked in
/// the strip, and its own probe drives the header's
/// latency and the LIVE/ERROR dot. Auto with no resolved pick falls back to the
/// pool: the fastest up server stands in for the latency (it is what urltest
/// converges on), and the pool is healthy if any member is up.
fn health_view(members: &[String], selected: String, auto_now: Option<String>, fresh: impl Fn(&str) -> Option<Option<u32>>) -> Health {
    let auto = is_auto_mode(&selected);
    let in_use: Option<String> = if auto { auto_now.clone() } else { Some(selected.clone()) };
    let mut up = 0u32;
    let mut down = 0u32;
    let mut chips = Vec::new();
    for tag in members {
        match fresh(tag) {
            Some(Some(ms)) => {
                up += 1;
                // All up servers appear in the strip; the active one is marked.
                chips.push(Server { down: false, name: tag.clone(), ms: Some(ms), active: in_use.as_deref() == Some(tag.as_str()) });
            }
            Some(None) => {
                down += 1;
                chips.push(Server { down: true, name: tag.clone(), ms: None, active: in_use.as_deref() == Some(tag.as_str()) });
            }
            None => {
                chips.push(Server { down: false, name: tag.clone(), ms: None, active: in_use.as_deref() == Some(tag.as_str()) });
            }
        }
    }
    // Keep the source's active-first order; the UI sorts by health and latency.
    chips.sort_by_key(|c| (!c.active, c.ms.is_none(), c.ms));

    let probed = |up: u32, down: u32| if up > 0 { Some(true) } else if down > 0 { Some(false) } else { None };
    let (active_ms, active_ok) = match in_use.as_deref() {
        Some(tag) => match fresh(tag) {
            Some(Some(ms)) => (Some(ms), Some(true)),
            Some(None) => (None, Some(false)),
            None => (None, None), // not yet probed — don't alarm
        },
        None => (chips.iter().filter_map(|c| c.ms).min(), probed(up, down)),
    };
    Health { total: members.len() as u32, up, down, active: selected, auto_now, chips, active_ms, active_ok }
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
    auto_now: Option<String>,
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

    fn history(&mut self, spans: [i64; 4], lane: Option<Lane>) -> crate::source::History {
        let Some(conn) = crate::metrics::open_read(&crate::metrics::db_path()) else {
            return crate::source::History::new();
        };
        let now = SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let lane_s = lane.map(|l| l.label());
        crate::metrics::history_map(&conn, now, spans, lane_s).unwrap_or_default()
    }

    fn force_probe(&self) -> Result<(), &'static str> {
        request_probe(self.probe_tx.as_ref())
    }

    fn update_subscriptions(&self) -> Result<(), &'static str> {
        if self.restart_in_flight() { return Err("update or router restart in progress"); }
        self.spawn_rowt(vec!["sub".into(), "update".into()], "subscriptions updated".into(), true);
        Ok(())
    }

    fn use_server(&self, tag: &str) {
        // In flight even pin → pin: that is a live clash switch, but `rowt use`
        // falls back to a restart when the switch fails (e.g. mid-restart).
        self.spawn_rowt(vec!["use".into(), tag.into()], format!("escape → {tag}"), true);
    }

    fn route_lane(&self, domain: &str, lane: Lane) {
        let verb = lane.label(); // escape | corp | block | (direct never routed here)
        self.spawn_rowt(
            vec![verb.into(), "add".into(), domain.into(), "--no-reload".into()],
            format!("{domain} → {verb}"),
            false,
        );
    }

    fn route_hotspot(&self, domain: &str) {
        self.spawn_rowt(
            vec!["hotspot".into(), "add".into(), domain.into(), "--no-reload".into()],
            format!("{domain} → hotspot"),
            false,
        );
    }

    fn unroute(&self, domain: &str) {
        // Single-lane invariant means the domain is in at most one lane, but we
        // don't track which from an errors row — remove from all four (a miss is
        // a harmless "not found"), so the result is always "back to direct".
        //
        // Hotspot only when its file exists: the three routing lanes are seeded
        // from templates, that one is created by the first `hotspot add`, and
        // `rm` on a lane that has no file dies ("hotspot list is empty") — which
        // writes an ABORT audit line, one per `d` press, on every machine that
        // never touched the lane.
        let out = Arc::clone(&self.ctl_out);
        let (bin_domain, ok) = (domain.to_string(), format!("{domain} → direct"));
        let mut lanes = vec!["escape", "corp", "block"];
        if self.cfg.join("hotspot-domains.txt").is_file() {
            lanes.push("hotspot");
        }
        std::thread::Builder::new()
            .name("rowt-monitor-ctl".into())
            .spawn(move || {
                let bin = std::env::var("ROWT_BIN").unwrap_or_else(|_| "rowt".to_string());
                let mut err: Option<String> = None;
                for lane in lanes {
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
            false,
        );
    }

    fn reload_router(&self) {
        // Lane edits were written with --no-reload; apply them ONCE now. We do
        // render + router restart (the lightweight lane-reload) rather than
        // `rowt reload`, which would also re-assert the system proxy and fight
        // the `o` toggle / captive-portal flow (CONTROLS.md §4.3 deviation).
        // Since the lanes became rule-sets the router watches (rowt #s54), the
        // render alone reaches a router that runs what it rendered, and the
        // restart is skipped (rowt #s56).
        let out = Arc::clone(&self.ctl_out);
        let in_flight = InFlight::new(&self.restarts);
        let cfg = self.cfg.clone();
        std::thread::Builder::new()
            .name("rowt-monitor-ctl".into())
            .spawn(move || {
                let _in_flight = in_flight;
                let bin = std::env::var("ROWT_BIN").unwrap_or_else(|_| "rowt".to_string());
                let render = std::process::Command::new(&bin).arg("render").output();
                let outcome = match render {
                    Ok(o) if o.status.success() && router_runs_rendered(&cfg) => {
                        crate::source::CtlOutcome::Ok("lane edits applied live — router not restarted".into())
                    }
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

    fn restart_in_flight(&self) -> bool {
        self.restarts.load(Ordering::SeqCst) > 0
    }

    fn drain_ctl(&self) -> Vec<crate::source::CtlOutcome> {
        self.ctl_out.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default()
    }

    fn poll(&mut self, window: Window, lane: Option<Lane>) -> Snapshot {
        if !self.restart_in_flight() {
            let path = self.cfg.join("sub-update.result");
            let stamp = modified(&path);
            if let Ok(mut previous) = self.sub_result_mtime.lock() {
                if stamp.is_some() && stamp != *previous {
                    if let Ok(message) = std::fs::read_to_string(path) {
                        let message = message.trim();
                        let outcome = if let Some(error) = message.strip_prefix("error: ") {
                            crate::source::CtlOutcome::Err(error.to_string())
                        } else { crate::source::CtlOutcome::Ok(message.to_string()) };
                        if let Ok(mut out) = self.ctl_out.lock() { out.push(outcome); }
                    }
                }
                *previous = stamp;
            }
        }
        // No rowt config at all -> demo fixture so we always render something.
        let Some(info) = self.host_info() else {
            return self.fallback.poll(window, lane);
        };
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
        let pool_mtime = modified(&self.cfg.join("servers.json"));
        self.pending_pool_probe |= pool_changed || pool_mtime != self.prev_pool_mtime;
        self.prev_pool_mtime = pool_mtime;

        // Connections + throughput from clash. The verdict on the ROUTER is not
        // this one call: /connections is the heavy endpoint, and its 400 ms
        // budget is a tail-latency risk (a large table under load, a busy Mac).
        // One slow answer used to flip the band to "down · wedged" for a tick
        // while traffic flowed — and an operator restarted a healthy router.
        // Read the router process stats every poll — NOT a one-shot base sampled
        // at launch plus the monitor's own runtime. A reload/recovery spawns a
        // NEW sing-box, so the counter must track the current process (resetting
        // on a restart). None = no sing-box process running.
        let sb = sing_box_stat();
        let mut conns_json = self.clash_get("/connections");
        let api_ok = conns_json.is_some();
        // Only when the heavy call failed with a process alive: a tiny request
        // that answers says the API is alive but slow.
        let version_ok = !api_ok && sb.is_some() && self.clash_get("/version").is_some();
        let (router_up, api_note) = router_verdict(api_ok, version_ok, sb.is_some(), &mut self.api_fail_streak);
        if api_ok {
            self.last_conns_json = conns_json.clone();
        } else if router_up {
            // Slow, not down: keep showing the last answer rather than an empty
            // table for a tick (same totals, so the rates read as zero deltas).
            conns_json = self.last_conns_json.clone();
        } else {
            self.last_conns_json = None;
        }
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
            if self.pending_pool_probe && !self.restart_in_flight() && !self.cfg.join("sub-update.lock").exists() && self.force_probe().is_ok() {
                self.pending_pool_probe = false;
            }
        }

        let (transient, persistent, blocked, errors) = self.errors(window, lane);
        // In auto mode the server in use is urltest's call, not the state file's:
        // one localhost read per tick, and only while auto is on.
        let auto_now = match state.get("selected").map(String::as_str) {
            Some(AUTO_GROUP) if router_up => self.auto_pick(),
            // `best` has no urltest group: the escape selector carries the pick.
            Some(BEST_MODE) if router_up => self.escape_pick(),
            _ => None,
        };
        let mut h = self.servers(&state, router_up, auto_now);

        // Ground truth beats the synthetic probe. The health dot goes ERROR when
        // the active server's `generate_204` delay probe fails — but some servers
        // can't reach gstatic yet proxy real sites fine, so a failed probe while
        // the escape lane is actively moving bytes is a false alarm. Note when the
        // escape lane has throughput and treat that as "live" for a short window
        // (so a lull between requests doesn't flip the dot), then let it override a
        // *failed* probe. Pending (None) and healthy (Some(true)) are left alone.
        let escape_rate = lanes.iter().find(|l| l.lane == Lane::Escape).map(|l| l.up + l.down).unwrap_or(0.0);
        if escape_rate > 0.0 {
            self.last_active_traffic = Some(Instant::now());
        }
        let traffic_recent = self.last_active_traffic.is_some_and(|t| t.elapsed() < Duration::from_secs(20));
        // Capture the RAW probe verdict before the display override, so the
        // self-heal below still re-probes a genuinely-failed probe (the override
        // would otherwise mask it and suppress recovery).
        let probe_failed = h.active_ok == Some(false);
        h.active_ok = health_with_traffic(h.active_ok, traffic_recent);

        // Self-heal a stale probe. The prober runs on a slow interval (default
        // 10 min), so a transient failure — a network blip / switch, the active
        // server's last pre-switch probe — would otherwise show ERROR (and a `—`
        // latency) until the next scheduled round even after the path is back.
        // Live escape traffic is strong evidence the path IS back, so when the
        // probe reads down *and* bytes are flowing we retry on a short 15s window
        // (this also refreshes the active server's latency and the whole strip);
        // with no traffic to corroborate, fall back to the slow 60s retry. A
        // just-recovered router always re-probes immediately.
        if router_up {
            let recovered = !self.prev_router_up;
            let since = self.last_force.map(|t| t.elapsed());
            if recovered || !probe_failed {
                self.force_retries = 0;
            }
            if should_force_probe(probe_failed, traffic_recent, recovered, since, self.force_retries, self.probe_interval) {
                let _ = self.force_probe();
                self.last_force = Some(Instant::now());
                if probe_failed && !recovered {
                    self.force_retries = self.force_retries.saturating_add(1);
                }
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
        let uptime = sb.map(|(s, _)| fmt_uptime(s)).unwrap_or_else(|| "—".to_string());
        // Router line: "running" carries CPU% (a spinning/wedged sing-box shows
        // ~100%+) — or "api slow" while the heavy call is not answering but the
        // process and the cheap probe are — and "down" carries WHY, so a glance
        // says what to fix:
        //   wedged  — process alive but the clash API stopped answering for good
        //             (three ticks, see router_verdict)
        //   config  — host.json isn't valid, so it can't start
        //   stopped — no process at all (crashed / never started)
        let (router, router_cpu, router_reason) = if router_up {
            ("running".to_string(), sb.map(|(_, c)| c), api_note.to_string())
        } else if sb.is_some() {
            ("down".to_string(), None, "wedged".to_string())
        } else if !info.config_ok {
            ("down".to_string(), None, "config".to_string())
        } else {
            ("down".to_string(), None, "stopped".to_string())
        };
        // System-proxy state only (on/other/off) — NOT the router; the interface
        // is already shown in `mode`, so no suffix here. Refreshed on a background
        // thread so a `rowt proxy on/off` shows up within ~2s.
        self.ensure_sysproxy_watcher();
        let proxy = self.sysproxy.lock().map(|s| s.clone()).unwrap_or_else(|_| "off".to_string());
        let (watch, watch_age) = read_watch_status(&self.cfg);
        let monitor_stale = self.monitor_stale();
        // The header names the server carrying traffic: auto's pick when it is
        // known, else the selection itself (`auto` until the first read lands).
        let server_name = match (h.active.as_str(), h.auto_now.as_deref()) {
            ("", _) => "—".to_string(),
            (sel, Some(pick)) if is_auto_mode(sel) => pick.to_string(),
            (sel, _) => sel.to_string(),
        };

        Snapshot {
            identity: Identity {
                mode,
                uptime,
                server_name,
                server_ms: h.active_ms,
                router,
                router_cpu,
                router_reason,
                router_up,
                active_ok: h.active_ok,
                proxy,
                watch,
                watch_age,
                collector: read_collector_status(router_up),
                name_reserve,
            },
            monitor_stale,
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
            probe_age: self.last_probe.lock().ok().and_then(|at| at.map(|t| t.elapsed().as_secs())),
            active_server: h.active,
            auto_now: h.auto_now,
            chips: h.chips,
        }
    }
}

// ---------------- helpers ----------------

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

fn subscription_message(stdout: &str) -> Option<String> {
    stdout.lines().map(str::trim).find(|line| line.starts_with("subscriptions ")).map(str::to_string)
}

fn request_probe(tx: Option<&SyncSender<()>>) -> Result<(), &'static str> {
    // The zero-capacity channel only accepts a request while the worker is
    // waiting between rounds; a busy round cannot accumulate another one.
    tx.ok_or("probe unavailable")?.try_send(()).map_err(|e| match e {
        mpsc::TrySendError::Full(()) => "previous probe still running…",
        mpsc::TrySendError::Disconnected(()) => "probe unavailable",
    })
}

/// The budget of one fast delay sample, in ms. Three of these measure a server
/// on an ordinary link.
const PROBE_BUDGET_MS: u32 = 5000;
/// The budget of the one slow sample taken when every fast sample ran out of
/// budget: it tells a slow link (the figure comes back, seconds long) from a
/// dead server (this one times out too). Seen 2026-10-19 on an in-flight link
/// with ~800 ms round trips: every fast sample timed out and all 22 servers
/// read "down" while the escape lane carried bytes; a 15 s sample read 7.2 s.
/// The clash API parses the budget as a 16-bit number, so keep it under 32768.
const SLOW_PROBE_BUDGET_MS: u32 = 15_000;

/// One delay sample's outcome: the figure; a budget overrun (the server may
/// just be slower than the budget — clash's 504, or our own HTTP timeout); or
/// a failure no budget would fix (the test URL refused: 503; no such tag: 404;
/// a zero delay).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sample {
    Ms(u32),
    Timeout,
    Failed,
}

#[cfg(test)]
fn probe_round(members: &[String], delays: &Delays, probe: impl Fn(&str, u32) -> Sample + Sync) -> Option<Instant> {
    probe_round_rows(members, delays, probe).0
}

/// `probe_round`, also returning each server's (tag, figure, samples answered)
/// — the rows the prober writes into rowt's shared latency table. `probe` takes
/// the tag and a budget in ms: three fast samples, then — only when all three
/// ran out of budget — one slow sample, whose figure stands alone.
fn probe_round_rows(members: &[String], delays: &Delays, probe: impl Fn(&str, u32) -> Sample + Sync)
    -> (Option<Instant>, Vec<(String, Option<u32>, usize)>) {
    let next = AtomicUsize::new(0);
    let rows: Mutex<Vec<(String, Option<u32>, usize)>> = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..members.len().min(10) {
            scope.spawn(|| {
                while let Some(tag) = members.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let fast: Vec<Sample> = (0..3).map(|_| probe(tag, PROBE_BUDGET_MS)).collect();
                    let mut samples: Vec<u32> = fast.iter().filter_map(|s| match s { Sample::Ms(v) => Some(*v), _ => None }).collect();
                    if samples.is_empty() && fast.iter().all(|s| *s == Sample::Timeout) {
                        if let Sample::Ms(v) = probe(tag, SLOW_PROBE_BUDGET_MS) {
                            samples.push(v);
                        }
                    }
                    samples.sort_unstable();
                    let ms = match samples.as_slice() {
                        [] => None,
                        [a, b] => Some(((*a as u64 + *b as u64) / 2) as u32),
                        values => Some(values[values.len() / 2]),
                    };
                    if let Ok(mut m) = delays.lock() {
                        m.insert(tag.clone(), (ms, Instant::now()));
                    }
                    if let Ok(mut r) = rows.lock() {
                        r.push((tag.clone(), ms, samples.len()));
                    }
                }
            });
        }
    });
    ((!members.is_empty()).then(Instant::now), rows.into_inner().unwrap_or_default())
}

/// Fold a probe round into rowt's shared latency table (`latency.tsv` in the
/// config directory), which `rowt use best` switches by and `rowt ping` writes
/// too — so with the monitor open, the watchdog finds these figures fresh and
/// chooses from the numbers on screen. The format is rowt_core::latency's:
/// `tag\tmedian (empty = no answer)\tsamples answered\tUTC time`, byte-sorted
/// under one header line, replaced whole (0600). This round's servers are
/// replaced; every other row is kept (pruning to the pool is rowt's job).
fn write_latency_table(cfg: &std::path::Path, round: &[(String, Option<u32>, usize)]) {
    if round.is_empty() {
        return;
    }
    let now = std::process::Command::new("date").args(["-u", "+%Y-%m-%d %H:%M:%S"])
        .stderr(std::process::Stdio::null()).output().ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    if now.is_empty() {
        return;
    }
    let path = cfg.join("latency.tsv");
    let digits = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
    let mut rows: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for l in std::fs::read_to_string(&path).unwrap_or_default().lines() {
        let f: Vec<&str> = l.split('\t').collect();
        if l.starts_with('#') || f.len() != 4 || f[0].is_empty() || f[3].is_empty() { continue }
        if (!f[1].is_empty() && !digits(f[1])) || !digits(f[2]) { continue }
        let m = if f[1].is_empty() { String::new() } else { f[1].parse::<u64>().map(|v| v.to_string()).unwrap_or_default() };
        let n = f[2].parse::<u64>().unwrap_or(0);
        rows.insert(f[0].to_string(), format!("{}\t{m}\t{n}\t{}", f[0], f[3]));
    }
    for (tag, ms, n) in round {
        let m = ms.map(|v| v.to_string()).unwrap_or_default();
        rows.insert(tag.clone(), format!("{tag}\t{m}\t{n}\t{now}"));
    }
    let mut body = String::from("# rowt latency table: tag, median ms (empty = no answer), samples answered, UTC time\n");
    for line in rows.values() {
        body.push_str(line);
        body.push('\n');
    }
    let tmp = cfg.join(format!(".latency.tsv.monitor.{}", std::process::id()));
    if std::fs::write(&tmp, body).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    if std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Run one clash delay test for a server through the tunnel (like `rowt ping`):
/// `GET /proxies/{tag}/delay`, with `timeout_ms` as clash's budget. The RTT in
/// ms, or why there is none: clash answers 504 when the test outran the budget
/// and 503 when it failed outright (404 for a tag it doesn't know).
fn clash_delay(port: u16, secret: Option<&str>, tag: &str, url: &str, timeout_ms: u32) -> Sample {
    let enc = url_encode(url);
    let path = format!("http://127.0.0.1:{}/proxies/{}/delay?timeout={}&url={}", port, tag, timeout_ms, enc);
    // The HTTP call must outlast clash's own delay timeout, by a wide margin:
    // a slow test's wall time has run ~5 s past the figure it reported.
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(timeout_ms as u64 + 10_000))
        .build();
    let mut req = agent.get(&path);
    if let Some(s) = secret {
        req = req.set("Authorization", &format!("Bearer {}", s));
    }
    match req.call() {
        Ok(resp) => match resp.into_json::<Value>().ok().and_then(|v| v.get("delay")?.as_u64()) {
            Some(d) if d > 0 => Sample::Ms(d.min(u32::MAX as u64) as u32),
            _ => Sample::Failed,
        },
        Err(ureq::Error::Status(504, _)) => Sample::Timeout,
        Err(ureq::Error::Status(_, _)) => Sample::Failed,
        // Our own timeout, or no router to ask: the slow sample settles it
        // either way, and fails as fast as this one did when the router is gone.
        Err(ureq::Error::Transport(_)) => Sample::Timeout,
    }
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

/// Whether an `escape` tag is a server of the pool: not the selector itself, and
/// not the urltest group auto mode lists as its first member — neither is a
/// server, so neither is a chip or probed. (Both stay in the escape tag set:
/// connection chains run through them, and lane classification needs them.)
fn is_pool_member(tag: &str) -> bool {
    tag != "escape" && tag != AUTO_GROUP
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
    escape_tags_of(&host).into_iter().filter(|t| is_pool_member(t)).collect()
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

/// The network-service name (e.g. "Wi-Fi") carrying rowt's system proxy, matching
/// rowt's `active_service`. Honours ROWT_IFACE, else falls back to the physical
/// NIC exactly like rowt's `detect_iface` — NOT the default-route interface,
/// which is a `utunN` when a full-tunnel VPN is up (rowt's core case). A utun has
/// no Hardware Port block here, so mapping it would return None and read the
/// proxy as "off" although rowt set it on the physical service.
fn active_service() -> Option<String> {
    let iface = std::env::var("ROWT_IFACE").ok().or_else(physical_iface)?;
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

/// The physical (non-tunnel) interface — an `enX` with an IPv4 and a router —
/// mirroring rowt's `detect_iface` exactly. This is the service rowt applies the
/// system proxy to, so the monitor must read the same one.
///
/// Prefer the physical NIC carrying the default route (the primary service apps'
/// system-proxy queries resolve to); fall back to the first qualifying `enX`
/// when a VPN owns the default route via a `utun`.
fn physical_iface() -> Option<String> {
    if let Some(def) = default_route_iface() {
        if def.starts_with("en") && iface_up(&def) {
            return Some(def);
        }
    }
    let out = std::process::Command::new("networksetup").arg("-listallhardwareports").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let Some(dev) = line.trim().strip_prefix("Device: ") else { continue };
        let dev = dev.trim();
        if dev.starts_with("en") && iface_up(dev) {
            return Some(dev.to_string());
        }
    }
    None
}

/// An `enX` with both an IPv4 address and a router (real connectivity), matching
/// rowt's `_iface_up`.
fn iface_up(dev: &str) -> bool {
    let has_ip = std::process::Command::new("ipconfig")
        .args(["getifaddr", dev])
        .output()
        .map(|o| o.status.success() && !o.stdout.trim_ascii().is_empty())
        .unwrap_or(false);
    let has_router = std::process::Command::new("ipconfig")
        .args(["getoption", dev, "router"])
        .output()
        .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
        .unwrap_or(false);
    has_ip && has_router
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

/// Does the running router run exactly the host.json `rowt render` just wrote?
/// Then the render alone has put the batched lane edits into it, through the
/// lane rule-sets it watches, and a restart would drop every connection for
/// nothing (rowt #s56). The same test as the shell's `_lanes_apply`: the knob
/// on, a live `host.pid`, and `host.running` (the cksum the router was started
/// from) equal to `cksum < host.json`, from the same tool so the same number.
/// Anything missing or unreadable answers "restart", as before.
fn router_runs_rendered(cfg: &Path) -> bool {
    let knob = std::env::var("ROWT_LANES_HOT").ok().filter(|v| !v.is_empty())
        .or_else(|| std::env::var("ROWT_CORP_HOT").ok().filter(|v| !v.is_empty()))
        .unwrap_or_else(|| "1".into());
    if knob != "1" {
        return false;
    }
    let pid = std::fs::read_to_string(cfg.join("host.pid")).ok()
        .and_then(|s| s.trim().parse::<i32>().ok()).filter(|p| *p > 0);
    if !pid.is_some_and(|p| unsafe { libc::kill(p, 0) } == 0) {
        return false;
    }
    let running = std::fs::read_to_string(cfg.join("host.running")).unwrap_or_default();
    let running = running.trim();
    let Ok(host) = std::fs::File::open(cfg.join("host.json")) else { return false };
    let now = std::process::Command::new("cksum").stdin(std::process::Stdio::from(host)).output();
    match now {
        Ok(o) if o.status.success() => !running.is_empty() && String::from_utf8_lossy(&o.stdout).trim() == running,
        _ => false,
    }
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

/// Active-server health with real traffic as the tie-breaker: a *failed* probe
/// (`Some(false)`) while the escape lane has recently moved bytes is a false alarm
/// (some servers can't reach the probe URL yet proxy real sites fine), so it reads
/// healthy. A healthy probe (`Some(true)`) or a pending one (`None`) is unchanged;
/// a failed probe with no recent traffic stays failed (genuinely unreachable).
fn health_with_traffic(active_ok: Option<bool>, traffic_recent: bool) -> Option<bool> {
    if active_ok == Some(false) && traffic_recent {
        Some(true)
    } else {
        active_ok
    }
}

/// Whether to force an immediate re-probe (bypassing the slow scheduled round).
/// A just-recovered router always re-probes. Otherwise only a *failed* active
/// probe is worth chasing: with live escape traffic (strong evidence the path is
/// back) retry on a short window so a transient failure clears fast and the
/// latency refreshes; without traffic, retry slowly. `since_last` is how long ago
/// we last forced a probe (`None` = never).
/// The self-heal's schedule while the active server's probe keeps failing:
/// the first retry waits 15 s when the escape lane is moving bytes (60 s when
/// it is not), and each retry after that — `retries` so far — waits twice as
/// long, up to `cap` (the probe interval). A transient failure still clears
/// within seconds; a link that stays slow for an hour gets a handful of rounds,
/// not back-to-back rounds of the whole pool, three or four samples a server,
/// on top of the traffic it is already struggling with.
fn should_force_probe(probe_failed: bool, traffic_recent: bool, router_recovered: bool, since_last: Option<Duration>, retries: u32, cap: Duration) -> bool {
    if router_recovered {
        return true;
    }
    if !probe_failed {
        return false;
    }
    let base = if traffic_recent { Duration::from_secs(15) } else { Duration::from_secs(60) };
    let throttle = base.saturating_mul(1u32 << retries.min(16)).min(cap);
    since_last.is_none_or(|d| d > throttle)
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

/// The header's watch cell, from three facts, as (state, age). Pure, so the
/// table below can be tested. `age` is seconds since the last COMPLETED tick —
/// the heartbeat rowt stamps on its way out. `launchctl list` alone only says
/// the job is registered: an agent whose ticks die, or whose router launchd
/// keeps SIGKILLing, reads "on" by that test forever. None = no heartbeat known
/// (an older rowt never writes one), and that reads as `on`, not `stalled` —
/// the day you upgrade, every machine would otherwise read stalled until its
/// first tick lands.
pub fn watch_cell(loaded: bool, installed: bool, age: Option<u64>, interval: u64) -> (&'static str, Option<u64>) {
    if !loaded {
        return (if installed { "off" } else { "\u{2014}" }, None);
    }
    match age {
        Some(a) if a > interval.saturating_mul(2) => ("stalled", Some(a)),
        Some(a) => ("on", Some(a)),
        None => ("on", None),
    }
}

/// Seconds since `watch.tick` was last stamped — the tick's exit heartbeat.
fn heartbeat_age(cfg: &std::path::Path) -> Option<u64> {
    std::fs::metadata(cfg.join("watch.tick")).ok()?.modified().ok()?.elapsed().ok().map(|e| e.as_secs())
}

/// The agent's StartInterval, from the plist launchd actually holds. The stall
/// threshold is twice this; hard-coding 120 would misread a machine that set
/// ROWT_WATCH_INTERVAL. Network changes fire ticks sooner (WatchPaths), never
/// later, so the interval is the longest a healthy agent stays silent.
fn tick_interval() -> u64 {
    let plist = std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join("Library/LaunchAgents/club.annaslife.rowt.watch.plist"));
    let body = plist.and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    body.split("StartInterval").nth(1)
        .and_then(|r| r.split("<integer>").nth(1))
        .and_then(|r| r.split('<').next())
        .and_then(|d| d.trim().parse().ok())
        .unwrap_or(120)
}

/// Watchdog LaunchAgent state for the header, as (state, age): "on" (loaded and
/// ticking), "stalled" (loaded, but no tick has completed in 2x the interval),
/// "off" (installed but not loaded), or "\u{2014}" (not installed). One
/// `launchctl list` call, one plist stat, one heartbeat stat.
fn read_watch_status(cfg: &std::path::Path) -> (String, Option<u64>) {
    const LABEL: &str = "club.annaslife.rowt.watch";
    let loaded = std::process::Command::new("launchctl")
        .args(["list", LABEL])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    let installed = std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join("Library/LaunchAgents").join(format!("{LABEL}.plist")))
        .is_some_and(|p| p.exists());
    let (state, age) = watch_cell(loaded, installed, heartbeat_age(cfg), tick_interval());
    (state.to_string(), age)
}

/// This process's executable against the rowt-monitor on PATH, both
/// canonicalized: a fresh process may report the symlink, and a binary whose
/// install was removed under it (a `brew upgrade` beneath a TUI left open for
/// days) fails to canonicalize at all — that failure IS the signal, not an
/// error. No rowt-monitor on PATH means nothing to compare against.
/// Returns (the path this process was started as, its canonical path while
/// the file still exists, the canonical rowt-monitor on PATH if there is one).
fn monitor_paths() -> Option<(PathBuf, Option<PathBuf>, Option<PathBuf>)> {
    let me = std::env::current_exe().ok()?;
    let mine = std::fs::canonicalize(&me).ok();
    let on_path = std::env::var_os("PATH")
        .and_then(|p| std::env::split_paths(&p).map(|d| d.join("rowt-monitor")).find(|c| c.is_file()))
        .and_then(|c| std::fs::canonicalize(c).ok());
    Some((me, mine, on_path))
}

/// The version in a Cellar path (`…/Cellar/rowt/3.6.1/libexec/bin/…` → `3.6.1`).
pub fn cellar_version(p: &Path) -> Option<String> {
    p.to_string_lossy().split("/Cellar/rowt/").nth(1).and_then(|s| s.split('/').next()).map(|s| s.to_string())
}

/// The top-rule label for a monitor that has outlived its install; None when
/// this process is the installed one.
fn detect_stale_monitor() -> Option<String> {
    let (me, mine, on_path) = monitor_paths()?;
    stale_label(&me, mine.as_deref(), on_path.as_deref())
}

fn stale_label(me: &Path, mine: Option<&Path>, on_path: Option<&Path>) -> Option<String> {
    match (mine, on_path) {
        (None, _) => Some(format!("monitor {} \u{00b7} its install is gone \u{00b7} restart",
                                  cellar_version(me).unwrap_or_else(|| "binary".into()))),
        (Some(m), Some(p)) if m != p => Some(match (cellar_version(m), cellar_version(p)) {
            (Some(a), Some(b)) => format!("monitor {a} \u{2260} installed {b} \u{00b7} restart"),
            _ => "monitor is not the installed one \u{00b7} restart".into(),
        }),
        _ => None,
    }
}

/// The binary a stale monitor should become: the rowt-monitor on PATH, when it
/// is a different file from this process's (or this process's file is gone).
/// None when there is nothing to become — the same file, or no rowt-monitor on
/// PATH — which is what keeps an exec from looping: once the new process IS the
/// one on PATH, this answers None for it.
pub fn stale_reexec_target() -> Option<PathBuf> {
    let (_, mine, on_path) = monitor_paths()?;
    reexec_target(mine.as_deref(), on_path.as_deref())
}

fn reexec_target(mine: Option<&Path>, on_path: Option<&Path>) -> Option<PathBuf> {
    let p = on_path?;
    match mine {
        // Our file is gone: become whatever is installed.
        None => Some(p.to_path_buf()),
        // A different INSTALLED build beside ours. A build run from the repo's
        // target/ against a brew install is being tested, not outlived: it keeps
        // the label and is never replaced by the install under test.
        Some(m) if m != p && cellar_version(m).is_some() && cellar_version(p).is_some() => Some(p.to_path_buf()),
        _ => None,
    }
}

/// Metrics-sidecar status, by heartbeat freshness rather than a pidfile: the
/// collector stamps `last_write` on every 5s bucket flush while its websocket is
/// connected (even with no traffic), so a fresh stamp means it's healthily
/// collecting. Two bucket periods of slack. When the router is down the collector
/// is expected to be idle, so a stale/absent store shows "—", not "off".
fn read_collector_status(router_up: bool) -> String {
    let now = SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    match crate::metrics::last_write(&crate::metrics::db_path()) {
        Some(w) if now - w <= 12 => "on".into(),
        _ if !router_up => "—".into(),
        Some(_) => "off".into(), // router up but the sidecar stopped writing
        None => "—".into(),      // never established
    }
}

/// Consecutive ticks (2 s each) on which neither /connections nor /version
/// answered, with a sing-box process alive, before the router reads as down.
const API_DOWN_AFTER: u32 = 3;

/// The router verdict from one tick's evidence: (up, note). `api_ok` —
/// /connections answered; `version_ok` — the cheap /version probe answered
/// (consulted only when /connections did not); `process_alive` — a sing-box
/// process exists; `streak` — failed ticks so far, carried across ticks and
/// updated here. Up and healthy is ("", up); up on the probe or within the
/// grace is "api slow"; down is "wedged" for a live process that stopped
/// answering, else "" for the caller to name (config / stopped). A process
/// that is gone reads as down at once: there is nothing to wait for.
fn router_verdict(api_ok: bool, version_ok: bool, process_alive: bool, streak: &mut u32) -> (bool, &'static str) {
    if api_ok {
        *streak = 0;
        return (true, "");
    }
    if !process_alive {
        *streak = 0;
        return (false, "");
    }
    if version_ok {
        *streak = 0;
        return (true, "api slow");
    }
    *streak += 1;
    if *streak < API_DOWN_AFTER { (true, "api slow") } else { (false, "wedged") }
}

#[cfg(test)]
mod router_verdict_tests {
    use super::*;

    #[test]
    fn one_slow_connections_call_is_not_down() {
        let mut streak = 0;
        assert_eq!(router_verdict(true, false, true, &mut streak), (true, ""));
        // the heavy call timed out, the cheap one answered: slow, still up
        assert_eq!(router_verdict(false, true, true, &mut streak), (true, "api slow"));
        assert_eq!(streak, 0, "an answering API resets the streak");
        assert_eq!(router_verdict(true, false, true, &mut streak), (true, ""));
    }

    #[test]
    fn a_live_process_that_stops_answering_is_down_after_three_ticks() {
        let mut streak = 0;
        assert_eq!(router_verdict(false, false, true, &mut streak), (true, "api slow"));
        assert_eq!(router_verdict(false, false, true, &mut streak), (true, "api slow"));
        assert_eq!(router_verdict(false, false, true, &mut streak), (false, "wedged"));
        assert_eq!(router_verdict(false, false, true, &mut streak), (false, "wedged"));
        // one answer and it is up again, streak gone
        assert_eq!(router_verdict(true, false, true, &mut streak), (true, ""));
        assert_eq!(streak, 0);
    }

    #[test]
    fn no_process_is_down_at_once_whatever_the_streak() {
        let mut streak = 1;
        assert_eq!(router_verdict(false, false, false, &mut streak), (false, ""));
        assert_eq!(streak, 0);
        // …and /version is never trusted over a missing process
        assert_eq!(router_verdict(false, true, false, &mut streak), (false, ""));
    }
}

/// Best-effort sing-box process stats: (uptime secs, cpu percent). None when no
/// sing-box process is running — which also tells the router-down reason apart
/// (no process = stopped, vs. alive-but-API-down = wedged). One pgrep + one ps.
fn sing_box_stat() -> Option<(u64, f32)> {
    let out = std::process::Command::new("pgrep").arg("-f").arg("sing-box").output().ok()?;
    let pid = String::from_utf8_lossy(&out.stdout).lines().next()?.trim().to_string();
    if pid.is_empty() {
        return None;
    }
    let out = std::process::Command::new("ps").args(["-o", "etime=,%cpu=", "-p", &pid]).output().ok()?;
    let line = String::from_utf8_lossy(&out.stdout);
    let mut parts = line.split_whitespace(); // "16:57 3.2" — etime has no spaces
    let secs = parse_etime(parts.next()?)?;
    let cpu = parts.next().and_then(|c| c.parse::<f32>().ok()).unwrap_or(0.0);
    Some((secs, cpu))
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

    /// rowt #s56: a batched lane edit skips the restart only when the router
    /// is alive AND was started from the host.json just rendered; a stale
    /// mark, a dead pid, a different render or the knob off all restart.
    #[test]
    fn a_router_running_the_rendered_config_takes_lane_edits_live() {
        let dir = std::env::temp_dir().join(format!("rowt-mon-hot-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let mark = |dir: &Path| {
            let host = std::fs::File::open(dir.join("host.json")).unwrap();
            let o = std::process::Command::new("cksum").stdin(std::process::Stdio::from(host)).output().unwrap();
            std::fs::write(dir.join("host.running"), o.stdout).unwrap();
        };
        std::fs::write(dir.join("host.json"), "{\"outbounds\":[]}\n").unwrap();
        std::fs::write(dir.join("host.pid"), format!("{}\n", std::process::id())).unwrap();
        assert!(!router_runs_rendered(&dir), "no host.running yet: the router predates live lane updates");
        mark(&dir);
        assert!(router_runs_rendered(&dir));
        std::fs::write(dir.join("host.json"), "{\"outbounds\":[{}]}\n").unwrap();
        assert!(!router_runs_rendered(&dir), "a render the router does not run (a geosite: came or went)");
        mark(&dir);
        let mut gone = std::process::Command::new("true").spawn().unwrap();
        let gone_pid = gone.id();
        gone.wait().unwrap();
        std::fs::write(dir.join("host.pid"), format!("{gone_pid}\n")).unwrap();
        assert!(!router_runs_rendered(&dir), "a stale mark from a stopped router");
        std::fs::write(dir.join("host.pid"), format!("{}\n", std::process::id())).unwrap();
        std::env::set_var("ROWT_LANES_HOT", "0");
        let off = router_runs_rendered(&dir);
        std::env::remove_var("ROWT_LANES_HOT");
        assert!(!off, "ROWT_LANES_HOT=0 always restarts");
        assert!(router_runs_rendered(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn probe_requests_while_the_worker_is_busy_are_not_queued() {
        let (tx, rx) = mpsc::sync_channel(0);
        for _ in 0..5 {
            assert_eq!(request_probe(Some(&tx)), Err("previous probe still running…"));
        }
        assert_eq!(rx.try_recv(), Err(mpsc::TryRecvError::Empty));
    }

    #[test]
    fn unavailable_prober_does_not_claim_to_start_a_round() {
        assert_eq!(request_probe(None), Err("probe unavailable"));
        let (tx, rx) = mpsc::sync_channel(0);
        drop(rx);
        assert_eq!(request_probe(Some(&tx)), Err("probe unavailable"));
    }

    #[test]
    fn idle_prober_accepts_one_request_then_ignores_repeated_requests() {
        let (tx, rx) = mpsc::sync_channel(0);
        let (finish_tx, finish_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            rx.recv_timeout(Duration::from_secs(2)).unwrap();
            finish_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(rx.try_recv(), Err(mpsc::TryRecvError::Empty));
        });
        let deadline = Instant::now() + Duration::from_secs(1);
        while request_probe(Some(&tx)).is_err() {
            assert!(Instant::now() < deadline, "idle worker did not accept a request");
            std::thread::yield_now();
        }
        for _ in 0..5 {
            assert_eq!(request_probe(Some(&tx)), Err("previous probe still running…"));
        }
        finish_tx.send(()).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn a_round_is_folded_into_the_shared_latency_table() {
        let dir = std::env::temp_dir().join(format!("rowt-mon-lat-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("latency.tsv"),
            "# header\nkeep\t50\t3\t2026-01-01 00:00:00\nB\t999\t3\t2026-01-01 00:00:00\njunk line\n").unwrap();
        write_latency_table(&dir, &[("B".into(), Some(80), 3), ("a".into(), None, 0)]);
        let body = std::fs::read_to_string(dir.join("latency.tsv")).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines[0], "# rowt latency table: tag, median ms (empty = no answer), samples answered, UTC time");
        // Byte order (B < a < keep), the probed rows replaced, the rest kept, junk dropped.
        assert!(lines[1].starts_with("B\t80\t3\t") && lines[2].starts_with("a\t\t0\t"), "{body}");
        assert_eq!(lines[3], "keep\t50\t3\t2026-01-01 00:00:00");
        assert_eq!(lines.len(), 4);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(dir.join("latency.tsv")).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn probe_round_samples_three_times_and_uses_the_successful_median() {
        use Sample::{Failed, Ms};
        for (samples, expected) in [
            ([Ms(900), Ms(100), Ms(200)], Some(200)),
            ([Failed, Ms(100), Ms(200)], Some(150)),
            ([Failed, Ms(100), Failed], Some(100)),
            ([Failed, Failed, Failed], None),
        ] {
            let delays = Arc::new(Mutex::new(HashMap::new()));
            let calls = AtomicUsize::new(0);
            let before = Instant::now();
            let finished = probe_round(&["server".into()], &delays, |_, budget| {
                assert_eq!(budget, PROBE_BUDGET_MS, "a failure is a verdict: no slow sample");
                samples[calls.fetch_add(1, Ordering::SeqCst)]
            });
            assert_eq!(calls.load(Ordering::SeqCst), 3);
            let result = delays.lock().unwrap()["server"];
            assert_eq!(result.0, expected);
            assert!(result.1 >= before);
            assert!(finished.is_some_and(|at| at >= result.1), "age starts after the round completes");
        }
    }

    #[test]
    fn three_fast_timeouts_earn_one_slow_sample_whose_figure_stands_alone() {
        use Sample::{Failed, Ms, Timeout};
        // (the three fast samples, the slow one) -> (the figure, samples taken)
        for (fast, slow, expected, taken) in [
            ([Timeout, Timeout, Timeout], Ms(7172), Some(7172), 4), // a slow link: seconds, not down
            ([Timeout, Timeout, Timeout], Timeout, None, 4),        // dead at the slow budget too: down
            ([Timeout, Timeout, Timeout], Failed, None, 4),
            ([Timeout, Failed, Timeout], Ms(7172), None, 3),        // a failure is a verdict: no slow sample
            ([Failed, Failed, Failed], Ms(7172), None, 3),
            ([Timeout, Ms(100), Timeout], Ms(7172), Some(100), 3),  // an answer is the figure
        ] {
            let delays = Arc::new(Mutex::new(HashMap::new()));
            let calls = AtomicUsize::new(0);
            let (_, rows) = probe_round_rows(&["server".into()], &delays, |_, budget| {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                if n < 3 {
                    assert_eq!(budget, PROBE_BUDGET_MS);
                    fast[n]
                } else {
                    assert_eq!(budget, SLOW_PROBE_BUDGET_MS, "the fourth sample is the slow one");
                    slow
                }
            });
            assert_eq!(calls.load(Ordering::SeqCst), taken, "{fast:?} then {slow:?}");
            assert_eq!(delays.lock().unwrap()["server"].0, expected, "{fast:?} then {slow:?}");
            // The latency table row counts the samples that answered: the slow
            // figure is one sample, like a single fast answer.
            assert_eq!(rows, vec![("server".to_string(), expected, usize::from(expected.is_some()))], "{fast:?} then {slow:?}");
        }
    }

    #[test]
    fn probe_round_limits_concurrent_nodes_to_ten_and_covers_the_pool() {
        let delays = Arc::new(Mutex::new(HashMap::new()));
        let members: Vec<_> = (0..20).map(|i| format!("server-{i}")).collect();
        let calls = Mutex::new(HashMap::<String, usize>::new());
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let (started_tx, started_rx) = mpsc::channel();
        let gate = (Mutex::new(false), std::sync::Condvar::new());
        std::thread::scope(|scope| {
            scope.spawn(|| probe_round(&members, &delays, |tag, _| {
                let n = {
                    let mut calls = calls.lock().unwrap();
                    let count = calls.entry(tag.to_string()).or_default();
                    *count += 1;
                    *count
                };
                let running = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(running, Ordering::SeqCst);
                if n == 1 {
                    started_tx.send(()).unwrap();
                    drop(gate.1.wait_while(gate.0.lock().unwrap(), |open| !*open).unwrap());
                }
                std::thread::sleep(Duration::from_millis(2));
                active.fetch_sub(1, Ordering::SeqCst);
                Sample::Ms(100)
            }));
            for _ in 0..10 {
                if started_rx.recv_timeout(Duration::from_secs(1)).is_err() { break; }
            }
            *gate.0.lock().unwrap() = true;
            gate.1.notify_all();
        });
        assert_eq!(peak.load(Ordering::SeqCst), 10);
        assert_eq!(delays.lock().unwrap().len(), members.len());
        assert!(calls.lock().unwrap().values().all(|&n| n == 3));
    }

    #[test]
    fn completed_nodes_are_published_and_replaced_without_waiting_for_slow_nodes() {
        let delays = Arc::new(Mutex::new(HashMap::new()));
        let members: Vec<_> = (0..11).map(|i| format!("server-{i}")).collect();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let (started_tx, started_rx) = mpsc::channel();
        let slow_calls = AtomicUsize::new(0);
        let next_started = std::thread::scope(|scope| {
            let worker = scope.spawn(|| probe_round(&members, &delays, |tag, _| {
                if tag == "server-0" && slow_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    release_rx.lock().unwrap().recv_timeout(Duration::from_secs(5)).unwrap();
                }
                if tag == "server-10" { started_tx.send(()).unwrap(); }
                Sample::Ms(100)
            }));
            let next_started = started_rx.recv_timeout(Duration::from_secs(1)).is_ok();
            let early_results = delays.lock().unwrap().clone();
            release_tx.send(()).unwrap();
            worker.join().unwrap();
            assert!(!early_results.contains_key("server-0"), "slow node is still running");
            assert!(!early_results.is_empty(), "finished nodes are visible before the round ends");
            next_started
        });
        assert!(next_started, "node 11 must start while an earlier node is still running");
        assert_eq!(delays.lock().unwrap().len(), 11);
    }

    #[test]
    fn empty_probe_round_has_no_completion_time() {
        let delays = Arc::new(Mutex::new(HashMap::new()));
        assert!(probe_round(&[], &delays, |_, _| panic!("empty pool")).is_none());
    }

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

    #[test]
    fn traffic_overrides_a_failed_probe() {
        // The reported bug: probe fails but the escape lane is moving bytes → the
        // header must read healthy, not ERROR.
        assert_eq!(health_with_traffic(Some(false), true), Some(true));
        // Failed probe with no traffic → genuinely unreachable, stays failed.
        assert_eq!(health_with_traffic(Some(false), false), Some(false));
        // A healthy or pending probe is never downgraded/altered by traffic.
        assert_eq!(health_with_traffic(Some(true), false), Some(true));
        assert_eq!(health_with_traffic(Some(true), true), Some(true));
        assert_eq!(health_with_traffic(None, true), None);
        assert_eq!(health_with_traffic(None, false), None);
    }

    #[test]
    fn force_probe_is_traffic_aware() {
        use std::time::Duration;
        let s = Some; // brevity
        let cap = Duration::from_secs(600);
        // A just-recovered router always re-probes, regardless of the rest.
        assert!(should_force_probe(false, false, true, s(Duration::from_secs(1)), 0, cap));
        // Never forced when the probe is healthy/pending.
        assert!(!should_force_probe(false, true, false, None, 0, cap));
        // Failed probe + live traffic → retry on the short 15s window.
        assert!(should_force_probe(true, true, false, s(Duration::from_secs(16)), 0, cap));
        assert!(!should_force_probe(true, true, false, s(Duration::from_secs(10)), 0, cap), "throttled within 15s");
        // Failed probe, no traffic → slow 60s retry (15s is not yet due).
        assert!(!should_force_probe(true, false, false, s(Duration::from_secs(20)), 0, cap));
        assert!(should_force_probe(true, false, false, s(Duration::from_secs(61)), 0, cap));
        // Never forced yet (None) → due immediately when the probe is failing.
        assert!(should_force_probe(true, true, false, None, 0, cap));
    }

    #[test]
    fn repeated_failures_back_off_to_the_probe_interval() {
        use std::time::Duration;
        let s = |secs: u64| Some(Duration::from_secs(secs));
        let cap = Duration::from_secs(600);
        // Each retry doubles the wait: 15 s, 30 s, 60 s, 120 s …
        assert!(!should_force_probe(true, true, false, s(29), 1, cap));
        assert!(should_force_probe(true, true, false, s(31), 1, cap));
        assert!(!should_force_probe(true, true, false, s(119), 3, cap));
        assert!(should_force_probe(true, true, false, s(121), 3, cap));
        // … and never past the probe interval, which runs a round anyway.
        assert!(!should_force_probe(true, true, false, s(599), 6, cap));
        assert!(should_force_probe(true, true, false, s(601), 6, cap));
        assert!(should_force_probe(true, false, false, s(601), 40, cap), "a long streak does not overflow");
        // A recovered router still re-probes at once, whatever the streak.
        assert!(should_force_probe(true, true, true, s(1), 6, cap));
    }
}

#[cfg(test)]
mod watch_cell_tests {
    use super::watch_cell;

    /// The whole reason the cell exists: `launchctl list` cannot tell a
    /// registered job from a working one. Every row here is a state the old
    /// "on"/"off" cell rendered identically to its neighbour.
    #[test]
    fn loaded_is_not_the_same_as_ticking() {
        // Not loaded: installed or not, and no age either way.
        assert_eq!(watch_cell(false, true, Some(5), 120), ("off", None));
        assert_eq!(watch_cell(false, false, None, 120), ("\u{2014}", None));
        // Loaded and ticking: on, with the age shown.
        assert_eq!(watch_cell(true, true, Some(50), 120), ("on", Some(50)));
        assert_eq!(watch_cell(true, true, Some(240), 120), ("on", Some(240)), "exactly 2x is still on");
        // Loaded but no tick has completed in over 2x the interval: stalled.
        assert_eq!(watch_cell(true, true, Some(241), 120), ("stalled", Some(241)));
        assert_eq!(watch_cell(true, true, Some(14 * 60), 120), ("stalled", Some(840)));
        // The threshold follows the configured interval, not a constant.
        assert_eq!(watch_cell(true, true, Some(500), 300), ("on", Some(500)));
        // No heartbeat known (an older rowt never writes one) reads as on, not
        // stalled — or every machine would read stalled the day it upgrades.
        assert_eq!(watch_cell(true, true, None, 120), ("on", None));
    }
}

#[cfg(test)]
mod auto_selection_tests {
    use super::*;

    #[test]
    fn best_counts_as_auto_and_its_pick_is_the_active_chip() {
        let h = health_view(&pool(), BEST_MODE.into(), Some("SG-1".into()), verdicts(&[("JP-1", Some(40)), ("SG-1", Some(80))]));
        assert!(h.chips.iter().any(|c| c.name == "SG-1" && c.active), "the pick is marked");
        assert_eq!(h.auto_now.as_deref(), Some("SG-1"));
    }

    fn pool() -> Vec<String> {
        ["JP-1", "SG-1", "US-1"].iter().map(|s| s.to_string()).collect()
    }

    /// `fresh` over a fixed table: `Some(ms)` up, `None` down; absent = pending.
    fn verdicts(v: &[(&str, Option<u32>)]) -> impl Fn(&str) -> Option<Option<u32>> {
        let m: HashMap<String, Option<u32>> = v.iter().map(|(t, ms)| (t.to_string(), *ms)).collect();
        move |tag: &str| m.get(tag).copied()
    }

    #[test]
    fn all_servers_are_visible_before_any_probe_completes() {
        for (selected, pick) in [("SG-1", None), (AUTO_GROUP, None), (AUTO_GROUP, Some("US-1".into()))] {
            let h = health_view(&pool(), selected.into(), pick.clone(), |_| None);
            assert_eq!((h.total, h.up, h.down), (3, 0, 0));
            assert_eq!(h.chips.len(), 3);
            assert!(h.chips.iter().all(|c| c.ms.is_none() && !c.down));
            let active = if selected == AUTO_GROUP { pick.as_deref() } else { Some(selected) };
            assert_eq!(h.chips.iter().find(|c| c.active).map(|c| c.name.as_str()), active);
        }
    }

    #[test]
    fn partial_probe_results_keep_pending_servers_in_the_list() {
        let h = health_view(&pool(), "SG-1".into(), None,
            verdicts(&[("JP-1", Some(40)), ("US-1", None)]));
        assert_eq!((h.total, h.up, h.down), (3, 1, 1));
        assert_eq!(h.chips.len(), 3);
        let pending = h.chips.iter().find(|c| c.name == "SG-1").unwrap();
        assert!(pending.active && !pending.down && pending.ms.is_none());
    }

    #[test]
    fn manual_mode_marks_the_pinned_server() {
        let h = health_view(&pool(), "SG-1".into(), None, verdicts(&[("JP-1", Some(40)), ("SG-1", Some(80)), ("US-1", None)]));
        let chips: Vec<_> = h.chips.iter().map(|c| (c.name.as_str(), c.active)).collect();
        assert_eq!(chips, vec![("SG-1", true), ("JP-1", false), ("US-1", false)], "down servers remain visible");
        assert_eq!((h.active_ms, h.active_ok), (Some(80), Some(true)));
        assert_eq!((h.total, h.up, h.down), (3, 2, 1));
        assert!(h.chips.iter().find(|c| c.name == "US-1").unwrap().down);
    }

    #[test]
    fn auto_mode_follows_urltests_pick_not_the_fastest() {
        // JP-1 is faster, but urltest is carrying traffic on SG-1: follow urltest.
        let h = health_view(&pool(), AUTO_GROUP.into(), Some("SG-1".into()), verdicts(&[("JP-1", Some(40)), ("SG-1", Some(80))]));
        assert_eq!((h.chips[0].name.as_str(), h.chips[0].active), ("SG-1", true));
        assert!(!h.chips[1].active);
        assert_eq!((h.active_ms, h.active_ok), (Some(80), Some(true)));
    }

    #[test]
    fn autos_pick_holds_the_strip_before_its_first_probe() {
        let h = health_view(&pool(), AUTO_GROUP.into(), Some("US-1".into()), verdicts(&[("JP-1", Some(40))]));
        assert_eq!((h.chips[0].name.as_str(), h.chips[0].ms, h.chips[0].active), ("US-1", None, true), "pinned, drawn with —");
        assert_eq!((h.active_ms, h.active_ok), (None, None), "pending: no alarm");
        assert!(!h.chips[0].down);
    }

    #[test]
    fn autos_pick_failing_its_probe_stays_pinned_and_flags_error() {
        let h = health_view(&pool(), AUTO_GROUP.into(), Some("US-1".into()), verdicts(&[("JP-1", Some(40)), ("US-1", None)]));
        assert_eq!((h.chips[0].name.as_str(), h.chips[0].ms, h.chips[0].active), ("US-1", None, true));
        assert_eq!(h.active_ok, Some(false));
        assert!(h.chips[0].down);
    }

    #[test]
    fn all_down_servers_remain_visible_including_the_manual_pin() {
        let h = health_view(&pool(), "SG-1".into(), None,
            verdicts(&[("JP-1", None), ("SG-1", None), ("US-1", None)]));
        assert_eq!((h.total, h.up, h.down), (3, 0, 3));
        assert_eq!(h.chips.len(), 3);
        assert!(h.chips.iter().all(|c| c.down && c.ms.is_none()));
        assert!(h.chips.iter().any(|c| c.name == "SG-1" && c.active));
    }

    #[test]
    fn unresolved_auto_falls_back_to_the_pool() {
        let h = health_view(&pool(), AUTO_GROUP.into(), None, verdicts(&[("JP-1", Some(40)), ("SG-1", Some(80))]));
        assert!(h.chips.iter().all(|c| !c.active), "no pick, nothing pinned");
        assert_eq!((h.active_ms, h.active_ok), (Some(40), Some(true)), "fastest up stands in; healthy if any is up");
    }

    #[test]
    fn in_flight_count_drops_however_the_command_thread_ends() {
        let count = Arc::new(AtomicUsize::new(0));
        let g = InFlight::new(&count);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        drop(g);
        assert_eq!(count.load(Ordering::SeqCst), 0);
        // A command thread that panics still releases its count on unwind — a
        // leaked count would refuse every server change until a restart.
        let g = InFlight::new(&count);
        let t = std::thread::spawn(move || {
            let _g = g;
            panic!("command thread died");
        });
        assert!(t.join().is_err());
        assert_eq!(count.load(Ordering::SeqCst), 0, "no permanent \"restart in progress\"");
    }

    #[test]
    fn the_auto_group_is_not_a_pool_member_but_stays_an_escape_tag() {
        let host: Value = serde_json::json!({"outbounds": [
            {"type": "urltest", "tag": "auto", "outbounds": ["JP-1", "SG-1"]},
            {"type": "selector", "tag": "escape", "outbounds": ["auto", "JP-1", "SG-1"]}
        ]});
        let tags = escape_tags_of(&host);
        assert!(tags.contains(AUTO_GROUP) && tags.contains("escape"), "lane classification still sees both");
        let mut members: Vec<String> = tags.into_iter().filter(|t| is_pool_member(t)).collect();
        members.sort();
        assert_eq!(members, vec!["JP-1", "SG-1"], "the pool — and its count — is servers only");
    }
}

#[cfg(test)]
mod stale_monitor_tests {
    use super::*;

    fn p(s: &str) -> PathBuf { PathBuf::from(s) }

    #[test]
    fn reexec_only_into_a_different_installed_binary() {
        let old = p("/opt/homebrew/Cellar/rowt/3.6.1/libexec/bin/rowt-monitor");
        let new = p("/opt/homebrew/Cellar/rowt/3.6.6/libexec/bin/rowt-monitor");
        // the install was removed under us: become the one on PATH
        assert_eq!(reexec_target(None, Some(&new)), Some(new.clone()));
        // a newer install beside ours: become it
        assert_eq!(reexec_target(Some(&old), Some(&new)), Some(new.clone()));
        // we ARE the installed one (the state right after an exec): nothing to do
        assert_eq!(reexec_target(Some(&new), Some(&new)), None);
        // nothing on PATH to become, whatever happened to our file
        assert_eq!(reexec_target(None, None), None);
        assert_eq!(reexec_target(Some(&old), None), None);
        // a repo build run against the live config, with a brew install on PATH:
        // it is being tested, so it keeps the label and is never replaced
        let dev = p("/Users/x/code/rowt/rowt-monitor/target/release/rowt-monitor");
        assert_eq!(reexec_target(Some(&dev), Some(&new)), None);
        assert_eq!(stale_label(&dev, Some(&dev), Some(&new)).as_deref(), Some("monitor is not the installed one \u{00b7} restart"));
        // …unless its file is gone (a cargo clean under it): then the install is all there is
        assert_eq!(reexec_target(None, Some(&new)), Some(new.clone()));
    }

    #[test]
    fn label_matches_the_target_decision() {
        let old = p("/opt/homebrew/Cellar/rowt/3.6.1/libexec/bin/rowt-monitor");
        let new = p("/opt/homebrew/Cellar/rowt/3.6.6/libexec/bin/rowt-monitor");
        assert_eq!(stale_label(&old, None, Some(&new)).as_deref(), Some("monitor 3.6.1 \u{00b7} its install is gone \u{00b7} restart"));
        assert_eq!(stale_label(&old, Some(&old), Some(&new)).as_deref(), Some("monitor 3.6.1 \u{2260} installed 3.6.6 \u{00b7} restart"));
        assert_eq!(stale_label(&new, Some(&new), Some(&new)), None);
        // gone, and nothing on PATH: the label still says so (a dev build run from target/)
        assert_eq!(stale_label(&p("/tmp/rowt-monitor"), None, None).as_deref(), Some("monitor binary \u{00b7} its install is gone \u{00b7} restart"));
        assert_eq!(cellar_version(&new).as_deref(), Some("3.6.6"));
        assert_eq!(cellar_version(&p("/tmp/x")), None);
    }
}
