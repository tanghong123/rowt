//! Pure parsers for the live data sources — kept free of I/O so they can be
//! unit-tested against captured sample payloads. `live.rs` wires them to the
//! clash API, config/state files, and lane logs.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::model::*;

// ---------------- key=value state file ----------------

/// Parse `~/.config/rowt/state` (`key=value` lines).
pub fn parse_state(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

// ---------------- clash /connections ----------------

#[derive(Clone, Debug)]
pub struct RawConn {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub up: u64,   // cumulative bytes
    pub down: u64, // cumulative bytes
    pub chains: Vec<String>,
    pub rule: String,
}

/// Parse the clash `/connections` payload into raw per-connection records.
pub fn parse_connections(v: &Value) -> Vec<RawConn> {
    let mut out = Vec::new();
    let Some(arr) = v.get("connections").and_then(|c| c.as_array()) else {
        return out;
    };
    for c in arr {
        let meta = c.get("metadata").cloned().unwrap_or(Value::Null);
        let host = meta
            .get("host")
            .and_then(|h| h.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| meta.get("destinationIP").and_then(|h| h.as_str()))
            .unwrap_or("")
            .to_string();
        let port = meta
            .get("destinationPort")
            .map(port_of)
            .unwrap_or(0);
        let chains = c
            .get("chains")
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        out.push(RawConn {
            id: c.get("id").and_then(|s| s.as_str()).unwrap_or("").to_string(),
            host,
            port,
            up: c.get("upload").and_then(u64_of).unwrap_or(0),
            down: c.get("download").and_then(u64_of).unwrap_or(0),
            chains,
            rule: normalize_rule(c.get("rule").and_then(|s| s.as_str()).unwrap_or("")),
        });
    }
    out
}

fn u64_of(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_f64().map(|f| f as u64))
}

fn port_of(v: &Value) -> u16 {
    v.as_u64()
        .map(|n| n as u16)
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0)
}

/// Clash reports rule kinds as e.g. `DomainSuffix`, `Final`; the UI shows the
/// sing-box snake_case form (`domain_suffix`, `final`).
pub fn normalize_rule(r: &str) -> String {
    if r.is_empty() {
        return "final".to_string();
    }
    if r.chars().any(|c| c.is_uppercase()) && !r.contains('_') {
        // CamelCase -> snake_case
        let mut out = String::new();
        for (i, ch) in r.chars().enumerate() {
            if ch.is_uppercase() {
                if i > 0 {
                    out.push('_');
                }
                out.extend(ch.to_lowercase());
            } else {
                out.push(ch);
            }
        }
        out
    } else {
        r.to_string()
    }
}

/// Which lane a connection took, from its outbound chain.
pub fn classify_lane(chains: &[String], escape_tags: &HashSet<String>) -> Lane {
    let has = |name: &str| chains.iter().any(|c| c == name);
    if has("block") {
        Lane::Block
    } else if has("corp") {
        Lane::Corp
    } else if has("escape") || chains.iter().any(|c| escape_tags.contains(c)) {
        Lane::Escape
    } else {
        Lane::Direct // explicit "direct" chain, or route final = direct
    }
}

