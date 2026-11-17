//! Lane membership as data, and the audit lines a change in it produces.
//!
//! `log/audit.log` brackets every mutating command, but a bracket says only
//! that `block import my-blocklist.txt` ran — not which entries it added. On
//! 2026-10-06 tracing why one site's bot-defence host was blocked meant
//! finding a month-old import file on disk and grepping it; had the file been
//! edited or deleted, the record would have been gone. A lane file written
//! outside rowt (by hand, or by an agent writing it directly) left no trace at
//! all.
//!
//! So the bracket now diffs lane membership. The state is every `(lane, entry)`
//! pair across the five lane files; a change is an entry whose SET of lanes
//! moved — `block -> none` when the monitor unroutes a host, `escape -> block`
//! when an add pulls it out of another lane, `none -> block` for each line of
//! an import, `escape -> direct` when `direct add` takes a host off escape. Diffing at the bracket rather than at each
//! writer is what covers every path at once: add, rm, import, clear, `config
//! import`, the monitor's routes, `corp sync`.
//!
//! Entries are kept AS WRITTEN (whitespace stripped, comments dropped) —
//! `domain:x`, `geosite:x` and `*.x` stay in their own spelling, so reversing a
//! line re-adds exactly the text that left. Pure: the caller reads the files.

use std::collections::{BTreeMap, BTreeSet};

/// The lanes whose membership is tracked, and the file each lives in — in the
/// byte order the snapshot sorts them, which is the order a multi-lane set is
/// spelled (`corp+hotspot`).
pub const LANES: [(&str, &str); 5] = [
    ("block", "block-domains.txt"),
    ("corp", "corp-domains.txt"),
    ("direct", "direct-domains.txt"),
    ("escape", "escape-domains.txt"),
    ("hotspot", "hotspot-domains.txt"),
];

/// The name for "in no lane": an unlisted entry takes the final route. Until
/// the direct lane existed (#s98) this was `direct`, what the monitor's key
/// calls an unroute; with a lane of that name, `none -> direct` (a `direct
/// add`) and `escape -> none` (an unroute) have to read differently.
pub const NONE: &str = "none";

/// awk's `[[:space:]]` under LC_ALL=C — the class the shell strips. Not
/// `char::is_whitespace` (which takes Unicode spaces too) and not
/// `is_ascii_whitespace` (which leaves out vertical tab).
fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\x0B' | '\x0C' | '\r')
}

/// One lane file's entries: whitespace removed, blank and `#` lines dropped.
pub fn entries(body: &str) -> Vec<String> {
    body.lines()
        .map(|l| l.chars().filter(|c| !is_space(*c)).collect::<String>())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}

