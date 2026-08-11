//! Lane list edits, as a pure transform.
//!
//! Reimplements `edit_list`'s add/rm/clear/import/dump plus `_lane_dedupe` from
//! bin/rowt. The side effects those paths also trigger — fetching a `geosite:`
//! rule-set, the "also covered by" hint, reloading the router — stay in the
//! shell; they are network and process work, not set logic.
//!
//! The invariant this exists to enforce: **a domain lives in exactly one lane**.
//! Adding to one lane pulls it out of the other two.

use crate::classify::Lane;

/// The three editable lane lists, as file contents.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lanes {
    pub escape: String,
    pub corp: String,
    pub block: String,
}

impl Lanes {
    fn get(&self, lane: Lane) -> &str {
        match lane {
            Lane::Escape => &self.escape,
            Lane::Corp => &self.corp,
            Lane::Block => &self.block,
            Lane::Direct => "",
        }
    }
    fn set(&mut self, lane: Lane, v: String) {
        match lane {
            Lane::Escape => self.escape = v,
            Lane::Corp => self.corp = v,
            Lane::Block => self.block = v,
            Lane::Direct => {}
        }
    }
    /// Editable lanes, in the order `_lane_dedupe` walks them.
    fn editable() -> [Lane; 3] {
        [Lane::Escape, Lane::Corp, Lane::Block]
    }
}

#[derive(Debug, Clone)]
pub enum Op {
    Add(Vec<String>),
    Rm(Vec<String>),
    Clear,
    /// Batch add — one entry per line, as `import` reads a file. Carries the
    /// source name because the shell names it in the summary line, and that
    /// summary is the message this has to reproduce.
    Import { lines: Vec<String>, source: String },
}

/// `tr -d '[:space:]'` — every whitespace character goes, not just the ends.
pub fn normalize_entry(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

fn has_line(body: &str, entry: &str) -> bool {
    body.lines().any(|l| l == entry)
}

/// `grep -vxF` — drop every line equal to `entry`, keeping the trailing newline
/// shape the shell produces.
fn drop_line(body: &str, entry: &str) -> String {
    let kept: Vec<&str> = body.lines().filter(|l| *l != entry).collect();
    if kept.is_empty() {
        String::new()
    } else {
        format!("{}\n", kept.join("\n"))
    }
}

/// The first line of an auto-managed block, if the lane has one.
///
/// Matched on the marker's SIGNATURE — "auto-managed" and "do not edit below"
/// on one comment line — rather than on `corp sync`'s two markers by name, so a
/// block introduced into any other lane later is protected by the same rule
/// without touching this code.
///
/// Both halves are required because "auto-managed" alone also appears in prose:
/// a header comment merely *describing* the blocks below would otherwise become
/// the boundary and push every later entry above it. Harmless — anything above
/// the real marker survives — but it moves entries somewhere nobody asked for.
///
/// The escape lane's `# --- imported from Shadowrocket ---` is deliberately NOT
/// such a block: nothing regenerates it, `apply` only ever appends and dedupes.
fn auto_block_start(body: &str) -> Option<usize> {
    body.lines().position(|l| {
        l.trim_start().starts_with('#')
            && l.contains("auto-managed")
            && l.contains("do not edit below")
    })
}

/// Add `entry` to a lane body — ABOVE any auto-managed block.
///
/// Appending at the end is the obvious implementation and it silently loses the
/// entry: `corp sync` rebuilds the corp lane from its marker down on every
/// watchdog tick, keeping only what is above it. The entry is reported as
/// added, is rendered into the router, and then disappears minutes later with
/// nothing in any log to say why. So it goes at the end of the hand-kept region
/// instead — after its last real line, before the blank separating it from the
/// marker.
fn append_line(body: &str, entry: &str) -> String {
    let Some(marker) = auto_block_start(body) else {
        return if body.is_empty() || body.ends_with('\n') {
            format!("{body}{entry}\n")
        } else {
            format!("{body}\n{entry}\n")
        };
    };
    let lines: Vec<&str> = body.lines().collect();
    let mut last = marker;
    while last > 0 && lines[last - 1].trim().is_empty() {
        last -= 1;
    }
    let mut out: Vec<&str> = Vec::with_capacity(lines.len() + 1);
    out.extend_from_slice(&lines[..last]);
    out.push(entry);
    out.extend_from_slice(&lines[last..]);
    let mut s = out.join("\n");
    s.push('\n');
    s
}

/// The result of an edit: the new lane contents plus the lines the shell prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub lanes: Lanes,
    pub messages: Vec<String>,
    /// Lanes the shell rewrites through `mktemp` + `mv` rather than in place —
    /// a removal, either an explicit `rm` or the single-lane pull-out. `mktemp`
    /// creates its file 0600 and `mv` carries that mode onto the lane file, so
    /// those two paths leave the lane 0600 where `add` (append) and `clear`
    /// (truncate in place) leave the mode alone.
    ///
    /// Incidental in the shell, but it is on-disk state, `parity cli-diff`
    /// compares modes, and a lane list is not secret enough to be worth
    /// diverging over. Reproduced, not corrected (PORTING.md §6.7).
    pub tightened: Vec<Lane>,
}