/// Build the connections table + per-lane aggregates + the `all` aggregate.
/// `rates` maps connection id -> (up_bytes_per_s, down_bytes_per_s). Block-lane
/// connections are excluded from both the list and the throughput totals.
pub fn build_conn_rows(
    raw: &[RawConn],
    rates: &HashMap<String, (f64, f64)>,
    escape_tags: &HashSet<String>,
) -> (Vec<Conn>, Vec<LaneAgg>, AllAgg) {
    // Group by (lane, host, port) -> concurrency + summed rates.
    struct Agg {
        lane: Lane,
        host: String,
        port: u16,
        conns: u32,
        up: f64,
        down: f64,
        rule: String,
    }
    let mut groups: Vec<Agg> = Vec::new();
    let mut lane_up: HashMap<Lane, f64> = HashMap::new();
    let mut lane_down: HashMap<Lane, f64> = HashMap::new();
    let mut lane_conns: HashMap<Lane, u32> = HashMap::new();

    for c in raw {
        let lane = classify_lane(&c.chains, escape_tags);
        if lane == Lane::Block {
            continue;
        }
        let (u, d) = rates.get(&c.id).copied().unwrap_or((0.0, 0.0));
        *lane_up.entry(lane).or_default() += u;
        *lane_down.entry(lane).or_default() += d;
        *lane_conns.entry(lane).or_default() += 1;

        if let Some(g) = groups.iter_mut().find(|g| g.host == c.host && g.port == c.port && g.lane == lane) {
            g.conns += 1;
            g.up += u;
            g.down += d;
        } else {
            groups.push(Agg {
                lane,
                host: c.host.clone(),
                port: c.port,
                conns: 1,
                up: u,
                down: d,
                rule: c.rule.clone(),
            });
        }
    }

    // Sort rows by throughput (down+up) desc for a stable, useful ordering.
    groups.sort_by(|a, b| (b.down + b.up).total_cmp(&(a.down + a.up)));
    let conns: Vec<Conn> = groups
        .into_iter()
        .map(|g| Conn {
            lane: g.lane,
            host: g.host,
            port: g.port,
            conns: g.conns,
            up: g.up,
            down: g.down,
            rule: g.rule,
        })
        .collect();

    let mut lanes = Vec::new();
    for lane in Lane::FILTERABLE {
        lanes.push(LaneAgg {
            lane,
            up: lane_up.get(&lane).copied().unwrap_or(0.0),
            down: lane_down.get(&lane).copied().unwrap_or(0.0),
            conns: lane_conns.get(&lane).copied().unwrap_or(0),
        });
    }
    let all = AllAgg {
        up: lane_up.values().sum(),
        down: lane_down.values().sum(),
        conns: lane_conns.values().sum(),
    };
    (conns, lanes, all)
}

// ---------------- lane logs ----------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ErrEvent {
    pub secs: i64,
    pub domain: String,
    pub kind: ErrKind,
}

/// Classify a lane-log error message into a UI category.
pub fn classify_err(is_block: bool, msg: &str) -> ErrKind {
    if is_block {
        return ErrKind::Blocked;
    }
    let m = msg.to_ascii_lowercase();
    if m.contains("lookup") || m.contains("no such host") || m.contains("server misbehaving") {
        ErrKind::Dns
    } else if m.contains("refused") {
        ErrKind::Refused
    } else if m.contains("reset") {
        ErrKind::Reset
    } else if m.contains("timeout") || m.contains("deadline exceeded") || m.contains("timed out") {
        ErrKind::Timeout
    } else {
        // unreachable / broken pipe / etc. — persistent
        ErrKind::Reset
    }
}

/// Parse a `lane-*.log` (TSV: `timestamp\tdomain\tmessage`).
pub fn parse_lane_log(text: &str, is_block: bool) -> Vec<ErrEvent> {
    text.lines()
        .filter_map(|line| {
            let mut it = line.splitn(3, '\t');
            let ts = it.next()?;
            let domain = it.next()?;
            let msg = it.next().unwrap_or("");
            let secs = parse_ts(ts)?;
            Some(ErrEvent {
                secs,
                domain: domain.to_string(),
                kind: classify_err(is_block, msg),
            })
        })
        .collect()
}

/// Aggregate events within the window (referenced to the newest event, so it is
/// timezone-agnostic). Returns (transient, persistent, blocked, rows).
/// (transient, persistent, blocked, per-domain rows sorted by count).
pub type ErrAgg = (ErrCat, ErrCat, ErrCat, Vec<ErrRow>);

