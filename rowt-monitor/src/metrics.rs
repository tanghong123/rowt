//! Traffic-metrics store: the SQLite schema, tiered rollup, and the queries that
//! both the collector (writer) and the monitor (reader) share. See METRICS.md.
//!
//! Tables `sample_5s / _1m / _1h / _1d` all have the same shape
//! `(ts, domain, lane, bytes_up, bytes_dn)` keyed by `(ts, domain, lane)`; `ts`
//! is the bucket-start epoch second. `bytes_up` is upload (↑), `bytes_dn` is
//! download (↓). Rollup folds each tier into the next as rows age past its
//! retention (§4). A `meta(k,v)` table holds `schema_version` and the collector
//! heartbeat (`pid`, `started`, `last_write`).

use std::collections::HashMap;
use std::path::PathBuf;

use rusqlite::{params, Connection};

pub const SCHEMA_VERSION: i64 = 1;

/// One resolution tier: its table, bucket step (seconds), and how long rows live
/// in it before being folded into the coarser tier below.
pub struct Tier {
    pub table: &'static str,
    pub step: i64,
    pub retain: i64,
}

/// 5s→1h, 1m→24h, 1h→90d, 1d→1y. Ordered fine→coarse (rollup walks pairs).
pub const TIERS: [Tier; 4] = [
    Tier { table: "sample_5s", step: 5, retain: 3600 },
    Tier { table: "sample_1m", step: 60, retain: 86_400 },
    Tier { table: "sample_1h", step: 3600, retain: 7_776_000 },
    Tier { table: "sample_1d", step: 86_400, retain: 31_536_000 },
];

/// `~/.config/rowt/metrics/traffic.db` (honoring `ROWT_CFG`, like `bin/rowt`).
pub fn db_path() -> PathBuf {
    let cfg = std::env::var("ROWT_CFG").ok().map(PathBuf::from).unwrap_or_else(|| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".config/rowt")
    });
    cfg.join("metrics").join("traffic.db")
}

/// Open (creating dirs + schema) the metrics DB in WAL mode.
pub fn open_db(path: &std::path::Path) -> rusqlite::Result<Connection> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "busy_timeout", 3000)?;
    init_schema(&conn)?;
    Ok(conn)
}

fn init_schema(conn: &Connection) -> rusqlite::Result<()> {
    let mut ddl = String::new();
    for t in &TIERS {
        ddl.push_str(&format!(
            "CREATE TABLE IF NOT EXISTS {} (\
               ts INTEGER NOT NULL, domain TEXT NOT NULL, lane TEXT NOT NULL,\
               bytes_up INTEGER NOT NULL DEFAULT 0, bytes_dn INTEGER NOT NULL DEFAULT 0,\
               PRIMARY KEY (ts, domain, lane)) WITHOUT ROWID;",
            t.table
        ));
    }
    ddl.push_str("CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT);");
    conn.execute_batch(&ddl)?;
    set_meta(conn, "schema_version", &SCHEMA_VERSION.to_string());
    Ok(())
}

// ---------------- meta / heartbeat ----------------

pub fn set_meta(conn: &Connection, k: &str, v: &str) {
    let _ = conn.execute(
        "INSERT INTO meta(k,v) VALUES(?1,?2) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
        params![k, v],
    );
}

pub fn get_meta(conn: &Connection, k: &str) -> Option<String> {
    conn.query_row("SELECT v FROM meta WHERE k=?1", params![k], |r| r.get(0)).ok()
}

fn get_meta_i64(conn: &Connection, k: &str) -> Option<i64> {
    get_meta(conn, k).and_then(|s| s.parse().ok())
}

// ---------------- writes ----------------

/// In-flight per-bucket accumulation: `(domain, lane) -> (up, dn)` bytes.
pub type Bucket = HashMap<(String, String), (u64, u64)>;

/// Add one connection's per-frame delta to the current bucket.
pub fn bucket_add(bucket: &mut Bucket, domain: &str, lane: &str, d_up: u64, d_dn: u64) {
    if d_up == 0 && d_dn == 0 {
        return;
    }
    let e = bucket.entry((domain.to_string(), lane.to_string())).or_default();
    e.0 += d_up;
    e.1 += d_dn;
}

