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
    /// The key `y` yanks in the connections pane.
    pub fn key(&self) -> String {
        format!("{}:{}", self.host, self.port)
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
    pub server_name: String, // e.g. "JP-Tokyo"
    pub server_ms: u32,      // active server RTT
    pub router: String,      // e.g. "running · :7890"
    pub router_up: bool,     // proxy/router reachable — drives the LIVE/DOWN dot
    pub proxy: String,       // e.g. "on · Wi-Fi"
    pub config: String,      // e.g. "host.json OK"
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