/// A lane template as rowt seeds it into a config that lacks the lane: the
/// template's lines, less each entry another lane already holds (compared with
/// whitespace removed, ASCII case folded). Seeding the direct lane's npm
/// entries into a config whose escape lane holds `npmjs.org` would otherwise
/// leave a twin, which the router settles by lane order and every render warns
/// about (#s98). `others` are the other lane files' bodies. Every line comes
/// out with a newline, as awk prints it.
pub fn seed_body(template: &str, others: &[String]) -> String {
    let fold = |l: &str| l.chars().filter(|c| !is_space(*c)).collect::<String>().to_ascii_lowercase();
    let have: BTreeSet<String> = others.iter().flat_map(|b| entries(b)).map(|e| e.to_ascii_lowercase()).collect();
    let mut out = String::new();
    for line in template.split_inclusive('\n') {
        let line = line.strip_suffix('\n').unwrap_or(line);
        let e = fold(line);
        if !e.is_empty() && !e.starts_with('#') && have.contains(&e) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// The last logged state with a freshly seeded lane's entries added, so the
/// seeding is logged as such (`LANE seeded: none -> direct …`) rather than as
/// an edit made outside rowt at the next command. Only the seeded lane's
/// entries are added: anything else that moved since is still the next
/// bracket's to find.
pub fn with_seeded(prev: &str, lane: &str, body: &str) -> String {
    let mut lines: BTreeSet<String> = prev.split('\n').filter(|l| !l.is_empty()).map(str::to_string).collect();
    for e in entries(body) {
        lines.insert(format!("{lane}\t{e}"));
    }
    lines.into_iter().map(|l| l + "\n").collect()
}

/// The state as `<lane>\t<entry>` lines, byte-sorted and unique — the format
/// persisted in `cache/lane-state.tsv`, and exactly what the shell's
/// `LC_ALL=C sort -u` produces. `read` returns a lane file's body ("" when it
/// does not exist).
pub fn snapshot(read: impl Fn(&str) -> String) -> String {
    let mut lines: BTreeSet<String> = BTreeSet::new();
    for (lane, file) in LANES {
        for e in entries(&read(file)) {
            lines.insert(format!("{lane}\t{e}"));
        }
    }
    lines.into_iter().map(|l| l + "\n").collect()
}

/// entry -> the lanes holding it, from a snapshot. A malformed line is skipped.
fn parse(snap: &str) -> BTreeMap<String, BTreeSet<String>> {
    let mut m: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for l in snap.lines() {
        if let Some((lane, e)) = l.split_once('\t') {
            if !lane.is_empty() && !e.is_empty() {
                m.entry(e.to_string()).or_default().insert(lane.to_string());
            }
        }
    }
    m
}

fn spell(set: Option<&BTreeSet<String>>) -> String {
    match set {
        Some(s) if !s.is_empty() => s.iter().cloned().collect::<Vec<_>>().join("+"),
        _ => NONE.to_string(),
    }
}

/// The audit lines for the move from `before` to `after`: one per transition,
/// `LANE <prefix><from> -> <to> (<n>): <entries…>`, transitions and entries
/// both byte-sorted. Nothing at all when nothing moved — the watchdog calls
/// this every tick, and a no-op line every two minutes would evict the whole
/// history from a log capped by line count.
///
/// The entry list is never truncated: a 300-entry import is one long line,
/// and the full list is what makes the line reversible.
pub fn diff_lines(before: &str, after: &str, prefix: &str) -> Vec<String> {
    let b = parse(before);
    let a = parse(after);
    let keys: BTreeSet<&String> = b.keys().chain(a.keys()).collect();
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for e in keys {
        let (from, to) = (spell(b.get(e)), spell(a.get(e)));
        if from != to {
            groups.entry(format!("{from} -> {to}")).or_default().push(e.clone());
        }
    }
    groups.into_iter()
        .map(|(t, es)| format!("LANE {prefix}{t} ({}): {}", es.len(), es.join(" ")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(files: &[(&str, &str)]) -> String {
        let m: BTreeMap<&str, &str> = files.iter().cloned().collect();
        snapshot(|f| m.get(f).map(|s| s.to_string()).unwrap_or_default())
    }

    #[test]
    fn a_seeded_lane_leaves_out_what_another_lane_holds() {
        let tmpl = "# header\n#\n\n# --- npm ---\nnpmjs.org\nnpmjs.com\nnpm.community\n";
        let others = vec!["NPMjs.org\n# npmjs.com is only a comment here\n".to_string(), "geosite:github\n".to_string()];
        assert_eq!(seed_body(tmpl, &others), "# header\n#\n\n# --- npm ---\nnpmjs.com\nnpm.community\n");
        // Nothing elsewhere: the template as it is; a last line gets its newline.
        assert_eq!(seed_body("a.example\nb.example", &[]), "a.example\nb.example\n");
    }

    #[test]
    fn seeding_is_logged_against_the_last_logged_state() {
        let prev = "escape\tNPMjs.org\nescape\tgithub.com\n";
        let now = with_seeded(prev, "direct", "# h\nnpmjs.com\nnpm.community\n");
        assert_eq!(now, "direct\tnpm.community\ndirect\tnpmjs.com\nescape\tNPMjs.org\nescape\tgithub.com\n");
        assert_eq!(diff_lines(prev, &now, "seeded: "), ["LANE seeded: none -> direct (2): npm.community npmjs.com"]);
    }

    #[test]
    fn entries_are_kept_as_written_and_comments_drop_out() {
        let body = "# header\n\n  a.example  \n\t# indented comment\ndomain:B.example\n*.c.example\ngeosite:x\n# --- rowt corp sync: marker ---\n10.0.0.0/8\n";
        assert_eq!(entries(body), ["a.example", "domain:B.example", "*.c.example", "geosite:x", "10.0.0.0/8"]);
    }

    #[test]
    fn the_snapshot_is_byte_sorted_and_unique() {
        let s = snap(&[("escape-domains.txt", "z.example\nB.example\nz.example\n"),
                       ("block-domains.txt", "y.example\n")]);
        // Byte order, as LC_ALL=C sort: block before escape, `B` before `z`.
        assert_eq!(s, "block\ty.example\nescape\tB.example\nescape\tz.example\n");
    }

    #[test]
    fn nothing_moved_means_no_line() {
        let s = snap(&[("block-domains.txt", "a.example\n")]);
        assert!(diff_lines(&s, &s, "").is_empty());
        assert!(diff_lines("", "", "").is_empty());
    }

    #[test]
    fn an_unroute_names_the_lane_it_left() {
        // The monitor's `d` runs `rm` against every lane; the log has to say
        // which one actually held the host.
        let before = snap(&[("block-domains.txt", "att-api.example\nkeep.example\n")]);
        let after = snap(&[("block-domains.txt", "keep.example\n")]);
        assert_eq!(diff_lines(&before, &after, ""), ["LANE block -> none (1): att-api.example"]);
    }

    #[test]
    fn the_direct_lane_is_a_lane_and_no_lane_is_none() {
        let before = snap(&[("escape-domains.txt", "npmjs.org\n")]);
        let after = snap(&[("direct-domains.txt", "npmjs.org\nnpmjs.com\n")]);
        assert_eq!(diff_lines(&before, &after, ""), [
            "LANE escape -> direct (1): npmjs.org",
            "LANE none -> direct (1): npmjs.com",
        ]);
    }

    #[test]
    fn an_import_is_one_line_listing_every_entry_and_a_pull_out_is_its_own() {
        let before = snap(&[("escape-domains.txt", "moved.example\n")]);
        let after = snap(&[("block-domains.txt", "c.example\na.example\nmoved.example\nb.example\n")]);
        // Byte order of the transition, as the shell's `sort`: escape before none.
        assert_eq!(diff_lines(&before, &after, ""), [
            "LANE escape -> block (1): moved.example",
            "LANE none -> block (3): a.example b.example c.example",
        ]);
    }

    #[test]
    fn an_entry_in_two_lanes_is_spelled_as_its_set() {
        let before = snap(&[("corp-domains.txt", "ts.example\n")]);
        let after = snap(&[("corp-domains.txt", "ts.example\n"), ("hotspot-domains.txt", "ts.example\n")]);
        assert_eq!(diff_lines(&before, &after, "watchdog corp sync: "),
                   ["LANE watchdog corp sync: corp -> corp+hotspot (1): ts.example"]);
    }

    #[test]
    fn a_shorter_transition_sorts_first_as_the_shell_does() {
        // `LC_ALL=C sort` over "<transition>\t<entry>": the tab sorts below every
        // printable byte, so `x -> corp` comes before `x -> corp+hotspot`.
        let before = snap(&[("block-domains.txt", "p.example\nq.example\n")]);
        let after = snap(&[("corp-domains.txt", "p.example\nq.example\n"), ("hotspot-domains.txt", "q.example\n")]);
        assert_eq!(diff_lines(&before, &after, ""), [
            "LANE block -> corp (1): p.example",
            "LANE block -> corp+hotspot (1): q.example",
        ]);
    }

    #[test]
    fn a_malformed_snapshot_line_is_skipped_not_fatal() {
        let before = "garbage\nblock\t\n\tnolane\nblock\tok.example\n";
        assert_eq!(diff_lines(before, "", ""), ["LANE block -> none (1): ok.example"]);
    }
}
