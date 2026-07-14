//! Data model for the monitor. Everything here is *derived* on the 2s tick
//! (see README "Data provenance"); the UI never mutates any of it.

use ratatui::style::Color;

use crate::theme;

/// Routing outcome for a connection / lane row.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Lane {
    Escape,
    Corp,
    Direct,
    Block,
}

impl Lane {
    pub fn label(self) -> &'static str {
        match self {
            Lane::Escape => "escape",
            Lane::Corp => "corp",
            Lane::Direct => "direct",
            Lane::Block => "block",
        }
    }
    pub fn color(self) -> Color {
        match self {
            Lane::Escape => theme::ESCAPE,
            Lane::Corp => theme::CORP,
            Lane::Direct => theme::DIRECT,
            Lane::Block => theme::BLOCK,
        }
    }
    /// The three lanes shown in the connections rate table (block is excluded).
    pub const FILTERABLE: [Lane; 3] = [Lane::Escape, Lane::Corp, Lane::Direct];
    /// Parse a lane label back to the enum (as stored in the metrics DB).
    pub fn from_label(s: &str) -> Lane {
        match s {
            "escape" => Lane::Escape,
            "corp" => Lane::Corp,
            "block" => Lane::Block,
            _ => Lane::Direct,
        }
    }
}

/// Which columns the connections pane is showing. `v` pans this window across a
/// conceptually-wide table (pinned `host:port` + [live | ↑ upload | ↓ download])
/// and wraps — see METRICS.md §5. Selection is by domain, so it rides the pan.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConnView {
    Live,
    Up,
    Down,
}

impl ConnView {
    pub fn pan_next(self) -> Self {
        match self {
            ConnView::Live => ConnView::Up,
            ConnView::Up => ConnView::Down,
            ConnView::Down => ConnView::Live,
        }
    }
    pub fn is_live(self) -> bool {
        matches!(self, ConnView::Live)
    }
    pub fn is_up(self) -> bool {
        matches!(self, ConnView::Up)
    }
    /// Caption chip suffix for the pane title (None in the Live view).
    pub fn chip(self) -> Option<&'static str> {
        match self {
            ConnView::Live => None,
            ConnView::Up => Some("▲ upload"),
            ConnView::Down => Some("▼ download"),
        }
    }
}

/// The timescale band for the flipped metrics columns (cycled by `w` when the
/// connections pane is focused in a metrics view). Each band defines four
/// trailing-window columns; `now`/`1m` render as rates, the rest as totals.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MetricsBand {
    Recent,
    Days,
    Year,
}

impl MetricsBand {
    pub const ALL: [MetricsBand; 3] = [MetricsBand::Recent, MetricsBand::Days, MetricsBand::Year];
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|b| *b == self).unwrap()
    }
    pub fn label(self) -> &'static str {
        match self {
            MetricsBand::Recent => "recent",
            MetricsBand::Days => "days",
            MetricsBand::Year => "year",
        }
    }
    /// The four columns: (header label, trailing-window seconds, is_rate).
    pub fn cols(self) -> [(&'static str, i64, bool); 4] {
        match self {
            MetricsBand::Recent => [("now", 5, true), ("1m", 60, true), ("1h", 3600, false), ("24h", 86_400, false)],
            MetricsBand::Days => [("1h", 3600, false), ("6h", 21_600, false), ("24h", 86_400, false), ("7d", 604_800, false)],
            MetricsBand::Year => [("24h", 86_400, false), ("7d", 604_800, false), ("30d", 2_592_000, false), ("1y", 31_536_000, false)],
        }
    }
    pub fn spans(self) -> [i64; 4] {
        let c = self.cols();
        [c[0].1, c[1].1, c[2].1, c[3].1]
    }
}

/// One row of the flipped metrics view: a domain, its dominant lane, and the
/// per-column byte totals for the active band (direction already selected).
#[derive(Clone, Debug)]
pub struct MetricRow {
    pub domain: String,
    pub lane: Lane,
    pub cols: [u64; 4],
}

impl MetricRow {
    /// The routed/copied key — the domain (matches `Conn::key`, so the shared
    /// selection machinery works over either list).
    pub fn key(&self) -> String {
        self.domain.clone()
    }
}

/// Failure / block classification for an errors-pane row.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ErrKind {
    Dns,     // transient  (orange)
    Timeout, // persistent (red)
    Reset,   // persistent (red)
    Refused, // persistent (red)
    Blocked, // blocked    (purple)
}

