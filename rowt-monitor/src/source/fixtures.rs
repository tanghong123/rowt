//! Static fixture reproducing the three golden renders in `renders/`. Values
//! are chosen so the formatters (`format.rs`) emit the exact strings shown.
//!
//! A small bounded random-walk is layered on the rates/#conn so the UI shows
//! motion during manual testing — anchored to the render values, never a
//! monotonic climb (see README: `#conn` is concurrency, not a counter).

use crate::model::*;
use crate::source::Source;

pub struct FixtureSource {
    tick: u64,
    /// Deterministic pseudo-random state (no `rand` dep; jitter for liveliness).
    seed: u64,
    /// When true, every field returns its exact anchor value (no jitter). Used
    /// by `--render` so the output matches the golden captures byte-for-byte.
    still: bool,
}

impl FixtureSource {
    pub fn new() -> Self {
        FixtureSource { tick: 0, seed: 0x9e3779b97f4a7c15, still: false }
    }

    /// A motionless fixture: anchors only, for golden-render diffing.
    pub fn still() -> Self {
        FixtureSource { tick: 0, seed: 0x9e3779b97f4a7c15, still: true }
    }

    fn next_unit(&mut self) -> f64 {
        // xorshift64* -> [0,1)
        let mut x = self.seed;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.seed = x;
        ((x.wrapping_mul(0x2545F4914F6CDD1D)) >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Jitter a rate by +/- pct around its anchor, clamped non-negative.
    fn jitter(&mut self, base: f64, pct: f64) -> f64 {
        if self.still {
            return base;
        }
        let d = (self.next_unit() * 2.0 - 1.0) * pct;
        (base * (1.0 + d)).max(0.0)
    }

    /// Random-walk a small concurrency count within [lo, hi] around base.
    fn walk_conn(&mut self, base: u32, lo: u32, hi: u32) -> u32 {
        if self.still {
            return base;
        }
        let step = (self.next_unit() * 3.0) as i32 - 1; // -1,0,1
        (base as i32 + step).clamp(lo as i32, hi as i32) as u32
    }
}

impl Default for FixtureSource {
    fn default() -> Self {
        Self::new()
    }
}

const KB: f64 = 1_000.0;
const MB: f64 = 1_000_000.0;

impl Source for FixtureSource {
    fn label(&self) -> &str {
        "fixtures"
    }

    fn poll(&mut self, _window: Window, _lane: Option<Lane>) -> Snapshot {
        // The demo fixture doesn't scope errors by lane (no per-lane fixture
        // data); the live source honours the filter.
        self.tick += 1;

        let conns = vec![
            conn(Lane::Escape, "i.ytimg.com", 443, self.walk_conn(5, 3, 8), self.jitter(8.0 * MB, 0.15), self.jitter(352.2 * MB, 0.1), "domain_suffix"),
            conn(Lane::Escape, "raw.githubusercontent.com", 443, self.walk_conn(2, 1, 4), self.jitter(915.0 * KB, 0.15), self.jitter(96.8 * MB, 0.1), "domain_suffix"),
            conn(Lane::Escape, "fonts.gstatic.com", 443, self.walk_conn(3, 1, 5), self.jitter(12.5 * MB, 0.15), self.jitter(43.1 * MB, 0.1), "domain_suffix"),
            conn(Lane::Escape, "www.youtube.com", 443, self.walk_conn(5, 3, 8), self.jitter(18.2 * MB, 0.15), self.jitter(427.7 * MB, 0.1), "domain_suffix"),
            conn(Lane::Escape, "api.anthropic.com", 443, self.walk_conn(2, 1, 4), self.jitter(376.1 * MB, 0.12), self.jitter(414.4 * MB, 0.1), "domain_suffix"),
            conn(Lane::Escape, "en.wikipedia.org", 443, self.walk_conn(1, 1, 3), self.jitter(1.4 * MB, 0.2), self.jitter(1.8 * MB, 0.2), "domain_suffix"),
            conn(Lane::Corp, "jira.corp.example.com", 443, self.walk_conn(5, 3, 7), self.jitter(8.1 * MB, 0.15), self.jitter(414.6 * MB, 0.1), "domain_suffix"),
            conn(Lane::Direct, "mirrors.aliyun.com", 443, self.walk_conn(1, 1, 3), self.jitter(509.0 * KB, 0.2), self.jitter(128.6 * MB, 0.12), "final"),
            conn(Lane::Direct, "dl.google.com", 443, self.walk_conn(1, 1, 3), self.jitter(104.0 * KB, 0.2), self.jitter(47.8 * MB, 0.12), "final"),
            conn(Lane::Direct, "gateway.icloud.com", 443, self.walk_conn(2, 1, 4), self.jitter(610.0 * KB, 0.2), self.jitter(519.0 * KB, 0.2), "final"),
        ];

        // Per-lane aggregates match the header rows in the renders.
        let lanes = vec![
            LaneAgg { lane: Lane::Escape, up: self.jitter(2.7 * MB, 0.06), down: self.jitter(3.5 * MB, 0.06), conns: 20 },
            LaneAgg { lane: Lane::Corp, up: self.jitter(144.0 * KB, 0.1), down: self.jitter(44.0 * KB, 0.1), conns: 3 },
            LaneAgg { lane: Lane::Direct, up: self.jitter(5.0 * KB, 0.15), down: self.jitter(2.0 * KB, 0.15), conns: 2 },
        ];
        let all = AllAgg {
            up: self.jitter(2.9 * MB, 0.05),
            down: self.jitter(3.5 * MB, 0.05),
            conns: 25,
        };

        let errors = vec![
            err(15, ErrKind::Timeout, "vpn.corp.example.com"),
            err(13, ErrKind::Blocked, "googlesyndication.com"),
            err(13, ErrKind::Blocked, "doubleclick.net"),
            err(12, ErrKind::Dns, "gateway.icloud.com"),
            err(11, ErrKind::Timeout, "dl.google.com"),
            err(11, ErrKind::Blocked, "criteo.com"),
            err(10, ErrKind::Blocked, "adnxs.com"),
            err(10, ErrKind::Blocked, "google-analytics.com"),
            err(10, ErrKind::Reset, "x.com"),
            err(8, ErrKind::Timeout, "rr5.googlevideo.com"),
        ];

        // The active server appears in the strip too, marked; the rest are the
        // idle-up pool. (total 10 = 9 up incl. active + 1 down.)
        let chips = vec![
            Server { name: "JP-Tokyo".into(), ms: 42, active: true },
            Server { name: "JP-Osaka".into(), ms: 175, active: false },
            Server { name: "KR-Seoul".into(), ms: 72, active: false },
            Server { name: "DE-Frankfurt".into(), ms: 195, active: false },
            Server { name: "HK-1".into(), ms: 82, active: false },
            Server { name: "SG-1".into(), ms: 81, active: false },
            Server { name: "TW-Taipei".into(), ms: 110, active: false },
            Server { name: "US-LA".into(), ms: 151, active: false },
            Server { name: "NL-Ams".into(), ms: 68, active: false },
        ];

        Snapshot {
            identity: Identity {
                mode: "host · en0".into(),
                uptime: "3h 15m".into(),
                server_name: "JP-Tokyo".into(),
                server_ms: Some(42),
                router: "running · :7890".into(),
                router_up: true,
                active_ok: Some(true),
                proxy: "on · Wi-Fi".into(),
                config: "host.json OK".into(),
                name_reserve: 8, // matches the golden (JP-Tokyo, ms at col 55)
            },
            all,
            lanes,
            conns,
            transient: ErrCat { count: 12, domains: 1 },
            persistent: ErrCat { count: 50, domains: 5 },
            blocked: ErrCat { count: 57, domains: 5 },
            errors,
            servers_total: 10,
            servers_up: 9,
            servers_down: 1,
            active_server: "JP-Tokyo".into(),
            chips,
        }
    }
}

fn conn(lane: Lane, host: &str, port: u16, conns: u32, up: f64, down: f64, rule: &str) -> Conn {
    Conn { lane, host: host.into(), port, conns, up, down, rule: rule.into() }
}

fn err(count: u32, kind: ErrKind, domain: &str) -> ErrRow {
    ErrRow { count, kind, domain: domain.into() }
}