/// Persist a completed 5-second bucket (upsert-accumulate), stamping `last_write`.
pub fn flush_bucket(conn: &Connection, ts: i64, bucket: &Bucket) -> rusqlite::Result<()> {
    if bucket.is_empty() {
        set_meta(conn, "last_write", &ts.to_string());
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO sample_5s(ts,domain,lane,bytes_up,bytes_dn) VALUES(?1,?2,?3,?4,?5) \
             ON CONFLICT(ts,domain,lane) DO UPDATE SET \
               bytes_up=bytes_up+excluded.bytes_up, bytes_dn=bytes_dn+excluded.bytes_dn",
        )?;
        for ((domain, lane), (up, dn)) in bucket {
            stmt.execute(params![ts, domain, lane, *up as i64, *dn as i64])?;
        }
    }
    tx.commit()?;
    set_meta(conn, "last_write", &ts.to_string());
    Ok(())
}

/// Fold each tier's rows older than its retention into the coarser tier, then
/// trim the coarsest past a year. Idempotent; cheap to call once a minute.
pub fn rollup(conn: &Connection, now: i64) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    for pair in TIERS.windows(2) {
        let (fine, coarse) = (&pair[0], &pair[1]);
        let cutoff = now - fine.retain;
        tx.execute(
            &format!(
                "INSERT INTO {c}(ts,domain,lane,bytes_up,bytes_dn) \
                 SELECT (ts/{step})*{step}, domain, lane, sum(bytes_up), sum(bytes_dn) \
                 FROM {f} WHERE ts < ?1 GROUP BY (ts/{step})*{step}, domain, lane \
                 ON CONFLICT(ts,domain,lane) DO UPDATE SET \
                   bytes_up=bytes_up+excluded.bytes_up, bytes_dn=bytes_dn+excluded.bytes_dn",
                c = coarse.table,
                f = fine.table,
                step = coarse.step,
            ),
            params![cutoff],
        )?;
        tx.execute(&format!("DELETE FROM {} WHERE ts < ?1", fine.table), params![cutoff])?;
    }
    // Trim the coarsest tier past its retention.
    let last = TIERS.last().unwrap();
    tx.execute(&format!("DELETE FROM {} WHERE ts < ?1", last.table), params![now - last.retain])?;
    tx.commit()?;
    Ok(())
}

/// The collector heartbeat (`last_write` epoch seconds), or None if the store
/// doesn't exist yet. Opens read-only-ish; never creates the DB. Cheap enough to
/// call on the monitor's 2s tick.
pub fn last_write(path: &std::path::Path) -> Option<i64> {
    if !path.exists() {
        return None;
    }
    let conn = Connection::open(path).ok()?;
    get_meta(&conn, "last_write").and_then(|s| s.parse().ok())
}

// ---------------- reads (monitor) ----------------

/// Collector liveness as the monitor sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollectorState {
    /// Heartbeat fresh — collecting. `last_write` within `fresh_secs`.
    On,
    /// Heartbeat stale — process may be up but not writing (router wedged/down).
    Stale,
    /// No DB / never written.
    Absent,
}

/// Read the collector heartbeat. `now` is wall-clock epoch seconds; a write
/// within `fresh` seconds counts as live.
pub fn collector_state(conn: &Connection, now: i64, fresh: i64) -> CollectorState {
    match get_meta_i64(conn, "last_write") {
        Some(w) if now - w <= fresh => CollectorState::On,
        Some(_) => CollectorState::Stale,
        None => CollectorState::Absent,
    }
}

/// One domain's totals over a window, for the flipped connections view.
#[derive(Clone, Debug)]
pub struct DomTotal {
    pub domain: String,
    pub lane: String,
    pub bytes_up: u64,
    pub bytes_dn: u64,
}

/// Pick the tier whose resolution best covers a trailing window of `span` seconds
/// (§5): ≤1h→5s, ≤24h→1m, ≤90d→1h, else 1d.
pub fn tier_for(span: i64) -> &'static Tier {
    TIERS.iter().find(|t| span <= t.retain).unwrap_or(TIERS.last().unwrap())
}

