//! `lane_errors` — what failed on a lane, by domain, over a window.
//!
//! The lane logs hold `timestamp<TAB>domain<TAB>reason`, one line per FAILED or
//! refused connection: sing-box runs at `warn`, so successful traffic is never
//! recorded. That is why every empty result says so at length — an empty list
//! means no errors, not no traffic, and the difference is the whole point of the
//! command.
//!
//! Two orderings here are load-bearing and neither is obvious.

/// `awk` category function — first pattern wins, in this order.
pub fn category(reason: &str) -> &'static str {
    let r = reason;
    if r.contains("timeout") {
        "timeout"
    } else if r.contains("reset") {
        "reset"
    } else if r.contains("refused") {
        "refused"
    } else if r.contains("unreachable")
        || r.contains("no route")
        || r.contains("network is")
        || r.contains("no such host")
    {
        "netfail"
    } else if r.contains("lookup") || r.contains("exchange") || r.contains("dns") {
        "dns"
    } else if r.contains("not permitted") {
        "blocked"
    } else {
        "err"
    }
}

/// One output row: total failures for a domain, and its dominant category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub count: u64,
    pub cat: &'static str,
    pub domain: String,
}

impl Row {
    /// `printf "%d\t%s\t%s\n"` — the line `sort -rn` actually sees.
    fn line(&self) -> String {
        format!("{}\t{}\t{}", self.count, self.cat, self.domain)
    }
}

/// Tally a lane log (possibly several concatenated rotations).
///
/// `cutoff` empty means "all". The shell compares `$1 >= cut` in awk with both
/// sides non-numeric, so it is a STRING comparison over `YYYY-MM-DD HH:MM:SS` —
/// which sorts correctly by construction, and is reproduced as one here.
pub fn tally(body: &str, cutoff: &str) -> Vec<Row> {
    use std::collections::BTreeMap;
    let mut totals: BTreeMap<&str, u64> = BTreeMap::new();
    let mut cats: BTreeMap<(&str, &'static str), u64> = BTreeMap::new();
    for line in body.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        // `NF>=2` — a bare line with no domain is not an event.
        if f.len() < 2 {
            continue;
        }
        if !cutoff.is_empty() && f[0] < cutoff {
            continue;
        }
        let dom = f[1];
        *totals.entry(dom).or_default() += 1;
        *cats.entry((dom, category(f.get(2).copied().unwrap_or("")))).or_default() += 1;
    }
    let mut rows: Vec<Row> = totals
        .into_iter()
        .map(|(dom, count)| {
            // The dominant category. When two categories tie for one domain the
            // shell picks whichever `for (k in cc)` reaches first — genuinely
            // unspecified, so there is no behavior to copy. This takes the
            // lexicographically first, which at least makes rowt-rs repeatable;
            // the bash side is listed in PORTING.md §6.7 to fix rather than
            // enshrine. Do NOT build a fixture that hits this: it would compare
            // two implementations against a coin flip.
            let best = cats
                .iter()
                .filter(|((d, _), _)| *d == dom)
                .max_by_key(|((_, c), n)| (**n, std::cmp::Reverse(*c)))
                .map(|((_, c), _)| *c)
                .unwrap_or("err");
            Row { count, cat: best, domain: dom.to_string() }
        })
        .collect();
    // `sort -rn`. awk's `for (d in c)` emits in hash order, so the sort is the
    // ONLY thing making this output deterministic — and for equal counts that
    // means sort's last-resort comparison, which is the whole line, bytewise
    // (LC_ALL=C), reversed along with everything else by -r. Sorting on the
    // count alone would be stable-looking on a fixture with distinct counts and
    // wrong the first time two domains failed the same number of times.
    rows.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| b.line().cmp(&a.line())));
    rows
}

/// `_period_cutoff`'s parse half: `(count, unit)` for `5m`/`2h`/`7d`, `None` for
/// `all` (no cutoff), and an error for anything else — which the shell turns
/// into the usage line and a non-zero exit.
pub fn parse_period(p: &str) -> Result<Option<(u32, char)>, ()> {
    if p == "all" {
        return Ok(None);
    }
    let (n, u) = match p.chars().last() {
        Some(c @ ('m' | 'h' | 'd')) => (&p[..p.len() - 1], c),
        // `${p%[mhd]}` strips nothing, so the digits check below sees the unit
        // too and rejects it — "1x" and "5" both fail, for the same reason.
        _ => (p, ' '),
    };
    if n.is_empty() || !n.chars().all(|c| c.is_ascii_digit()) || u == ' ' {
        return Err(());
    }
    n.parse().map(|v| Some((v, u))).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_are_first_match_wins() {
        assert_eq!(category("dial tcp: i/o timeout"), "timeout");
        assert_eq!(category("read: connection reset by peer"), "reset");
        assert_eq!(category("operation not permitted"), "blocked");
        assert_eq!(category("something odd"), "err");
        // "no such host" is netfail, even though it is a name failure — the
        // shell tests netfail before dns and the order is the behavior.
        assert_eq!(category("lookup x: no such host"), "netfail");
    }

    #[test]
    fn equal_counts_break_by_whole_line_descending() {
        let body = "\
2026-01-01\tb.example\ttimeout
2026-01-01\ta.example\ttimeout
2026-01-01\tc.example\treset
";
        let r = tally(body, "");
        // all counts are 1, so the order is entirely the last-resort compare:
        // "1\ttimeout\tb" > "1\ttimeout\ta" > "1\treset\tc"
        assert_eq!(
            r.iter().map(|x| x.domain.as_str()).collect::<Vec<_>>(),
            ["b.example", "a.example", "c.example"]
        );
    }

    #[test]
    fn the_cutoff_is_a_string_compare_on_the_timestamp() {
        let body = "2026-01-01 10:00:00\ta\ttimeout\n2026-02-01 10:00:00\tb\ttimeout\n";
        assert_eq!(tally(body, "2026-01-15 00:00:00").len(), 1);
        assert_eq!(tally(body, "").len(), 2);
    }

    #[test]
    fn a_line_without_a_domain_is_not_an_event() {
        assert!(tally("2026-01-01\n", "").is_empty());
    }

    #[test]
    fn periods_parse_the_way_the_shell_strips_them() {
        assert_eq!(parse_period("all"), Ok(None));
        assert_eq!(parse_period("10m"), Ok(Some((10, 'm'))));
        assert_eq!(parse_period("7d"), Ok(Some((7, 'd'))));
        assert_eq!(parse_period("5"), Err(()));    // no unit
        assert_eq!(parse_period("1x"), Err(()));   // unknown unit
        assert_eq!(parse_period("nonsense"), Err(()));
    }
}
