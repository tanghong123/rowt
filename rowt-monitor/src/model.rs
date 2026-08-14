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
            Lane::Escape => theme::escape(),
            Lane::Corp => theme::corp(),
            Lane::Direct => theme::direct(),
            Lane::Block => theme::block(),
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

/// Registry second levels — the labels that are part of the public suffix rather
/// than of anyone's name. Only consulted under a two-letter ccTLD, so a `.com`
/// host can never reach them.
///
/// Deliberately NOT `geosite::is_generic`: that list exists to guess a *brand*
/// and so carries `api`, `cdn`, `static`, `mail`… — under it `a.b.cdn.com` would
/// broaden to `b.cdn.com` instead of `cdn.com`.
const REGISTRY_SLD: [&str; 10] = ["co", "com", "net", "org", "edu", "gov", "ac", "mil", "or", "ne"];

/// Why this entry is too broad to write, or `None` if it is fine.
///
/// Re-exported from `rowt_core::lanes` rather than reimplemented: the CLI
/// refuses on exactly this rule, so a second copy here could drift and the bar
/// would paint an entry safe that `rowt <lane> add` then declines — or worse,
/// the other way round. The TUI applies it with no `--force` escape, because a
/// keystroke in an editable field is not the place to override a safety rule.
pub use rowt_core::lanes::entry_risk;

/// The broadened lane entry for `host` — its registrable domain (`x.y.z.com` →
/// `z.com`, `x.y.z.co.uk` → `z.co.uk`). `None` when there is nothing broader to
/// add: an IP literal, a bare label, or a host that already *is* its registrable
/// domain (`x.com`). The caller stays inert on `None` — it does not fall back to
/// the host, which is what the lowercase key already does.
///
/// Bare, not dot-led. Measured against sing-box 1.13.14 with
/// `sing-box rule-set match`, which is the router's own matcher:
///
/// ```text
/// domain_suffix ["z.com"]   z.com ✓   a.z.com ✓   xz.com ✗
/// domain_suffix [".z.com"]  z.com ✗   a.z.com ✓   xz.com ✗
/// ```
///
/// sing-box matches on a LABEL BOUNDARY either way, so the dot's only effect is
/// to *lose* the apex — which is not what "cover the whole service" means.
/// `classify.rs` uses the same boundary rule, so `rowt explain` agrees.
///
/// Sibling subdomains parked in another lane are safe: `_lane_dedupe` is
/// exact-line, so adding `alicdn.com` here leaves `dev.g.alicdn.com` in corp,
/// and the render's longest-match ordering keeps the longer entry winning.
pub fn parent_suffix(host: &str) -> Option<String> {
    let h = host.trim().trim_end_matches('.').to_ascii_lowercase();
    // An IPv6 literal, or an IPv4 one (a final label that's all digits can't be
    // a TLD) — `3.4` would be nonsense, so decline.
    if h.contains(':') || h.rsplit('.').next().is_some_and(|t| t.chars().all(|c| c.is_ascii_digit())) {
        return None;
    }
    let labels: Vec<&str> = h.split('.').filter(|l| !l.is_empty()).collect();
    let n = labels.len();
    if n < 2 {
        return None;
    }
    let take = if labels[n - 1].len() == 2 && REGISTRY_SLD.contains(&labels[n - 2]) { 3 } else { 2 };
    if n <= take {
        return None; // the host is already the registrable domain
    }
    Some(labels[n - take..].join("."))
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
            MetricsBand::Recent => [("1m", 60, true), ("5m", 300, true), ("1h", 3600, false), ("24h", 86_400, false)],
            MetricsBand::Days => [("1d", 86_400, false), ("3d", 259_200, false), ("5d", 432_000, false), ("7d", 604_800, false)],
            MetricsBand::Year => [("7d", 604_800, false), ("30d", 2_592_000, false), ("120d", 10_368_000, false), ("1y", 31_536_000, false)],
        }
    }
    pub fn spans(self) -> [i64; 4] {
        let c = self.cols();
        [c[0].1, c[1].1, c[2].1, c[3].1]
    }
}

/// One row of the connections pane, unified across all views (METRICS.md §5): a
/// host with its live stats AND its per-band byte history in both directions, so
/// `v` pans columns without reordering or re-querying. Live rows (`conns > 0`)
/// sort on top by throughput; dormant/historical rows (`conns == 0`) follow,
/// ranked by history and rendered greyed.
#[derive(Clone, Debug)]
pub struct ConnRow {
    pub host: String,
    pub port: u16,
    pub lane: Lane,
    pub conns: u32,     // live concurrency (0 = dormant/historical → greyed)
    pub live_up: f64,   // live cumulative bytes (the Live view's UP/DOWN columns)
    pub live_down: f64,
    pub rule: String,
    pub hist_up: [u64; 4], // per-band-column history (the ↑ metrics view)
    pub hist_down: [u64; 4], // the ↓ metrics view
}

impl ConnRow {
    /// Routed/copied/selection key — the domain (matches `Conn::key`).
    pub fn key(&self) -> String {
        self.host.clone()
    }
    pub fn is_live(&self) -> bool {
        self.conns > 0
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
            ErrKind::Dns => theme::transient(),
            ErrKind::Timeout | ErrKind::Reset | ErrKind::Refused => theme::persistent(),
            ErrKind::Blocked => theme::blocked(),
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

#[cfg(test)]
mod tests {
    use super::parent_suffix;

    #[test]
    fn parent_suffix_broadens_to_the_registrable_domain() {
        let s = |h: &str| parent_suffix(h).unwrap();
        assert_eq!(s("x.y.z.com"), "z.com");
        assert_eq!(s("polyfill.alicdn.com"), "alicdn.com");
        assert_eq!(s("i.ytimg.com"), "ytimg.com");
        // A generic-looking label under a generic TLD is still just a label —
        // the `is_generic` trap this rule exists to avoid.
        assert_eq!(s("a.b.cdn.com"), "cdn.com");
        // Case and a trailing root dot normalise away.
        assert_eq!(s("API.Anthropic.COM."), "anthropic.com");
    }

    #[test]
    fn parent_suffix_keeps_registry_second_levels_whole() {
        let s = |h: &str| parent_suffix(h).unwrap();
        assert_eq!(s("www.bbc.co.uk"), "bbc.co.uk");
        assert_eq!(s("x.y.z.co.uk"), "z.co.uk"); // not `co.uk` — that's a registry
        assert_eq!(s("x.y.com.cn"), "y.com.cn");
        assert_eq!(s("a.b.ne.jp"), "b.ne.jp");
        // Two-letter ccTLD, but the SLD is somebody's name, not a registry.
        assert_eq!(s("x.y.z.io"), "z.io");
        assert_eq!(s("mail.google.co"), "google.co");
    }

    #[test]
    fn parent_suffix_declines_when_there_is_nothing_to_broaden() {
        // Already the registrable domain — bare and registry-SLD forms.
        assert_eq!(parent_suffix("z.com"), None);
        assert_eq!(parent_suffix("y.com.cn"), None);
        assert_eq!(parent_suffix("bbc.co.uk"), None);
        // IP literals: `1.2.3.4` → `.3.4` would be nonsense.
        assert_eq!(parent_suffix("1.2.3.4"), None);
        assert_eq!(parent_suffix("2606:4700:4700::1111"), None);
        // Nothing to work with.
        assert_eq!(parent_suffix("localhost"), None);
        assert_eq!(parent_suffix(""), None);
    }


}