/// Apply one operation to one lane.
pub fn apply(lanes: &Lanes, target: Lane, op: &Op) -> Edit {
    let mut out = lanes.clone();
    let mut msgs = Vec::new();
    let mut tightened: Vec<Lane> = Vec::new();
    macro_rules! tighten {
        ($l:expr) => {
            if !tightened.contains(&$l) {
                tightened.push($l);
            }
        };
    }

    match op {
        Op::Add(entries) | Op::Import { lines: entries, .. } => {
            let importing = matches!(op, Op::Import { .. });
            let mut added = 0usize;
            let mut already = 0usize;
            for raw in entries {
                let e = normalize_entry(raw);
                // import skips comments and blanks; add just skips blanks.
                if e.is_empty() || (importing && e.starts_with('#')) {
                    continue;
                }
                // `geosite:` is escape/block only — corp routes internal
                // domains and CIDRs, and a rule-set cannot express those.
                if !importing && target == Lane::Corp && e.starts_with("geosite:") {
                    msgs.push(format!(
                        "  skipped {e} — geosite: is only for the escape and block lanes"
                    ));
                    continue;
                }
                let body = out.get(target).to_string();
                if has_line(&body, &e) {
                    already += 1;
                    if !importing {
                        msgs.push(format!("  already present: {e}"));
                    }
                } else {
                    out.set(target, append_line(&body, &e));
                    added += 1;
                    if !importing {
                        msgs.push(format!("  added: {e}"));
                    }
                }
                // single-lane invariant
                for lane in Lanes::editable() {
                    if lane == target {
                        continue;
                    }
                    let other = out.get(lane).to_string();
                    if has_line(&other, &e) {
                        out.set(lane, drop_line(&other, &e));
                        tighten!(lane);
                        msgs.push(format!("  moved out of {} lane: {e}", lane.as_str()));
                    }
                }
            }
            if importing {
                let source = match op {
                    Op::Import { source, .. } => source.as_str(),
                    _ => "",
                };
                msgs.push(format!(
                    "  {} += {added} new, {already} already present (from {source})",
                    target.as_str()
                ));
            }
        }
        Op::Rm(entries) => {
            for raw in entries {
                let e = normalize_entry(raw);
                let body = out.get(target).to_string();
                if has_line(&body, &e) {
                    out.set(target, drop_line(&body, &e));
                    tighten!(target);
                    msgs.push(format!("  removed: {e}"));
                } else {
                    msgs.push(format!("  not found: {e}"));
                }
            }
        }
        Op::Clear => {
            // Keep the header: comment and blank lines survive, entries go.
            let kept: Vec<&str> = out
                .get(target)
                .lines()
                .filter(|l| {
                    let t = l.trim_start();
                    t.is_empty() || t.starts_with('#')
                })
                .collect();
            // The shell captures the kept lines with `$(grep …)`, which strips
            // TRAILING newlines, then writes `printf '%s\n'`. So a blank line at
            // the end of the header disappears while blank lines between
            // comments survive — and an all-entries file clears to a single
            // empty line rather than to nothing.
            let body = kept.join("\n");
            out.set(target, format!("{}\n", body.trim_end_matches('\n')));
            msgs.push(format!("  {} list cleared (comments kept)", target.as_str()));
        }
    }
    Edit { lanes: out, messages: msgs, tightened }
}