pub fn aggregate_errors(events: &[ErrEvent], window_secs: i64) -> ErrAgg {
    let now = events.iter().map(|e| e.secs).max().unwrap_or(0);
    let cutoff = now - window_secs;

    let mut tr_ev = 0u32;
    let mut pe_ev = 0u32;
    let mut bl_ev = 0u32;
    let mut tr_dom: HashSet<&str> = HashSet::new();
    let mut pe_dom: HashSet<&str> = HashSet::new();
    let mut bl_dom: HashSet<&str> = HashSet::new();

    // per-domain: total count + kind tally
    let mut per: HashMap<&str, (u32, HashMap<ErrKind, u32>)> = HashMap::new();

    for e in events.iter().filter(|e| e.secs >= cutoff) {
        match category(e.kind) {
            Cat::Transient => {
                tr_ev += 1;
                tr_dom.insert(&e.domain);
            }
            Cat::Persistent => {
                pe_ev += 1;
                pe_dom.insert(&e.domain);
            }
            Cat::Blocked => {
                bl_ev += 1;
                bl_dom.insert(&e.domain);
            }
        }
        let entry = per.entry(&e.domain).or_insert((0, HashMap::new()));
        entry.0 += 1;
        *entry.1.entry(e.kind).or_default() += 1;
    }

    let mut rows: Vec<ErrRow> = per
        .into_iter()
        .map(|(dom, (count, kinds))| {
            let kind = kinds
                .into_iter()
                .max_by_key(|(_, n)| *n)
                .map(|(k, _)| k)
                .unwrap_or(ErrKind::Reset);
            ErrRow {
                count,
                kind,
                domain: dom.to_string(),
            }
        })
        .collect();
    rows.sort_by(|a, b| b.count.cmp(&a.count).then(a.domain.cmp(&b.domain)));

    (
        ErrCat { count: tr_ev, domains: tr_dom.len() as u32 },
        ErrCat { count: pe_ev, domains: pe_dom.len() as u32 },
        ErrCat { count: bl_ev, domains: bl_dom.len() as u32 },
        rows,
    )
}

enum Cat {
    Transient,
    Persistent,
    Blocked,
}

fn category(k: ErrKind) -> Cat {
    match k {
        ErrKind::Dns => Cat::Transient,
        ErrKind::Timeout | ErrKind::Reset | ErrKind::Refused => Cat::Persistent,
        ErrKind::Blocked => Cat::Blocked,
    }
}

pub fn window_secs(w: Window) -> i64 {
    match w {
        Window::M5 => 5 * 60,
        Window::M10 => 10 * 60,
        Window::H1 => 60 * 60,
        Window::H24 => 24 * 60 * 60,
    }
}

// ---------------- time ----------------

