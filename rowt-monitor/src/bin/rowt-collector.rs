//! rowt-collector — the always-on traffic-metrics sidecar (see METRICS.md).
//!
//! Holds the clash `/connections` websocket, diffs each connection's cumulative
//! byte counters into 5-second per-`(domain, lane)` buckets, reconciles the
//! remainder against the monotonic grand totals, and writes the tiered SQLite
//! store. `bin/rowt` runs it as a sidecar tied to the router's lifecycle; it is
//! never launched from inside the TUI.
//!
//! Env: `ROWT_COLLECT_EP` (controller host:port, default 127.0.0.1:9090),
//! `ROWT_COLLECT_SECRET` (clash bearer), `ROWT_CFG` (config dir for the DB).

use std::collections::HashMap;
use std::net::TcpStream;
use std::thread::sleep;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use rowt_monitor::metrics::{self, Bucket};
use rowt_monitor::model::Lane;
use rowt_monitor::source::parse::{classify_lane, parse_connections};

/// Idle when the ws is dead (router down): reconnect backoff, capped.
const BACKOFF_START: u64 = 1;
const BACKOFF_MAX: u64 = 30;
/// A wedged router can hold the socket open but stop pushing — bail if no frame
/// arrives within this long and let the backoff loop re-establish.
const READ_TIMEOUT: Duration = Duration::from_secs(15);
/// Run the tier rollup at most this often.
const ROLLUP_EVERY: i64 = 60;

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn u64_field(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(|x| x.as_u64().or_else(|| x.as_f64().map(|f| f as u64))).unwrap_or(0)
}