/// `dump` — the active entries, comments and blanks removed.
pub fn dump(body: &str) -> Vec<String> {
    body.lines()
        .filter(|l| {
            let t = l.trim_start();
            !(t.is_empty() || t.starts_with('#'))
        })
        .map(|s| s.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lanes() -> Lanes {
        Lanes {
            escape: "# hdr\nexample.com\n".into(),
            corp: "# hdr\ncorp.example\n".into(),
            block: "# hdr\nads.example\n".into(),
        }
    }

    #[test]
    fn adding_to_one_lane_pulls_it_from_the_others() {
        let e = apply(&lanes(), Lane::Corp, &Op::Add(vec!["example.com".into()]));
        assert!(e.lanes.corp.contains("example.com"));
        assert!(!e.lanes.escape.contains("example.com"));
        assert!(e.messages.iter().any(|m| m.contains("moved out of escape lane")));
    }

    #[test]
    fn re_adding_is_a_no_op_that_says_so() {
        let e = apply(&lanes(), Lane::Escape, &Op::Add(vec!["example.com".into()]));
        assert_eq!(e.lanes, lanes());
        assert_eq!(e.messages, vec!["  already present: example.com"]);
    }

    #[test]
    fn geosite_is_refused_on_corp_only() {
        let e = apply(&lanes(), Lane::Corp, &Op::Add(vec!["geosite:google".into()]));
        assert_eq!(e.lanes, lanes());
        assert!(e.messages[0].contains("only for the escape and block lanes"));
        let ok = apply(&lanes(), Lane::Escape, &Op::Add(vec!["geosite:google".into()]));
        assert!(ok.lanes.escape.contains("geosite:google"));
    }

    #[test]
    fn removing_what_is_absent_reports_it() {
        let e = apply(&lanes(), Lane::Escape, &Op::Rm(vec!["nope.example".into()]));
        assert_eq!(e.lanes, lanes());
        assert_eq!(e.messages, vec!["  not found: nope.example"]);
    }

    #[test]
    fn clear_keeps_the_header() {
        let e = apply(&lanes(), Lane::Escape, &Op::Clear);
        assert_eq!(e.lanes.escape, "# hdr\n");
    }

    #[test]
    fn clear_drops_a_trailing_blank_but_keeps_an_inner_one() {
        // `$(grep …)` strips trailing newlines; blanks between comments survive.
        let l = Lanes {
            escape: "# a\n\n# b\nx.example\n\n".into(),
            ..Default::default()
        };
        assert_eq!(apply(&l, Lane::Escape, &Op::Clear).lanes.escape, "# a\n\n# b\n");
    }

    #[test]
    fn clearing_a_headerless_list_leaves_one_blank_line() {
        // Matches `printf '%s\n' "$kept"` with an empty $kept — a quirk, but the
        // file contents are what the parity gate compares.
        let l = Lanes { escape: "a.example\n".into(), ..Default::default() };
        assert_eq!(apply(&l, Lane::Escape, &Op::Clear).lanes.escape, "\n");
    }

    #[test]
    fn whitespace_inside_an_entry_is_removed_not_just_trimmed() {
        assert_eq!(normalize_entry("  a b.example  "), "ab.example");
    }

    #[test]
    fn import_counts_rather_than_narrating() {
        let src = vec!["# c".into(), "".into(), "new.example".into(), "example.com".into()];
        let e = apply(&lanes(), Lane::Escape, &Op::Import { lines: src, source: "list.txt".into() });
        assert_eq!(e.messages, vec!["  escape += 1 new, 1 already present (from list.txt)"]);
        assert!(e.lanes.escape.contains("new.example"));
    }

    #[test]
    fn dump_drops_comments_and_blanks() {
        assert_eq!(dump("# h\n\na.example\n"), vec!["a.example"]);
    }

    const SYNCED: &str = "# hand\nalibaba-inc.com\n\n# --- rowt corp sync: DHCP search domains (auto-managed, do not edit below) ---\nhz.ali.com\n\n# --- rowt corp sync (auto-managed; superset of live tunnel routes — do not edit below) ---\n47.110.90.146/32\n";

    #[test]
    fn an_entry_goes_above_the_auto_managed_block_not_at_the_end() {
        // The bug this replaced was silent and cost three hand-added corp
        // entries: `corp sync` rebuilds from its marker down on every watchdog
        // tick, so an appended entry is reported added, routed, and then gone.
        let got = append_line(SYNCED, "dev.g.alicdn.com");
        let lines: Vec<&str> = got.lines().collect();
        let entry = lines.iter().position(|l| *l == "dev.g.alicdn.com").unwrap();
        let marker = lines.iter().position(|l| l.contains("auto-managed")).unwrap();
        assert!(entry < marker, "entry landed inside the regenerated region:\n{got}");
        // It joins the hand region's last real line, keeping the blank that
        // separates that region from the marker.
        assert_eq!(lines[entry - 1], "alibaba-inc.com");
        assert_eq!(lines[entry + 1], "");
        // Nothing else moved.
        assert_eq!(lines.iter().filter(|l| l.contains("auto-managed")).count(), 2);
        assert!(got.contains("hz.ali.com") && got.contains("47.110.90.146/32"));
    }

    #[test]
    fn a_lane_with_no_auto_block_still_appends_at_the_end() {
        // escape and block have no regenerating writer, so nothing changes for
        // them — including the no-trailing-newline case.
        assert_eq!(append_line("a.com\n", "b.com"), "a.com\nb.com\n");
        assert_eq!(append_line("a.com", "b.com"), "a.com\nb.com\n");
        assert_eq!(append_line("", "b.com"), "b.com\n");
    }

    #[test]
    fn an_imported_marker_is_not_an_auto_managed_block() {
        // `server import --apply` writes this into the escape lane, and it only
        // ever appends + dedupes — treating it as regenerated would put every
        // later entry above someone's imported domains for no reason.
        let body = "a.com\n# --- imported ---\nb.com\n";
        assert_eq!(append_line(body, "c.com"), "a.com\n# --- imported ---\nb.com\nc.com\n");
    }

    #[test]
    fn a_marker_on_the_very_first_line_puts_the_entry_above_it() {
        // The FULL marker text — the signature needs both halves, so an
        // abbreviated one is correctly not treated as a boundary.
        let body = "# --- rowt corp sync (auto-managed, do not edit below) ---\n10.0.0.0/8\n";
        let got = append_line(body, "x.example");
        assert!(got.starts_with("x.example\n"), "{got}");
    }

    #[test]
    fn prose_mentioning_auto_managed_is_not_a_marker() {
        // A header comment DESCRIBING the blocks below is not the boundary —
        // the real markers also say "do not edit below". Caught by a fixture
        // whose own header said "auto-managed" and pushed the entry to line 3.
        let body = "# two auto-managed blocks follow\na.com\n\n# --- rowt corp sync (auto-managed, do not edit below) ---\n10.0.0.0/8\n";
        let got = append_line(body, "b.com");
        let lines: Vec<&str> = got.lines().collect();
        let entry = lines.iter().position(|l| *l == "b.com").unwrap();
        // Below the prose, above the real marker.
        assert_eq!(lines[entry - 1], "a.com");
        assert!(entry < lines.iter().position(|l| l.contains("do not edit below")).unwrap());
    }
}
