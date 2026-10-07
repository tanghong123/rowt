//! One server's latency from several delay samples — the figure `rowt ping`
//! prints, by the same rule the monitor's prober uses (rowt-monitor
//! `probe_round`, a separate workspace with its own copy).
//!
//! A single delay test over a lossy cross-border link spikes or fails on its
//! own, so each server is sampled three times back to back and the middle value
//! is the one worth ranking by. The rule for fewer successes is the monitor's:
//! two give their mean (floored, as integer division does), one gives itself,
//! none means the server did not answer at all.
//!
//! `auto` (sing-box's urltest) still switches on sing-box's own stored figure,
//! which every delay test overwrites with its one result — there is no API to
//! store a median. `best` is the mode where rowt chooses instead: the medians go
//! into a table every rowt prober shares, and the watchdog picks from it by
//! sing-box's own rule (keep the current server unless another beats it by more
//! than the tolerance), so one slow or failed sample can no longer cause a switch.
//!
//! The table, `latency.tsv` in the config directory, one row per server:
//!
//! ```text
//! <tag>\t<median ms, empty when no sample answered>\t<samples answered>\t<UTC time>
//! ```
//!
//! The time is `YYYY-MM-DD HH:MM:SS` in UTC: the shell writes it with `date -u`,
//! two such stamps compare chronologically as plain strings, and the parity
//! harness already masks that shape. Rows are byte-sorted by tag under one
//! `#` header line; the file is replaced whole (0600).

/// How many samples one server gets.
pub const SAMPLES: usize = 3;

/// The figure from the samples that succeeded, in any order. `None` when none did.
pub fn aggregate(ok: &[u64]) -> Option<u64> {
    let mut v = ok.to_vec();
    v.sort_unstable();
    match v.as_slice() {
        [] => None,
        [a, b] => Some((a + b) / 2),
        values => Some(values[values.len() / 2]),
    }
}

/// The escape selector's default for a render, and the tag the health probe
/// asks about: the state's `selected` ("auto" when unset), except in `best`
/// mode, where it is the watchdog's last pick — "" before its first, and the
/// render then defaults to the first server. The shell's `_escape_sel`; every
/// renderer and the health probe go through it, or `best` would render as a
/// selector naming no server and be probed as an outbound that does not exist.
pub fn escape_selection(selected: &str, best_pick: &str) -> String {
    match selected {
        "" => "auto".to_string(),
        "best" => best_pick.to_string(),
        s => s.to_string(),
    }
}

/// One server's row in the shared latency table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub tag: String,
    /// `None` when none of the samples answered.
    pub median: Option<u64>,
    pub ok: u64,
    /// `YYYY-MM-DD HH:MM:SS`, UTC.
    pub at: String,
}

pub const TABLE_HEADER: &str =
    "# rowt latency table: tag, median ms (empty = no answer), samples answered, UTC time";

/// The table's rows. Comments, blank and malformed lines are skipped, and a tag
/// seen twice keeps its LAST row — the file is written whole, so a duplicate
/// only comes from a hand edit.
pub fn table_parse(body: &str) -> Vec<Row> {
    let mut m: std::collections::BTreeMap<String, Row> = std::collections::BTreeMap::new();
    for l in body.lines() {
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() != 4 || f[0].is_empty() || f[3].is_empty() {
            continue;
        }
        // Digits only, as the shell's `^[0-9]+$` — `u64::from_str` alone would
        // also take a leading `+`.
        let digits = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
        let median = if f[1].is_empty() {
            None
        } else if digits(f[1]) {
            match f[1].parse::<u64>() { Ok(v) => Some(v), Err(_) => continue }
        } else {
            continue
        };
        if !digits(f[2]) { continue }
        let Ok(ok) = f[2].parse::<u64>() else { continue };
        m.insert(f[0].to_string(), Row { tag: f[0].to_string(), median, ok, at: f[3].to_string() });
    }
    m.into_values().collect()
}

/// The file body: the header, then one line per row, byte-sorted by tag.
pub fn table_render(rows: &[Row]) -> String {
    let mut v: Vec<&Row> = rows.iter().collect();
    v.sort_by(|a, b| a.tag.cmp(&b.tag));
    let mut s = format!("{TABLE_HEADER}\n");
    for r in v {
        let m = r.median.map(|x| x.to_string()).unwrap_or_default();
        s.push_str(&format!("{}\t{}\t{}\t{}\n", r.tag, m, r.ok, r.at));
    }
    s
}

/// The table after a probe: `fresh` replaces those servers' rows, every other
/// row is kept, and a row whose tag is no longer in `pool` is dropped — a server
/// removed from the pool must not linger as a candidate.
pub fn table_merge(existing: &[Row], fresh: &[Row], pool: &[String]) -> Vec<Row> {
    let mut m: std::collections::BTreeMap<String, Row> = std::collections::BTreeMap::new();
    for r in existing.iter().chain(fresh.iter()) {
        if pool.iter().any(|t| t == &r.tag) {
            m.insert(r.tag.clone(), r.clone());
        }
    }
    m.into_values().collect()
}

/// The pool servers whose figure is missing or older than `cutoff` (a UTC
/// stamp in the table's format) — what a prober has to measure again. Pool
/// order is kept.
pub fn stale(pool: &[String], rows: &[Row], cutoff: &str) -> Vec<String> {
    pool.iter()
        .filter(|t| match rows.iter().find(|r| &r.tag == *t) {
            Some(r) => r.at.as_str() < cutoff,
            None => true,
        })
        .cloned()
        .collect()
}