/// Parse `YYYY-MM-DD HH:MM:SS` to seconds (tz-agnostic; only differences are
/// used, and all lane-log timestamps share one clock).
pub fn parse_ts(s: &str) -> Option<i64> {
    let (d, t) = s.split_once(' ')?;
    let mut di = d.split('-');
    let y: i64 = di.next()?.trim().parse().ok()?;
    let m: i64 = di.next()?.parse().ok()?;
    let da: i64 = di.next()?.parse().ok()?;
    let mut ti = t.split(':');
    let h: i64 = ti.next()?.parse().ok()?;
    let mi: i64 = ti.next()?.parse().ok()?;
    let se: i64 = ti.next()?.trim().parse().ok()?;
    Some(days_from_civil(y, m, da) * 86400 + h * 3600 + mi * 60 + se)
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn state_parse() {
        let s = parse_state("clash_secret=abc\nselected=Hong-Server\nmode=host\n");
        assert_eq!(s["selected"], "Hong-Server");
        assert_eq!(s["mode"], "host");
    }

    #[test]
    fn ts_and_civil() {
        // 2026-07-04 15:25:44 vs 2026-07-04 15:20:44 == 300s apart
        let a = parse_ts("2026-07-04 15:25:44").unwrap();
        let b = parse_ts("2026-07-04 15:20:44").unwrap();
        assert_eq!(a - b, 300);
        // 1970-01-01 00:00:00 == 0
        assert_eq!(parse_ts("1970-01-01 00:00:00").unwrap(), 0);
    }

    #[test]
    fn rule_normalization() {
        assert_eq!(normalize_rule("DomainSuffix"), "domain_suffix");
        assert_eq!(normalize_rule("Final"), "final");
        assert_eq!(normalize_rule("domain_suffix"), "domain_suffix");
        assert_eq!(normalize_rule(""), "final");
    }

    #[test]
    fn lane_classification() {
        let esc: HashSet<String> = ["Hong-Server", "Ds415"].iter().map(|s| s.to_string()).collect();
        assert_eq!(classify_lane(&["Hong-Server".into(), "escape".into()], &esc), Lane::Escape);
        assert_eq!(classify_lane(&["Ds415".into()], &esc), Lane::Escape);
        assert_eq!(classify_lane(&["corp".into()], &esc), Lane::Corp);
        assert_eq!(classify_lane(&["block".into()], &esc), Lane::Block);
        assert_eq!(classify_lane(&["direct".into()], &esc), Lane::Direct);
        assert_eq!(classify_lane(&[], &esc), Lane::Direct);
    }

    #[test]
    fn err_classification() {
        assert_eq!(classify_err(true, "operation not permitted"), ErrKind::Blocked);
        assert_eq!(classify_err(false, "lookup x: broken pipe"), ErrKind::Dns);
        assert_eq!(classify_err(false, "dial tcp 1.2.3.4:443: i/o timeout"), ErrKind::Timeout);
        assert_eq!(classify_err(false, "connect: connection refused"), ErrKind::Refused);
        assert_eq!(classify_err(false, "read: connection reset by peer"), ErrKind::Reset);
        assert_eq!(classify_err(false, "connect: network is unreachable"), ErrKind::Reset);
    }

    #[test]
    fn connections_parse_and_aggregate() {
        let v = json!({
            "connections": [
                {"id":"a","metadata":{"host":"api.anthropic.com","destinationPort":"443"},
                 "upload":100,"download":200,"chains":["Hong-Server","escape"],"rule":"DomainSuffix"},
                {"id":"b","metadata":{"host":"api.anthropic.com","destinationPort":"443"},
                 "upload":10,"download":20,"chains":["Hong-Server","escape"],"rule":"DomainSuffix"},
                {"id":"c","metadata":{"host":"ads.example","destinationPort":"443"},
                 "upload":5,"download":5,"chains":["block"],"rule":"RuleSet"},
                {"id":"d","metadata":{"host":"baidu.com","destinationPort":"443"},
                 "upload":1,"download":1,"chains":["direct"],"rule":"Final"}
            ]
        });
        let raw = parse_connections(&v);
        assert_eq!(raw.len(), 4);
        let esc: HashSet<String> = ["Hong-Server"].iter().map(|s| s.to_string()).collect();
        let mut rates = HashMap::new();
        rates.insert("a".to_string(), (1000.0, 2000.0));
        rates.insert("b".to_string(), (100.0, 200.0));
        rates.insert("d".to_string(), (5.0, 5.0));
        let (conns, lanes, all) = build_conn_rows(&raw, &rates, &esc);
        // block excluded; anthropic grouped (concurrency 2)
        let anth = conns.iter().find(|c| c.host == "api.anthropic.com").unwrap();
        assert_eq!(anth.conns, 2);
        assert_eq!(anth.lane, Lane::Escape);
        assert_eq!(anth.rule, "domain_suffix");
        assert_eq!(anth.up, 1100.0);
        assert!(!conns.iter().any(|c| c.host == "ads.example"), "block excluded");
        let esc_lane = lanes.iter().find(|l| l.lane == Lane::Escape).unwrap();
        assert_eq!(esc_lane.conns, 2);
        assert_eq!(all.conns, 3); // escape 2 + direct 1, block excluded
        assert_eq!(all.down, 2205.0);
    }

    #[test]
    fn error_aggregation_window() {
        let log = "\
2026-07-04 10:00:00\tvpn.corp.example.com\tdial tcp 1.1.1.1:443: i/o timeout
2026-07-04 10:00:10\tvpn.corp.example.com\tdial tcp 1.1.1.1:443: i/o timeout
2026-07-04 10:05:00\tgateway.icloud.com\tlookup gateway.icloud.com: broken pipe
2026-07-04 09:00:00\told.example.com\tdial tcp: i/o timeout";
        let events = parse_lane_log(log, false);
        assert_eq!(events.len(), 4);
        // 10m window referenced to newest (10:05:00) -> excludes the 09:00 event
        let (tr, pe, bl, rows) = aggregate_errors(&events, 10 * 60);
        assert_eq!(pe.count, 2); // two timeouts
        assert_eq!(pe.domains, 1);
        assert_eq!(tr.count, 1); // one dns
        assert_eq!(bl.count, 0);
        assert_eq!(rows[0].domain, "vpn.corp.example.com");
        assert_eq!(rows[0].count, 2);
        assert_eq!(rows[0].kind, ErrKind::Timeout);
        assert!(!rows.iter().any(|r| r.domain == "old.example.com"), "outside window");
    }
}
