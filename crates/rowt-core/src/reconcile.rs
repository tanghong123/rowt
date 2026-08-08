//! Corp-lane superset reconcile — the port of `config/corp-sync-reconcile.py`.
//!
//! The corp lane only has to *contain* every live tunnel route; a superset is
//! fine, since an over-broad hand-added `11.0.0.0/8` already covers a live
//! `11.122.0.0/15`. Each rewrite costs a sing-box reload, so the rule is: change
//! nothing unless some live route is actually uncovered.

use ipnet::Ipv4Net;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Every live route is already covered — nothing is rewritten, nothing reloads.
    NoChange,
    /// A live route was uncovered; this is the new managed block, sorted.
    Change(Vec<Ipv4Net>),
}

/// Parse one CIDR per line, ignoring blanks, comments, non-IPv4 and junk, and
/// de-duplicating by normalized form — `_load` in the Python.
pub fn load(body: &str) -> Vec<Ipv4Net> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for raw in body.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // `ip_network(..., strict=False)` — host bits are allowed and cleared.
        let Ok(net) = line.parse::<Ipv4Net>() else { continue };
        let net = net.trunc();
        if seen.insert(net.to_string()) {
            out.push(net);
        }
    }
    out
}

fn covered_by(a: &Ipv4Net, pool: &[Ipv4Net]) -> bool {
    pool.iter().any(|n| n.contains(a))
}

fn collapse(nets: &[Ipv4Net]) -> Vec<Ipv4Net> {
    Ipv4Net::aggregate(&nets.to_vec())
}

/// A: live tunnel routes. H: hand-typed corp CIDRs. B: the managed block.
/// P: ranges the router already sends unbound, which never belong in the block.
pub fn reconcile(active: &[Ipv4Net], hand: &[Ipv4Net], block: &[Ipv4Net], private: &[Ipv4Net]) -> Outcome {
    let p = collapse(private);
    let a: Vec<Ipv4Net> = active.iter().filter(|n| !covered_by(n, &p)).copied().collect();
    let b: Vec<Ipv4Net> = block.iter().filter(|n| !covered_by(n, &p)).copied().collect();

    let mut cover_src = hand.to_vec();
    cover_src.extend_from_slice(&b);
    let cover = collapse(&cover_src);
    if a.iter().all(|x| covered_by(x, &cover)) {
        return Outcome::NoChange;
    }

    let hcover = if hand.is_empty() { Vec::new() } else { collapse(hand) };
    // A block CIDR overlapping a live route is dropped WHOLE, never shrunk.
    let keep = b.iter().filter(|c| !a.iter().any(|x| c.contains(&x.network()) || x.contains(&c.network())));
    // Live routes already covered by hand entries are not re-added (minimal).
    let add = a.iter().filter(|x| !covered_by(x, &hcover));

    let mut merged: BTreeMap<(u32, u8), Ipv4Net> = BTreeMap::new();
    for net in keep.chain(add) {
        merged.insert((net.network().into(), net.prefix_len()), *net);
    }
    Outcome::Change(merged.into_values().collect())
}

/// The stdout contract the shell reads: `CHANGE` + the block, or `NOCHANGE`.
pub fn render_outcome(o: &Outcome) -> String {
    match o {
        Outcome::NoChange => "NOCHANGE".to_string(),
        Outcome::Change(nets) => {
            let mut s = String::from("CHANGE");
            for n in nets {
                s.push('\n');
                s.push_str(&n.to_string());
            }
            s
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nets(v: &[&str]) -> Vec<Ipv4Net> {
        v.iter().map(|s| s.parse().unwrap()).collect()
    }

    #[test]
    fn a_broader_hand_entry_already_covers_a_live_route() {
        let o = reconcile(&nets(&["11.122.0.0/15"]), &nets(&["11.0.0.0/8"]), &[], &[]);
        assert_eq!(o, Outcome::NoChange);
    }

    #[test]
    fn an_uncovered_route_forces_a_rewrite() {
        let o = reconcile(&nets(&["30.1.0.0/16"]), &[], &nets(&["12.0.0.0/8"]), &[]);
        assert_eq!(o, Outcome::Change(nets(&["12.0.0.0/8", "30.1.0.0/16"])));
    }

    #[test]
    fn a_colliding_block_entry_is_dropped_whole_not_shrunk() {
        // 30.0.0.0/8 overlaps the live 30.1.0.0/16 and a second route is
        // uncovered, so the block is rewritten and the overlapping entry goes.
        let o = reconcile(
            &nets(&["30.1.0.0/16", "40.0.0.0/16"]),
            &[],
            &nets(&["30.0.0.0/8"]),
            &[],
        );
        assert_eq!(o, Outcome::Change(nets(&["30.1.0.0/16", "40.0.0.0/16"])));
    }

    #[test]
    fn private_ranges_are_never_mirrored_and_are_pruned() {
        let o = reconcile(
            &nets(&["10.1.0.0/16", "30.1.0.0/16"]),
            &[],
            &nets(&["10.9.0.0/16"]),
            &nets(&["10.0.0.0/8"]),
        );
        // the 10/8 route is skipped, the 10/8 block entry pruned, 30/16 added
        assert_eq!(o, Outcome::Change(nets(&["30.1.0.0/16"])));
    }

    #[test]
    fn collapsing_lets_two_halves_cover_their_supernet() {
        // Neither half contains 10.0.0.0/8 alone; collapsed they do.
        let o = reconcile(&nets(&["10.0.0.0/8"]), &nets(&["10.0.0.0/9", "10.128.0.0/9"]), &[], &[]);
        assert_eq!(o, Outcome::NoChange);
    }

    #[test]
    fn junk_and_duplicates_are_ignored_on_load() {
        let l = load("# c\n\n10.0.0.0/8\nnot-a-cidr\n10.0.0.0/8\n::1/128\n");
        assert_eq!(l, nets(&["10.0.0.0/8"]));
    }

    #[test]
    fn host_bits_are_tolerated() {
        assert_eq!(load("10.1.2.3/8\n"), nets(&["10.0.0.0/8"]));
    }
}