/// Top domains by total bytes over the trailing window `[now-span, now]`, summed
/// from the covering tier. `lane` filters (None = all lanes, excludes the
/// `(unattributed)` reconciliation row unless `include_unattr`). Ranked by the
/// chosen direction (`up`) descending.
pub fn top_domains(
    conn: &Connection,
    now: i64,
    span: i64,
    lane: Option<&str>,
    up: bool,
    limit: usize,
    include_unattr: bool,
) -> rusqlite::Result<Vec<DomTotal>> {
    let tier = tier_for(span);
    let order = if up { "s_up" } else { "s_dn" };
    let lane_clause = match lane {
        Some(_) => "AND lane = ?3",
        None if include_unattr => "",
        None => "AND lane <> '-'",
    };
    let sql = format!(
        "SELECT domain, \
                CASE WHEN ?4 THEN '' ELSE max(lane) END AS lane, \
                sum(bytes_up) AS s_up, sum(bytes_dn) AS s_dn \
         FROM {t} WHERE ts >= ?1 {lane_clause} GROUP BY domain \
         ORDER BY {order} DESC LIMIT ?2",
        t = tier.table,
        lane_clause = lane_clause,
        order = order,
    );
    let mut stmt = conn.prepare(&sql)?;
    let group_all_lanes = lane.is_none();
    let rows = stmt.query_map(
        params![now - span, limit as i64, lane.unwrap_or(""), group_all_lanes],
        |r| {
            Ok(DomTotal {
                domain: r.get(0)?,
                lane: r.get(1)?,
                bytes_up: r.get::<_, i64>(2)? as u64,
                bytes_dn: r.get::<_, i64>(3)? as u64,
            })
        },
    )?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.pragma_update(None, "journal_mode", "MEMORY").unwrap();
        init_schema(&c).unwrap();
        c
    }

    #[test]
    fn flush_upsert_accumulates() {
        let c = mem();
        let mut b: Bucket = Bucket::new();
        bucket_add(&mut b, "api.anthropic.com", "escape", 100, 200);
        bucket_add(&mut b, "api.anthropic.com", "escape", 10, 20); // same key merges
        flush_bucket(&c, 1000, &b).unwrap();
        // Second flush into the SAME bucket ts accumulates in SQL.
        let mut b2: Bucket = Bucket::new();
        bucket_add(&mut b2, "api.anthropic.com", "escape", 1, 2);
        flush_bucket(&c, 1000, &b2).unwrap();
        let (up, dn): (i64, i64) = c
            .query_row(
                "SELECT bytes_up, bytes_dn FROM sample_5s WHERE ts=1000 AND domain='api.anthropic.com'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((up, dn), (111, 222));
        assert_eq!(get_meta(&c, "last_write").as_deref(), Some("1000"));
    }

    #[test]
    fn zero_delta_is_dropped() {
        let mut b: Bucket = Bucket::new();
        bucket_add(&mut b, "x", "direct", 0, 0);
        assert!(b.is_empty());
    }

    #[test]
    fn rollup_folds_5s_into_1m_and_deletes() {
        let c = mem();
        let now = 10_000_000;
        // Two 5s buckets in the same minute, both older than 1h.
        let old = now - 7200;
        let m = (old / 60) * 60;
        for (ts, up) in [(m, 3u64), (m + 5, 4u64)] {
            let mut b: Bucket = Bucket::new();
            bucket_add(&mut b, "d", "escape", up, up * 10);
            flush_bucket(&c, ts, &b).unwrap();
        }
        rollup(&c, now).unwrap();
        let n5: i64 = c.query_row("SELECT count(*) FROM sample_5s", [], |r| r.get(0)).unwrap();
        assert_eq!(n5, 0, "aged-out 5s rows removed");
        let (ts, up, dn): (i64, i64, i64) = c
            .query_row("SELECT ts, bytes_up, bytes_dn FROM sample_1m", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert_eq!(ts, m, "folded to the minute bucket");
        assert_eq!((up, dn), (7, 70), "summed across the two 5s buckets");
    }

    #[test]
    fn top_domains_ranks_and_filters() {
        let c = mem();
        let now = 10_000;
        for (dom, lane, up, dn) in [
            ("a.com", "escape", 100u64, 900u64),
            ("b.com", "direct", 50, 50),
            ("(unattributed)", "-", 1, 1),
        ] {
            let mut b: Bucket = Bucket::new();
            bucket_add(&mut b, dom, lane, up, dn);
            flush_bucket(&c, now - 10, &b).unwrap();
        }
        // By download, all lanes, excluding the reconciliation row.
        let dn = top_domains(&c, now, 3600, None, false, 10, false).unwrap();
        assert_eq!(dn[0].domain, "a.com");
        assert!(!dn.iter().any(|d| d.domain == "(unattributed)"));
        // Lane filter = direct keeps only b.com.
        let d2 = top_domains(&c, now, 3600, Some("direct"), true, 10, false).unwrap();
        assert_eq!(d2.len(), 1);
        assert_eq!(d2[0].domain, "b.com");
    }
}
