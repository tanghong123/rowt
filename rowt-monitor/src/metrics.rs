//! Traffic-metrics store: the SQLite schema, tiered rollup, and the queries that
//! both the collector (writer) and the monitor (reader) share. See METRICS.md.
//!
//! Tables `sample_5s / _1m / _1h / _1d` all have the same shape
//! `(ts, domain, lane, bytes_up, bytes_dn, port)` keyed by
//! `(ts, domain, lane, port)`; `ts` is the bucket-start epoch second. `bytes_up`
//! is upload (↑), `bytes_dn` is download (↓). `port` is the rowt port the bytes
//! came in on (§3). Rollup folds each tier into the next as rows age past its
//! retention (§4). A `meta(k,v)` table holds `schema_version` and the collector
//! heartbeat (`pid`, `started`, `last_write`).

use std::collections::HashMap;
use std::path::PathBuf;

use rusqlite::{params, Connection, Transaction, TransactionBehavior};

/// 2 added `port` to every tier (and to the key). A version-1 store is migrated
/// in place by `open_db`, its rows marked `NO_PORT`.
pub const SCHEMA_VERSION: i64 = 2;

/// The port of main, rowt's own listener (127.0.0.1:7890, inbound `in`), as
/// `rowt port list` names it.
pub const MAIN_PORT: &str = "main";
/// The port is not known: rows recorded before the store had a port column, and
/// the reconciled remainder, which the grand totals cannot split by port.
pub const NO_PORT: &str = "-";

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

/// One tier's table. `port` comes last so the first five columns keep the
/// places they had in version 1, for a `SELECT *` written against it.
fn tier_ddl(table: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {table} (\
           ts INTEGER NOT NULL, domain TEXT NOT NULL, lane TEXT NOT NULL,\
           bytes_up INTEGER NOT NULL DEFAULT 0, bytes_dn INTEGER NOT NULL DEFAULT 0,\
           port TEXT NOT NULL DEFAULT '{NO_PORT}',\
           PRIMARY KEY (ts, domain, lane, port)) WITHOUT ROWID;"
    )
}

fn table_exists(conn: &Connection, table: &str) -> rusqlite::Result<bool> {
    conn.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1", params![table], |r| r.get::<_, i64>(0))
        .map(|n| n > 0)
}

/// Does this tier have the `port` column? Read from the table itself, not from
/// `schema_version`: a version-1 collector run against a migrated store (a
/// downgrade) writes `schema_version=1` back while the table keeps its port.
pub fn has_port(conn: &Connection, table: &str) -> rusqlite::Result<bool> {
    conn.query_row("SELECT count(*) FROM pragma_table_info(?1) WHERE name='port'", params![table], |r| r.get::<_, i64>(0))
        .map(|n| n > 0)
}