impl ErrKind {
    pub fn label(self) -> &'static str {
        match self {
            ErrKind::Dns => "dns",
            ErrKind::Timeout => "timeout",
            ErrKind::Reset => "reset",
            ErrKind::Refused => "refused",
            ErrKind::Blocked => "blocked",
        }
    }
    pub fn color(self) -> Color {
        match self {
            ErrKind::Dns => theme::TRANSIENT,
            ErrKind::Timeout | ErrKind::Reset | ErrKind::Refused => theme::PERSISTENT,
            ErrKind::Blocked => theme::BLOCKED,
        }
    }
}

/// The rolling window over which the errors pane aggregates.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Window {
    M5,
    M10,
    H1,
    H24,
}

impl Window {
    pub const ALL: [Window; 4] = [Window::M5, Window::M10, Window::H1, Window::H24];
    pub fn label(self) -> &'static str {
        match self {
            Window::M5 => "5m",
            Window::M10 => "10m",
            Window::H1 => "1h",
            Window::H24 => "24h",
        }
    }
    pub fn index(self) -> usize {
        Window::ALL.iter().position(|w| *w == self).unwrap()
    }
}

/// One row in the live connections table.
#[derive(Clone, Debug)]
pub struct Conn {
    pub lane: Lane,
    pub host: String,
    pub port: u16,
    pub conns: u32, // concurrency — small, bounded (NOT a monotonic counter)
    pub up: f64,    // bytes/s
    pub down: f64,  // bytes/s
    pub rule: String,
}

impl Conn {
    /// The text `y` yanks in the connections pane: the domain only (no `:port`),
    /// so it drops straight into a browser / `dig` / rowt rule.
    pub fn key(&self) -> String {
        self.host.clone()
    }
}

/// One row in the errors & blocked table.
#[derive(Clone, Debug)]
pub struct ErrRow {
    pub count: u32,
    pub kind: ErrKind,
    pub domain: String,
}

/// Per-lane aggregate for the connections header rate table.
#[derive(Clone, Copy, Debug)]
pub struct LaneAgg {
    pub lane: Lane,
    pub up: f64,
    pub down: f64,
    pub conns: u32,
}

/// Aggregate error/block category (header of the errors pane).
#[derive(Clone, Copy, Debug)]
pub struct ErrCat {
    pub count: u32,
    pub domains: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct AllAgg {
    pub up: f64,
    pub down: f64,
    pub conns: u32,
}

#[derive(Clone, Debug)]
pub struct Server {
    pub name: String,
    pub ms: u32,
    pub active: bool, // the currently-selected server (marked in the strip)
}

/// Session facts for the identity band.
#[derive(Clone, Debug)]
pub struct Identity {
    pub mode: String,        // e.g. "host · en0"
    pub uptime: String,      // e.g. "3h 15m"
    pub server_name: String,     // e.g. "JP-Tokyo" (or "auto")
    pub server_ms: Option<u32>,  // active server RTT; None -> shown as "—"
    pub router: String,      // base word: "running" | "down"
    pub router_cpu: Option<f32>, // sing-box CPU% when running (for "running · N%")
    pub router_reason: String,   // when down: "wedged" | "config" | "stopped" (else "")
    pub router_up: bool,     // proxy/router reachable — drives the LIVE/DOWN dot
    /// Active server probe result: Some(true) = reachable, Some(false) = failing
    /// (→ ERROR), None = not probed yet. Drives the LIVE/ERROR distinction.
    pub active_ok: Option<bool>,
    pub proxy: String,       // e.g. "on · Wi-Fi"
    pub watch: String,       // watchdog LaunchAgent: "on" | "off" | "—"
    pub collector: String,   // metrics sidecar: "on" | "off" | "—" (by last-write freshness)
    /// Columns reserved for the active server name in the header, so the ms
    /// column doesn't jump as the active server changes. Sized to the pool's
    /// longest name (bounded). 8 reproduces the golden (`JP-Tokyo`).
    pub name_reserve: u16,
}

/// A full observation for one tick. Instantaneous fields (rates, conns,
/// servers, identity) come from clash/state/system; the errors block is the
/// re-aggregation of the log window and is refilled when the window changes.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub identity: Identity,
    pub all: AllAgg,
    pub lanes: Vec<LaneAgg>,
    pub conns: Vec<Conn>,

    pub transient: ErrCat,
    pub persistent: ErrCat,
    pub blocked: ErrCat,
    pub errors: Vec<ErrRow>,

    pub servers_total: u32,
    pub servers_up: u32,
    pub servers_down: u32,
    pub active_server: String,
    pub chips: Vec<Server>, // idle-but-up pool, sorted by latency
}