fn main() {
    let ep = std::env::var("ROWT_COLLECT_EP").unwrap_or_else(|_| "127.0.0.1:9090".into());
    let secret = std::env::var("ROWT_COLLECT_SECRET").ok().filter(|s| !s.is_empty());

    let db = match metrics::open_db(&metrics::db_path()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("rowt-collector: cannot open metrics DB: {e}");
            std::process::exit(1);
        }
    };
    metrics::set_meta(&db, "pid", &std::process::id().to_string());
    metrics::set_meta(&db, "started", &now_secs().to_string());
    eprintln!("rowt-collector: started (pid {}), controller {ep}", std::process::id());

    let mut st = State::new();
    let mut backoff = BACKOFF_START;
    loop {
        match connect(&ep, secret.as_deref()) {
            Ok(mut ws) => {
                backoff = BACKOFF_START;
                eprintln!("rowt-collector: connected to /connections");
                // stream() returns when the socket drops or wedges.
                stream(&mut ws, &db, &mut st);
                st.flush_pending(&db); // don't lose the in-flight bucket
                st.reset(); // sing-box may have restarted → cumulative counters reset
                eprintln!("rowt-collector: disconnected");
            }
            Err(_) => {
                // Router almost certainly down; this is the normal idle path.
                sleep(Duration::from_secs(backoff));
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
    }
}

type Ws = WebSocket<MaybeTlsStream<TcpStream>>;

fn connect(ep: &str, secret: Option<&str>) -> Result<Ws, Box<dyn std::error::Error>> {
    let mut req = format!("ws://{ep}/connections").into_client_request()?;
    if let Some(s) = secret {
        req.headers_mut().insert("Authorization", format!("Bearer {s}").parse()?);
    }
    let (ws, _resp) = tungstenite::connect(req)?;
    if let MaybeTlsStream::Plain(s) = ws.get_ref() {
        let _ = s.set_read_timeout(Some(READ_TIMEOUT));
    }
    Ok(ws)
}

/// Read frames until the socket errors (drop, wedge/timeout, or close).
fn stream(ws: &mut Ws, db: &rusqlite::Connection, st: &mut State) {
    loop {
        match ws.read() {
            Ok(Message::Text(txt)) => {
                if let Ok(v) = serde_json::from_str::<Value>(&txt) {
                    st.on_frame(&v, db);
                }
            }
            Ok(Message::Binary(bin)) => {
                if let Ok(v) = serde_json::from_slice::<Value>(&bin) {
                    st.on_frame(&v, db);
                }
            }
            Ok(Message::Close(_)) => return,
            Ok(_) => {} // ping/pong/frame — tungstenite handles control replies
            Err(_) => return, // drop or read timeout → let the outer loop reconnect
        }
    }
}

/// Per-connection cursor + bucketing state, held across frames within one ws
/// session (reset on disconnect, since sing-box restarts zero the counters).
struct State {
    /// connection id -> last-seen cumulative (up, dn)
    last: HashMap<String, (u64, u64)>,
    /// grand (uploadTotal, downloadTotal) at the last frame
    last_total: Option<(u64, u64)>,
    /// have we seen a baseline frame this session? (first frame primes, no counting)
    primed: bool,
    bucket: Bucket,
    bucket_ts: i64,
    last_rollup: i64,
}

impl State {
    fn new() -> Self {
        let now = now_secs();
        State {
            last: HashMap::new(),
            last_total: None,
            primed: false,
            bucket: Bucket::new(),
            bucket_ts: now - now.rem_euclid(5),
            last_rollup: now,
        }
    }

    /// Clear per-session cursors (called on disconnect). Keeps the bucket cadence.
    fn reset(&mut self) {
        self.last.clear();
        self.last_total = None;
        self.primed = false;
    }

    fn flush_pending(&mut self, db: &rusqlite::Connection) {
        if !self.bucket.is_empty() {
            let _ = metrics::flush_bucket(db, self.bucket_ts, &self.bucket);
            self.bucket.clear();
        }
    }

    fn on_frame(&mut self, v: &Value, db: &rusqlite::Connection) {
        let now = now_secs();
        let ts5 = now - now.rem_euclid(5);
        // Roll the 5s bucket over on a boundary crossing.
        if ts5 != self.bucket_ts {
            let _ = metrics::flush_bucket(db, self.bucket_ts, &self.bucket);
            self.bucket.clear();
            self.bucket_ts = ts5;
        }

        let raw = parse_connections(v);
        let up_total = u64_field(v, "uploadTotal");
        let dn_total = u64_field(v, "downloadTotal");

        // First frame after (re)connect: establish the baseline only — counting
        // deltas now would dump each connection's pre-existing cumulative into one
        // bucket as a spike.
        if !self.primed {
            for c in &raw {
                self.last.insert(c.id.clone(), (c.up, c.down));
            }
            self.last_total = Some((up_total, dn_total));
            self.primed = true;
            return;
        }

        let no_tags = std::collections::HashSet::new(); // chains carry the literal "escape" tag
        let (mut sum_up, mut sum_dn) = (0u64, 0u64);
        let mut seen: HashMap<String, (u64, u64)> = HashMap::with_capacity(raw.len());
        for c in &raw {
            let lane = classify_lane(&c.chains, &no_tags);
            if lane == Lane::Block {
                continue; // sinkholed — excluded, matching the connections view
            }
            let (lu, ld) = self.last.get(&c.id).copied().unwrap_or((0, 0));
            let d_up = c.up.saturating_sub(lu);
            let d_dn = c.down.saturating_sub(ld);
            seen.insert(c.id.clone(), (c.up, c.down));
            if d_up == 0 && d_dn == 0 {
                continue;
            }
            let domain = if c.host.is_empty() { "?" } else { c.host.as_str() };
            metrics::bucket_add(&mut self.bucket, domain, lane.label(), d_up, d_dn);
            sum_up += d_up;
            sum_dn += d_dn;
        }
        self.last = seen; // drop cursors for closed connections

        // Reconcile the remainder (sub-frame-lived connections, block lane, etc.)
        // against the monotonic grand totals so the books balance.
        if let Some((pu, pd)) = self.last_total {
            let g_up = up_total.saturating_sub(pu);
            let g_dn = dn_total.saturating_sub(pd);
            let un_up = g_up.saturating_sub(sum_up);
            let un_dn = g_dn.saturating_sub(sum_dn);
            metrics::bucket_add(&mut self.bucket, "(unattributed)", "-", un_up, un_dn);
        }
        self.last_total = Some((up_total, dn_total));

        if now - self.last_rollup >= ROLLUP_EVERY {
            let _ = metrics::rollup(db, now);
            self.last_rollup = now;
        }
    }
}