/// `best`'s choice, by sing-box's own urltest rule with medians in place of
/// single samples: keep `current` unless another pool server's median beats it
/// by MORE than `tolerance` ms, or `current` has no median at all (down, not in
/// the table, or not in the pool). Among candidates the lowest median wins,
/// ties broken by tag byte order. `None` when no pool server has a median —
/// then nothing is known to be better, and the caller keeps what it has.
pub fn choose(current: &str, pool: &[String], rows: &[Row], tolerance: u64) -> Option<String> {
    let in_pool = |t: &str| pool.iter().any(|p| p == t);
    let mut cands: Vec<(u64, &str)> = rows.iter()
        .filter(|r| in_pool(&r.tag))
        .filter_map(|r| r.median.map(|m| (m, r.tag.as_str())))
        .collect();
    cands.sort();
    let &(best_ms, best_tag) = cands.first()?;
    let cur = rows.iter().find(|r| r.tag == current && in_pool(current)).and_then(|r| r.median);
    match cur {
        Some(cm) if cm <= best_ms + tolerance => Some(current.to_string()),
        _ => Some(best_tag.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(tag: &str, median: Option<u64>, at: &str) -> Row {
        Row { tag: tag.into(), median, ok: if median.is_some() { 3 } else { 0 }, at: at.into() }
    }
    fn pool(t: &[&str]) -> Vec<String> { t.iter().map(|s| s.to_string()).collect() }

    #[test]
    fn best_selects_its_pick_and_everything_else_passes_through() {
        assert_eq!(escape_selection("best", "beta"), "beta");
        assert_eq!(escape_selection("best", ""), "");
        assert_eq!(escape_selection("", "beta"), "auto");
        assert_eq!(escape_selection("alpha", "beta"), "alpha");
        assert_eq!(escape_selection("auto", "beta"), "auto");
    }

    #[test]
    fn the_table_round_trips_sorted_with_its_header() {
        let rows = vec![row("zeta", Some(90), "2026-10-07 10:00:00"), row("alpha", None, "2026-10-07 10:00:01")];
        let body = table_render(&rows);
        assert_eq!(body, format!("{TABLE_HEADER}\nalpha\t\t0\t2026-10-07 10:00:01\nzeta\t90\t3\t2026-10-07 10:00:00\n"));
        assert_eq!(table_parse(&body), vec![rows[1].clone(), rows[0].clone()]);
    }

    #[test]
    fn a_malformed_table_line_is_skipped_not_fatal() {
        let body = "# x\nshort\tline\nbad\tms\t3\t2026-10-07 10:00:00\nplus\t+5\t1\t2026-10-07 10:00:00\nok\t5\t1\t2026-10-07 10:00:00\n\n";
        assert_eq!(table_parse(body), vec![Row { tag: "ok".into(), median: Some(5), ok: 1, at: "2026-10-07 10:00:00".into() }]);
    }

    #[test]
    fn a_merge_replaces_probed_rows_keeps_the_rest_and_drops_servers_gone_from_the_pool() {
        let old = vec![row("a", Some(100), "t1"), row("b", Some(200), "t1"), row("gone", Some(1), "t1")];
        let fresh = vec![row("b", Some(150), "t2")];
        assert_eq!(table_merge(&old, &fresh, &pool(&["a", "b"])),
                   vec![row("a", Some(100), "t1"), row("b", Some(150), "t2")]);
    }

    #[test]
    fn stale_means_missing_or_older_than_the_cutoff() {
        let rows = vec![row("a", Some(1), "2026-10-07 10:00:00"), row("b", Some(1), "2026-10-07 09:00:00")];
        assert_eq!(stale(&pool(&["a", "b", "c"]), &rows, "2026-10-07 09:30:00"), pool(&["b", "c"]));
    }

    #[test]
    fn the_current_server_is_kept_unless_beaten_by_more_than_the_tolerance() {
        let rows = vec![row("cur", Some(140), "t"), row("fast", Some(90), "t")];
        let p = pool(&["cur", "fast"]);
        assert_eq!(choose("cur", &p, &rows, 50).as_deref(), Some("cur"));   // 140 <= 90+50
        assert_eq!(choose("cur", &p, &rows, 49).as_deref(), Some("fast"));  // 140 >  90+49
    }

    #[test]
    fn a_current_server_with_no_median_is_replaced_by_the_fastest() {
        let rows = vec![row("cur", None, "t"), row("b", Some(300), "t"), row("a", Some(200), "t")];
        assert_eq!(choose("cur", &pool(&["cur", "a", "b"]), &rows, 50).as_deref(), Some("a"));
        // Not in the table, or not in the pool, counts as no median.
        assert_eq!(choose("nowhere", &pool(&["a", "b"]), &rows, 50).as_deref(), Some("a"));
    }

    #[test]
    fn with_nothing_answering_there_is_no_choice() {
        let rows = vec![row("a", None, "t"), row("b", None, "t")];
        assert_eq!(choose("a", &pool(&["a", "b"]), &rows, 50), None);
    }

    #[test]
    fn equal_medians_break_by_tag_and_off_pool_rows_are_ignored() {
        let rows = vec![row("b", Some(100), "t"), row("a", Some(100), "t"), row("x", Some(1), "t")];
        assert_eq!(choose("", &pool(&["a", "b"]), &rows, 50).as_deref(), Some("a"));
    }

    #[test]
    fn three_successes_give_the_middle_value_whatever_their_order() {
        assert_eq!(aggregate(&[300, 100, 200]), Some(200));
        assert_eq!(aggregate(&[100, 100, 900]), Some(100));
    }

    #[test]
    fn two_successes_give_their_mean_floored() {
        assert_eq!(aggregate(&[351, 250]), Some(300));
        assert_eq!(aggregate(&[200, 200]), Some(200));
    }

    #[test]
    fn one_success_is_the_figure_and_none_is_unreachable() {
        assert_eq!(aggregate(&[120]), Some(120));
        assert_eq!(aggregate(&[]), None);
    }
}