fn init_schema(conn: &Connection) -> rusqlite::Result<()> {
    // One transaction: a reader sees every tier before or every tier after,
    // and an interrupted migration leaves version 1 whole. IMMEDIATE takes the
    // write lock up front, under busy_timeout: a deferred one that reads and
    // then writes is refused at once, without waiting, if another writer
    // committed in between.
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    for t in &TIERS {
        // Version 1 was keyed (ts, domain, lane). A key cannot be altered in
        // place, so rebuild: rename, create the new shape, copy, drop.
        if table_exists(&tx, t.table)? && !has_port(&tx, t.table)? {
            tx.execute_batch(&format!(
                "ALTER TABLE {t} RENAME TO {t}_v1; {ddl} \
                 INSERT INTO {t}(ts,domain,lane,bytes_up,bytes_dn,port) \
                   SELECT ts, domain, lane, bytes_up, bytes_dn, '{NO_PORT}' FROM {t}_v1; \
                 DROP TABLE {t}_v1;",
                t = t.table,
                ddl = tier_ddl(t.table),
            ))?;
        }
        tx.execute_batch(&tier_ddl(t.table))?;
    }
    tx.execute_batch("CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT);")?;
    set_meta(&tx, "schema_version", &SCHEMA_VERSION.to_string());
    tx.commit()
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

// ---------------- writes ----------------

/// In-flight per-bucket accumulation: `(domain, lane, port) -> (up, dn)` bytes.
pub type Bucket = HashMap<(String, String, String), (u64, u64)>;

/// Add one connection's per-frame delta to the current bucket.
pub fn bucket_add(bucket: &mut Bucket, domain: &str, lane: &str, port: &str, d_up: u64, d_dn: u64) {
    if d_up == 0 && d_dn == 0 {
        return;
    }
    let e = bucket.entry((domain.to_string(), lane.to_string(), port.to_string())).or_default();
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
            "INSERT INTO sample_5s(ts,domain,lane,port,bytes_up,bytes_dn) VALUES(?1,?2,?3,?4,?5,?6) \
             ON CONFLICT(ts,domain,lane,port) DO UPDATE SET \
               bytes_up=bytes_up+excluded.bytes_up, bytes_dn=bytes_dn+excluded.bytes_dn",
        )?;
        for ((domain, lane, port), (up, dn)) in bucket {
            stmt.execute(params![ts, domain, lane, port, *up as i64, *dn as i64])?;
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
                "INSERT INTO {c}(ts,domain,lane,port,bytes_up,bytes_dn) \
                 SELECT (ts/{step})*{step}, domain, lane, port, sum(bytes_up), sum(bytes_dn) \
                 FROM {f} WHERE ts < ?1 GROUP BY (ts/{step})*{step}, domain, lane, port \
                 ON CONFLICT(ts,domain,lane,port) DO UPDATE SET \
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

/// Open the store for reading (None if it doesn't exist yet — never creates it).
pub fn open_read(path: &std::path::Path) -> Option<Connection> {
    if !path.exists() {
        return None;
    }
    Connection::open(path).ok()
}

// ---------------- reads (monitor) ----------------

/// Per-domain byte history for the connections pane's flipped views: for each of
/// the band's four spans, sum `bytes_up` and `bytes_dn` per domain over the
/// trailing window `[now-span, now]`, UNIONed across all tiers (a window spans
/// tiers — the recent hour is only in sample_5s, older data only in the coarser
/// ones; post-rollup they are time-disjoint, so UNION ALL sums without double
/// counting). Lanes are merged (one entry per domain) with the dominant lane kept
/// for coloring. `lane` filters to one lane, `port` to one rowt port. A store
/// still at version 1 (its collector not yet restarted after an upgrade) reads
/// as all `NO_PORT`, so it shows whole with no port filter and empty with one.
/// Returns `domain -> (dominant_lane_label, [up per column], [dn per column])`.
pub fn history_map(
    conn: &Connection,
    now: i64,
    spans: [i64; 4],
    lane: Option<&str>,
    port: Option<&str>,
) -> rusqlite::Result<HashMap<String, (String, [u64; 4], [u64; 4])>> {
    let lane_arg = lane.unwrap_or("");
    let port_arg = port.unwrap_or("");
    let mut parts = Vec::with_capacity(TIERS.len());
    for t in &TIERS {
        let port_col = if has_port(conn, t.table)? { "port".to_string() } else { format!("'{NO_PORT}'") };
        parts.push(format!("SELECT domain, lane, {port_col} AS port, bytes_up AS u, bytes_dn AS d FROM {} WHERE ts >= ?1", t.table));
    }
    let union = parts.join(" UNION ALL ");
    let mut up: HashMap<String, [u64; 4]> = HashMap::new();
    let mut dn: HashMap<String, [u64; 4]> = HashMap::new();
    // Dominant lane per domain, weighted by total bytes over the widest span.
    let mut lane_bytes: HashMap<String, HashMap<String, u64>> = HashMap::new();
    for (ci, &span) in spans.iter().enumerate() {
        // `?2 = ''` / `?3 = ''` disable the lane / port filter; '-'
        // (unattributed) is always excluded.
        let sql = format!(
            "SELECT domain, lane, sum(u), sum(d) FROM ({union}) \
             WHERE (?2 = '' OR lane = ?2) AND (?3 = '' OR port = ?3) AND lane <> '-' GROUP BY domain, lane",
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(params![now - span, lane_arg, port_arg])?;
        let widest = ci == spans.len() - 1;
        while let Some(r) = rows.next()? {
            let d: String = r.get(0)?;
            let l: String = r.get(1)?;
            let su = r.get::<_, i64>(2)?.max(0) as u64;
            let sd = r.get::<_, i64>(3)?.max(0) as u64;
            up.entry(d.clone()).or_default()[ci] += su;
            dn.entry(d.clone()).or_default()[ci] += sd;
            if widest {
                *lane_bytes.entry(d).or_default().entry(l).or_default() += su + sd;
            }
        }
    }
    Ok(up
        .into_iter()
        .map(|(d, ucols)| {
            let dcols = dn.get(&d).copied().unwrap_or_default();
            let lane = lane_bytes
                .get(&d)
                .and_then(|m| m.iter().max_by_key(|(_, v)| **v).map(|(k, _)| k.clone()))
                .unwrap_or_else(|| "direct".into());
            (d, (lane, ucols, dcols))
        })
        .collect())
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
        bucket_add(&mut b, "api.anthropic.com", "escape", "main", 100, 200);
        bucket_add(&mut b, "api.anthropic.com", "escape", "main", 10, 20); // same key merges
        flush_bucket(&c, 1000, &b).unwrap();
        // Second flush into the SAME bucket ts accumulates in SQL.
        let mut b2: Bucket = Bucket::new();
        bucket_add(&mut b2, "api.anthropic.com", "escape", "main", 1, 2);
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
        bucket_add(&mut b, "x", "direct", "main", 0, 0);
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
            bucket_add(&mut b, "d", "escape", "main", up, up * 10);
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
    fn history_map_multi_span_and_lane() {
        let c = mem();
        let now = 100_000;
        for (ts, dom, lane, up, dn) in [
            (now - 10, "a.com", "escape", 10u64, 1000u64),
            (now - 2000, "a.com", "escape", 5, 500), // in 1h/24h windows, not 60s
            (now - 10, "b.com", "direct", 3, 30),
            (now - 10, "(unattributed)", "-", 99, 99),
        ] {
            let mut b: Bucket = Bucket::new();
            bucket_add(&mut b, dom, lane, "main", up, dn);
            flush_bucket(&c, ts, &b).unwrap();
        }
        let spans = [60i64, 300, 3600, 86_400];
        let m = history_map(&c, now, spans, None, None).unwrap();
        // unattributed excluded; both domains present with dominant lane.
        assert!(!m.contains_key("(unattributed)"));
        let (lane, up, dn) = &m["a.com"];
        assert_eq!(lane, "escape");
        // a.com's 60s column excludes the 2000s-old bucket; 1h/24h include both.
        assert_eq!(dn[0], 1000, "1m col: only the recent bucket");
        assert_eq!(dn[2], 1500, "1h col: both buckets");
        assert_eq!(up[2], 15, "1h up col: both buckets");
        // Lane filter = direct keeps only b.com.
        let only = history_map(&c, now, spans, Some("direct"), None).unwrap();
        assert_eq!(only.len(), 1);
        assert!(only.contains_key("b.com"));
    }

    /// The version-1 shape: keyed (ts, domain, lane), no port column.
    fn v1_store() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        let mut ddl = String::new();
        for t in &TIERS {
            ddl.push_str(&format!(
                "CREATE TABLE {} (ts INTEGER NOT NULL, domain TEXT NOT NULL, lane TEXT NOT NULL,\
                   bytes_up INTEGER NOT NULL DEFAULT 0, bytes_dn INTEGER NOT NULL DEFAULT 0,\
                   PRIMARY KEY (ts, domain, lane)) WITHOUT ROWID;",
                t.table
            ));
        }
        ddl.push_str("CREATE TABLE meta (k TEXT PRIMARY KEY, v TEXT); INSERT INTO meta VALUES('schema_version','1');");
        c.execute_batch(&ddl).unwrap();
        c
    }

    fn count_and_sums(c: &Connection) -> Vec<(i64, i64, i64)> {
        TIERS
            .iter()
            .map(|t| {
                c.query_row(
                    &format!("SELECT count(*), coalesce(sum(bytes_up),0), coalesce(sum(bytes_dn),0) FROM {}", t.table),
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap()
            })
            .collect()
    }

    #[test]
    fn version_1_store_migrates_with_rows_and_bytes_unchanged() {
        let c = v1_store();
        for (i, t) in TIERS.iter().enumerate() {
            let i = i as i64;
            c.execute_batch(&format!(
                "INSERT INTO {} VALUES ({ts}, 'a.com', 'escape', {u}, {d}), ({ts}, 'b.com', 'direct', 5, 6), \
                 ({ts}, '(unattributed)', '-', 1, 2);",
                t.table,
                ts = 1000 + i,
                u = 100 + i,
                d = 200 + i
            ))
            .unwrap();
        }
        let before = count_and_sums(&c);
        init_schema(&c).unwrap();
        assert_eq!(count_and_sums(&c), before, "every tier keeps its rows and bytes");
        for t in &TIERS {
            assert!(has_port(&c, t.table).unwrap(), "{} has the port column", t.table);
            let unknown: i64 = c
                .query_row(&format!("SELECT count(*) FROM {} WHERE port = '-'", t.table), [], |r| r.get(0))
                .unwrap();
            assert_eq!(unknown, 3, "{}: rows from before the port carry '-', not 'main'", t.table);
        }
        assert_eq!(get_meta(&c, "schema_version").as_deref(), Some("2"));
        // No leftover copy, and a second open is a no-op.
        let v1: i64 = c.query_row("SELECT count(*) FROM sqlite_master WHERE name LIKE '%_v1'", [], |r| r.get(0)).unwrap();
        assert_eq!(v1, 0);
        init_schema(&c).unwrap();
        assert_eq!(count_and_sums(&c), before);
        // The migrated store takes port-keyed writes alongside the old rows.
        let mut b: Bucket = Bucket::new();
        bucket_add(&mut b, "a.com", "escape", "vm", 7, 8);
        flush_bucket(&c, 1000, &b).unwrap();
        let rows: i64 = c.query_row("SELECT count(*) FROM sample_5s WHERE ts=1000 AND domain='a.com'", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 2, "the same (ts, domain, lane) holds one row per port");
    }

    #[test]
    fn ports_are_kept_apart_through_flush_and_rollup() {
        let c = mem();
        let now = 10_000_000;
        let m = ((now - 7200) / 60) * 60;
        for (ts, port, up) in [(m, "main", 3u64), (m + 5, "main", 4), (m + 5, "vm", 50)] {
            let mut b: Bucket = Bucket::new();
            bucket_add(&mut b, "d", "escape", port, up, up * 10);
            flush_bucket(&c, ts, &b).unwrap();
        }
        rollup(&c, now).unwrap();
        let got: Vec<(String, i64)> = c
            .prepare("SELECT port, bytes_up FROM sample_1m ORDER BY port")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(got, vec![("main".to_string(), 7), ("vm".to_string(), 50)]);
    }

    #[test]
    fn history_map_port_filter() {
        let c = mem();
        let now = 100_000;
        for (dom, lane, port, dn) in [
            ("a.com", "escape", "main", 1000u64),
            ("a.com", "escape", "vm", 70),
            ("b.com", "direct", "vm", 30),
            ("c.com", "direct", NO_PORT, 9), // recorded before the port column
        ] {
            let mut b: Bucket = Bucket::new();
            bucket_add(&mut b, dom, lane, port, 1, dn);
            flush_bucket(&c, now - 10, &b).unwrap();
        }
        let spans = [60i64, 300, 3600, 86_400];
        let all = history_map(&c, now, spans, None, None).unwrap();
        assert_eq!(all["a.com"].2[0], 1070, "no port filter sums every port");
        assert!(all.contains_key("c.com"), "rows without a port still count toward all");
        let vm = history_map(&c, now, spans, None, Some("vm")).unwrap();
        assert_eq!(vm.len(), 2);
        assert_eq!(vm["a.com"].2[0], 70);
        assert_eq!(vm["b.com"].2[0], 30);
        let vm_direct = history_map(&c, now, spans, Some("direct"), Some("vm")).unwrap();
        assert_eq!(vm_direct.keys().collect::<Vec<_>>(), vec!["b.com"], "lane and port filters compose");
    }

    #[test]
    fn history_map_reads_a_store_still_at_version_1() {
        // A new monitor against the store an old collector still writes.
        let c = v1_store();
        let now = 100_000;
        c.execute("INSERT INTO sample_5s VALUES (?1, 'a.com', 'escape', 10, 1000)", params![now - 10]).unwrap();
        let spans = [60i64, 300, 3600, 86_400];
        let all = history_map(&c, now, spans, None, None).unwrap();
        assert_eq!(all["a.com"].2[0], 1000, "the whole history shows with no port filter");
        let main = history_map(&c, now, spans, None, Some(MAIN_PORT)).unwrap();
        assert!(main.is_empty(), "no row has a known port yet");
        assert!(!has_port(&c, "sample_5s").unwrap(), "reading never migrates");
    }
}
