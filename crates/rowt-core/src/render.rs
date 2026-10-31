//! The render, as pure functions.
//!
//! This is a reimplementation of the jq program in `bin/rowt` (`assemble_host`,
//! `assemble_vm`, `group_jq`, `list_json`). It is held to that original by
//! canonical-JSON equality — see PORTING.md §6.3 and `tests/parity`.
//!
//! Nothing here touches the network, the clock, or the OS. The two values that
//! do — the physical interface and the clash secret — are inputs, so the
//! platform seam stays in the shell until Phase 5.

use serde_json::{json, Map, Value};

/// One lane list, already parsed. Mirrors `list_json`'s output.
#[derive(Debug, Default, Clone)]
pub struct Lists {
    pub escape_domains: Vec<String>,
    /// Every corp suffix, the synced DHCP domains included: they rank by depth
    /// like a hand entry (the bands below), as they did inline before PR #5.
    pub corp_domains: Vec<String>,
    pub corp_cidrs: Vec<String>,
    pub block_domains: Vec<String>,
    // `--domain` entries (`domain:<host>` lines), rendered as sing-box `domain`
    // rules ahead of every suffix rule.
    pub escape_exact: Vec<String>,
    pub corp_exact: Vec<String>,
    pub block_exact: Vec<String>,
}

/// Cached `geosite:<name>` rule-sets, split the way the render uses them.
#[derive(Debug, Default, Clone)]
pub struct Geo {
    /// Categories named by the escape list that are present in the cache.
    pub escape: Vec<String>,
    /// Categories named by the block list that are present in the cache.
    pub block: Vec<String>,
    /// Every cached category referenced by any lane, as (tag, path).
    pub sets: Vec<(String, String)>,
    /// Path to the ad/tracker set, or empty when it is not cached.
    pub ads_path: String,
}

/// Everything the host render needs. Field-for-field the jq program's `--arg`s.
#[derive(Debug, Clone)]
pub struct HostInput {
    pub escapes: Value,
    pub listen: String,
    pub port: u64,
    pub iface: String,
    pub clash: String,
    pub secret: String,
    pub log_level: String,
    pub final_route: String,
    pub dns_direct: String,
    pub private_default: String,
    pub private_cidrs: Vec<String>,
    pub lists: Lists,
    pub geo: Geo,
    /// Where the escape lane's rules point. "escape" normally; "direct" in
    /// local mode, where no tunnel exists. The rules are still emitted either
    /// way — they carry precedence over broader block entries and the ad set.
    pub escape_outbound: String,
    /// The hosts file the guarded escape members resolve through
    /// (`escapedns`); the `dns-escape` server is rendered only when a member
    /// names it.
    pub escape_hosts: String,
    /// Named extra ports (`ports.tsv`, in `ports::rows` order). Empty renders
    /// byte-identically to a config from before they existed.
    pub ports: Vec<crate::ports::Port>,
    /// The directory the lane rule-sets live in (`RULE_SETS_DIR` under the
    /// config dir), spelled the way host.json names each set's file.
    pub sets_dir: String,
}

/// Where the lane rules live, and why.
///
/// sing-box builds host.json's route rules once, at start — but a LOCAL
/// rule-set is a file it watches and swaps in place when the file is replaced.
/// So every lane rule is a rule-set under `<config dir>/rule-sets/`, host.json
/// only points at them, and a lane edit leaves host.json byte-identical: the
/// running router picks the change up without a restart (`lanes_apply` in
/// rowt-cli's lifecycle; `_lanes_apply` in bin/rowt). Before this, every
/// `escape add` was a render plus `router restart`, which reset every
/// connection on the machine.
///
/// Precedence is the catch. Inline, the render sorted every suffix of the three
/// lanes together, most specific first — jq's `split(".")|length`, then string
/// length — and one rule-set rule is ONE position in that order. The order is
/// kept by splitting each lane into BANDS by split count: for a host and a
/// label count there is exactly one candidate suffix (its last N labels;
/// matching is on a label boundary), so two entries of one band can both match
/// a host only when they are the same entry, and the single-lane rule keeps an
/// entry to one lane. A dot-led `.z.example` (the names UNDER z.example) splits
/// to 3 like the bare `x.z.example`, and both can match one host; the bare one
/// is always the longer string, so inline it always won — hence a bare set AND
/// a dot set per band, bare first. Exact (`domain:`) entries are whole-host
/// equality, one match per host at most: one set per lane, ahead of every
/// suffix, in lane order, as inline. Band 8 holds eight labels or more — the
/// set count is fixed, or the first deeper entry would change host.json — so
/// within it the length tie-break is lost, and the render warns on the one
/// pair that can get wrong (`overflow_pairs`).
///
/// Each set has a directory of its own, `rule-sets/<tag>/<tag>.json`: sing-box
/// watches a local set's DIRECTORY, and on macOS the watcher (kqueue) holds a
/// descriptor for every file in it — 49 sets in one directory cost 49² of them
/// (measured on 1.13.14: 2,550 open files on the directory; one directory per
/// set: 100).
pub const RULE_SETS_DIR: &str = "rule-sets";
/// The corp lane's CIDRs, hand-added and synced alike — one set, since they
/// were one rule (the `ip_cidr` set contributor PR #5 introduced, kept).
pub const CORP_SET_CIDR: &str = "corp-cidr";
/// The deepest band; it holds that many labels or more.
pub const BANDS: usize = 8;
/// The lanes whose rules are rule-sets, in the order their rules are emitted
/// within a band. That order decides only a tie the single-lane rule already
/// forbids, and it matches `classify`'s tie convention.
pub const SET_LANES: [&str; 3] = ["escape", "corp", "block"];

/// One lane set: the `domain` entries, or the `domain_suffix` entries of one
/// split count, bare (`z.example`: the name and what is under it) or dot-led
/// (`.z.example`: only what is under it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    Exact,
    Bare(usize),
    Dot(usize),
}

impl Band {
    /// The set's tag; its file is `<tag>.json` under `RULE_SETS_DIR`.
    pub fn tag(self, lane: &str) -> String {
        match self {
            Band::Exact => format!("{lane}-exact"),
            Band::Bare(n) => format!("{lane}-{n}"),
            Band::Dot(n) => format!("{lane}-{n}-dot"),
        }
    }

    /// Which set a suffix entry goes to: jq's `split(".")|length`, capped at
    /// `BANDS`, the dot-led entries apart. (A dot-led entry splits to two at
    /// least, so `Dot(1)` never occurs.)
    pub fn of(suffix: &str) -> Band {
        let n = suffix.split('.').count().min(BANDS);
        if suffix.starts_with('.') { Band::Dot(n) } else { Band::Bare(n) }
    }
}

/// Every (lane, band) in route order: the exact sets in lane order, then for
/// each band from the deepest down, the bare sets in lane order, then the dot
/// sets. 48 in all — `3 + 3×8 + 3×7` — and always all present, empty or not.
pub fn lane_bands() -> Vec<(&'static str, Band)> {
    let mut out = Vec::with_capacity(48);
    out.extend(SET_LANES.iter().map(|l| (*l, Band::Exact)));
    for n in (1..=BANDS).rev() {
        out.extend(SET_LANES.iter().map(|l| (*l, Band::Bare(n))));
        if n >= 2 {
            out.extend(SET_LANES.iter().map(|l| (*l, Band::Dot(n))));
        }
    }
    out
}

/// One rule-set file, byte for byte what the shell's jq writes:
/// `{"version":1,"rules":[{"<key>":[…]}]}` and a newline, with no rule at all
/// when the list is empty (an empty list would match nothing anyway, but
/// sing-box refuses a rule with no condition).
pub fn rule_set_json(key: &str, values: &[String]) -> String {
    let rules = if values.is_empty() {
        "[]".to_string()
    } else {
        format!("[{{{}:{}}}]", Value::from(key), Value::from(values.to_vec()))
    };
    format!("{{\"version\":1,\"rules\":{rules}}}\n")
}

/// The 48 lane sets, `(tag, bytes)`, in `lane_bands` order. Entries keep the
/// lane file's order and are ASCII-lowercased: sing-box lowercases the name it
/// matches but not the rule, so a mixed-case entry matched nothing inline
/// (measured on 1.13.14 with `rule-set match`), and lowercased, an entry spelled
/// two ways in two lanes is the one entry the single-lane rule meant it to be.
pub fn lane_sets(lists: &Lists) -> Vec<(String, String)> {
    let lower = |v: &[String]| -> Vec<String> { v.iter().map(|s| s.to_ascii_lowercase()).collect() };
    lane_bands()
        .into_iter()
        .map(|(lane, band)| {
            let (suffixes, exacts) = lane_lists(lists, lane);
            let body = match band {
                Band::Exact => rule_set_json("domain", &lower(exacts)),
                b => {
                    let in_band: Vec<String> = lower(suffixes).into_iter().filter(|s| Band::of(s) == b).collect();
                    rule_set_json("domain_suffix", &in_band)
                }
            };
            (band.tag(lane), body)
        })
        .collect()
}

/// A lane's suffix and exact lists.
fn lane_lists<'a>(lists: &'a Lists, lane: &str) -> (&'a [String], &'a [String]) {
    match lane {
        "escape" => (&lists.escape_domains, &lists.escape_exact),
        "corp" => (&lists.corp_domains, &lists.corp_exact),
        _ => (&lists.block_domains, &lists.block_exact),
    }
}

/// The pairs band 8 ranks by lane order where inline ranked by length: an
/// entry of more than `BANDS` labels in one lane, and a suffix of it with
/// `BANDS` labels or more in another. Each is `(deeper entry, its lane, the
/// shallower suffix, its lane)`; the render warns on them. Compared lowercased,
/// as the sets hold them.
pub fn overflow_pairs(lists: &Lists) -> Vec<(String, &'static str, String, &'static str)> {
    let deep: Vec<(String, &str)> = SET_LANES
        .iter()
        .flat_map(|l| lane_lists(lists, l).0.iter().map(move |s| (s.to_ascii_lowercase(), *l)))
        .filter(|(s, _)| s.split('.').count() >= BANDS)
        .collect();
    let mut out = Vec::new();
    for (a, la) in &deep {
        for (b, lb) in &deep {
            if la == lb || a.split('.').count() <= BANDS || a == b {
                continue;
            }
            // b is a label-suffix of a: the bare form under a boundary, or the
            // dot-led form, which is strictly under its name.
            let under = if let Some(bare) = b.strip_prefix('.') {
                a.ends_with(b) && a.len() > b.len() || a == bare
            } else {
                a.ends_with(&format!(".{b}"))
            };
            if under {
                out.push((a.clone(), *la, b.clone(), *lb));
            }
        }
    }
    out
}

/// Two lanes holding the same entry in different spellings of case: lowercased
/// they are one entry in two sets, and lane order decides. `add` keeps this from
/// happening; a hand-edited file can still do it. `(entry, first lane, second lane)`.
pub fn case_twins(lists: &Lists) -> Vec<(String, &'static str, &'static str)> {
    let mut out = Vec::new();
    for (i, la) in SET_LANES.iter().enumerate() {
        for lb in &SET_LANES[i + 1..] {
            let (sa, ea) = lane_lists(lists, la);
            let (sb, eb) = lane_lists(lists, lb);
            for (xa, xb) in [(sa, sb), (ea, eb)] {
                for e in xa {
                    if xb.iter().any(|f| f.eq_ignore_ascii_case(e)) {
                        out.push((e.to_ascii_lowercase(), *la, *lb));
                    }
                }
            }
        }
    }
    out
}

/// Parse one lane list exactly as `list_json` does.
///
/// The order matters and is not obvious: comment/blank lines are dropped on the
/// RAW line, then *all* whitespace is removed from what survives (not merely
/// trimmed — an inner space would close up too), then a `*.z.com` becomes
/// `.z.com` (`unwildcard`), then `geosite:` meta lines go, then the CIDR/domain
/// filter runs, and finally empties are dropped.
pub fn parse_list(contents: &str, filter: Filter) -> Vec<String> {
    contents
        .lines()
        .filter(|raw| {
            let t = raw.trim_start_matches([' ', '\t']);
            !(t.is_empty() || t.starts_with('#'))
        })
        .map(|raw| raw.chars().filter(|c| !c.is_whitespace()).collect::<String>())
        .map(|s| unwildcard(&s))
        .filter(|s| !s.starts_with("geosite:"))
        .filter_map(|s| {
            let exact = s.strip_prefix(EXACT_PREFIX).map(str::to_string);
            match filter {
                Filter::Exact => exact,
                // A `domain:` line is an EXACT rule, never a suffix or a CIDR.
                // Every other filter MUST drop it: they select by "is / isn't a
                // CIDR", a test `domain:z.com` passes, so without this it would
                // render as a `domain_suffix` — silently becoming the leaky
                // suffix the flag exists to avoid.
                _ if exact.is_some() => None,
                Filter::All => Some(s),
                Filter::Cidr => is_cidr(&s).then_some(s),
                Filter::Domain => (!is_cidr(&s)).then_some(s),
            }
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// Marks a lane entry as an EXACT host rule (sing-box `domain`) rather than the
/// default `domain_suffix`. Same shape as `geosite:` — a hostname can't contain
/// a colon, so the prefix is unambiguous, and both readers already split on one.
pub const EXACT_PREFIX: &str = "domain:";

/// `*.z.com` → `.z.com`: the wildcard spelling of a dot-led entry.
///
/// Shadowrocket and Surge write "the names under z.com" as `*.z.com`, and so
/// did rowt's own onboard and README (`corp add '*.corp.example.com'`). sing-box's
/// `domain_suffix` has no wildcard, though, so a stored `*.z.com` was a literal
/// `*` that no name contains: the entry matched nothing, silently. rowt's own
/// spelling of the same thing is `.z.com` (sing-box: the subdomains, not the
/// apex), so the wildcard is read as the dot — by every reader of a lane list,
/// which is what makes an entry already on disk work without an edit, and by
/// `add`/`import`, which store the dot.
///
/// Only that shape: `*.` alone, and a `*` anywhere else, pass as written.
pub fn unwildcard(e: &str) -> String {
    match e.strip_prefix("*.") {
        Some(rest) if !rest.is_empty() => format!(".{rest}"),
        _ => e.to_string(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    All,
    Cidr,
    Domain,
    /// Only `domain:<host>` lines, with the prefix stripped.
    Exact,
}

/// `grep -E '/[0-9]+$'` — a trailing slash-and-digits marks a CIDR.
pub(crate) fn is_cidr(s: &str) -> bool {
    match s.rsplit_once('/') {
        Some((_, suffix)) => !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()),
        None => false,
    }
}

/// The `geosite:<name>` categories named by a list, in file order with
/// duplicates kept — `geosites_of` does no dedupe, and neither does the per-lane
/// rule it feeds.
pub fn geosites_of(contents: &str) -> Vec<String> {
    contents
        .lines()
        .filter_map(|raw| {
            let t = raw.trim_start_matches([' ', '\t']);
            let rest = t.strip_prefix("geosite:")?;
            let rest = rest.trim_start_matches([' ', '\t']);
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '_' || *c == '-')
                .collect();
            if name.is_empty() {
                None
            } else {
                Some(name)
            }
        })
        .collect()
}

/// How often `auto`'s urltest re-tests every server: `ROWT_AUTO_INTERVAL` when it
/// is set, else what `use auto --every` saved in state, else 20m — the shell's
/// `_auto_interval`. Read at every render, so a reload keeps the saved value.
pub fn auto_interval(env: Option<&str>, saved: &str) -> String {
    match env.filter(|v| !v.is_empty()) {
        Some(v) => v.to_string(),
        None if !saved.is_empty() => saved.to_string(),
        None => "20m".to_string(),
    }
}

/// The urltest's settings: how often a round runs, how many ms faster another
/// server must test before a round moves the pick (`ROWT_AUTO_TOLERANCE`,
/// default 100), and whether a switch also closes the connections left on the
/// old pick (`ROWT_AUTO_INTERRUPT=1`, opt-in). The last two came from PR #4.
#[derive(Clone, Debug, PartialEq)]
pub struct UrlTest {
    pub interval: String,
    pub tolerance: u64,
    pub interrupt: bool,
}

impl UrlTest {
    /// From the environment's raw values and the saved `--every`, as the shell
    /// reads them. A tolerance that is not whole milliseconds renders as the
    /// default; the render refuses it first (`auto_tolerance`).
    pub fn new(interval: String, tolerance: Option<&str>, interrupt: Option<&str>) -> Self {
        UrlTest {
            interval,
            tolerance: auto_tolerance(tolerance).unwrap_or(100),
            interrupt: interrupt == Some("1"),
        }
    }
}

/// `ROWT_AUTO_TOLERANCE` as the shell's `_auto_tolerance` reads it: unset or
/// empty is 100; one to five digits is that many ms; anything else is `None`.
pub fn auto_tolerance(env: Option<&str>) -> Option<u64> {
    match env.filter(|v| !v.is_empty()) {
        None => Some(100),
        Some(v) if v.len() <= 5 && v.bytes().all(|b| b.is_ascii_digit()) => v.parse().ok(),
        Some(_) => None,
    }
}

/// Seconds in a `use auto --every` duration: whole numbers with `s`, `m` or `h`,
/// run together the way Go writes them (`90s`, `3m`, `1m30s`, `1h`). `None` for
/// anything else — the shell's `_duration_secs` prints nothing for the same.
pub fn duration_secs(s: &str) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    let (mut total, mut n, mut digits) = (0u64, 0u64, false);
    for c in s.chars() {
        match c {
            '0'..='9' => {
                n = n.checked_mul(10)?.checked_add(c as u64 - '0' as u64)?;
                digits = true;
            }
            's' | 'm' | 'h' if digits => {
                let unit = match c { 'h' => 3600, 'm' => 60, _ => 1 };
                total = total.checked_add(n.checked_mul(unit)?)?;
                n = 0;
                digits = false;
            }
            _ => return None,
        }
    }
    if digits { None } else { Some(total) }
}

/// The escape outbound group: members (optionally interface-bound), then either
/// a urltest + selector (`auto`) or a plain selector pinned to one server.
pub fn group(servers: &[Value], iface: &str, selected: &str, auto: &UrlTest) -> Value {
    let members: Vec<Value> = servers
        .iter()
        .map(|s| {
            let mut m = s.clone();
            if !iface.is_empty() {
                if let Some(obj) = m.as_object_mut() {
                    obj.insert("bind_interface".into(), json!(iface));
                }
            }
            m
        })
        .collect();
    let tags: Vec<Value> = members.iter().map(|m| m["tag"].clone()).collect();

    let mut out = members.clone();
    if selected == "auto" {
        let mut urltest = json!({
            "type": "urltest", "tag": "auto", "outbounds": tags,
            "url": "https://www.gstatic.com/generate_204", "interval": auto.interval,
            "tolerance": auto.tolerance
        });
        if auto.interrupt {
            urltest["interrupt_exist_connections"] = json!(true);
        }
        out.push(urltest);
        let mut sel_outbounds = vec![json!("auto")];
        sel_outbounds.extend(tags.iter().cloned());
        out.push(json!({
            "type": "selector", "tag": "escape",
            "outbounds": sel_outbounds, "default": "auto"
        }));
    } else {
        // jq: `if ($tags | index($sel)) then $sel else ($tags[0] // "block")`
        let default = if tags.iter().any(|t| t == selected) {
            json!(selected)
        } else {
            tags.first().cloned().unwrap_or(json!("block"))
        };
        out.push(json!({
            "type": "selector", "tag": "escape",
            "outbounds": tags, "default": default
        }));
    }
    Value::Array(out)
}

/// Where a lane's rules point: the escape lane at `escape_outbound` ("direct"
/// in local mode, where no tunnel exists — the rules are still emitted, since
/// they carry precedence over broader block entries and the ad set).
fn lane_outbound(lane: &str, escape_outbound: &str) -> String {
    match lane {
        "escape" => escape_outbound.to_string(),
        other => other.to_string(),
    }
}

/// The lane rules: one rule-set rule per (lane, band), in `lane_bands` order —
/// first match wins, so that order IS the precedence (see `RULE_SETS_DIR`).
fn lane_rules(escape_outbound: &str) -> Vec<Value> {
    lane_bands()
        .into_iter()
        .map(|(lane, band)| json!({"rule_set": [band.tag(lane)], "outbound": lane_outbound(lane, escape_outbound)}))
        .collect()
}

/// The host configuration — what lands in `host.json`.
pub fn render_host(i: &HostInput) -> Value {
    let mut rules: Vec<Value> = vec![json!({"action": "sniff"})];
    rules.extend(lane_rules(&i.escape_outbound));
    rules.extend(
        i.geo.escape.iter()
            .map(|n| json!({"rule_set": [format!("geosite-{n}")], "outbound": i.escape_outbound})),
    );
    rules.extend(
        i.geo.block.iter()
            .map(|n| json!({"rule_set": [format!("geosite-{n}")], "outbound": "block"})),
    );
    // The ad set sits before the address rules, and a name no rule matched is
    // resolved (only when the final route is direct) so the address rules can
    // route it — the shell's renderer explains why, at length.
    if !i.geo.ads_path.is_empty() {
        rules.push(json!({"rule_set": ["geosite-ads"], "outbound": "block"}));
    }
    let address_rules = !i.lists.corp_cidrs.is_empty() || i.private_default == "corp";
    if i.final_route == "direct" && address_rules {
        rules.push(json!({"action": "resolve", "server": "dns-direct", "strategy": "prefer_ipv4"}));
    }
    // Always there: the set carries the CIDRs, none or many, so a corp sync that
    // changes them leaves this file alone. (The resolve rule above still keys on
    // whether there are any — the one shape change a corp sync can cause.)
    rules.push(json!({"rule_set": [CORP_SET_CIDR], "outbound": "corp"}));
    if i.private_default == "corp" {
        rules.push(json!({"ip_cidr": i.private_cidrs, "outbound": "corp"}));
    }
    let rules = port_rules(rules, i);

    let mut dns_servers = json!([
        {"type": "local", "tag": "local"},
        {"type": "https", "tag": "dns-direct", "server": i.dns_direct, "detour": "direct"}
    ]);
    if crate::escapedns::uses_guard(&i.escapes) {
        if let Some(a) = dns_servers.as_array_mut() {
            a.push(crate::escapedns::hosts_server(&i.escape_hosts));
        }
    }
    // Corp names must resolve through the corp resolver or they answer with a
    // public IP and leave the tunnel — so every corp set, exact and suffix,
    // names the local server: one rule over the lane's sixteen sets (a list of
    // sets matches on any of them), always present since the sets always are.
    let corp_sets: Vec<String> =
        lane_bands().into_iter().filter(|(l, _)| *l == "corp").map(|(l, b)| b.tag(l)).collect();
    let dns_rules = vec![json!({"rule_set": corp_sets, "server": "local"})];

    let mut outbounds = i.escapes.as_array().cloned().unwrap_or_default();
    outbounds.push(json!({
        "type": "direct", "tag": "corp",
        "domain_resolver": {"server": "local", "strategy": "prefer_ipv4"}
    }));
    outbounds.push(json!({"type": "direct", "tag": "direct", "bind_interface": i.iface}));
    outbounds.push(json!({"type": "block", "tag": "block"}));

    let mut route = Map::new();
    route.insert("rules".into(), json!(rules));
    route.insert("final".into(), json!(i.final_route));
    route.insert("auto_detect_interface".into(), json!(false));
    route.insert(
        "default_domain_resolver".into(),
        json!({"server": "dns-direct", "strategy": "prefer_ipv4"}),
    );
    let mut allsets: Vec<Value> = i
        .geo
        .sets
        .iter()
        .map(|(tag, path)| json!({"type": "local", "tag": tag, "format": "binary", "path": path}))
        .collect();
    if !i.geo.ads_path.is_empty() {
        allsets.push(json!({
            "type": "local", "tag": "geosite-ads",
            "format": "binary", "path": i.geo.ads_path
        }));
    }
    // The lane sets in route order, then the corp CIDRs — every one a `source`
    // (JSON) set under `sets_dir`, written by the render (`lanes_write`).
    let set_path = |tag: &str| format!("{}/{tag}/{tag}.json", i.sets_dir);
    allsets.extend(lane_bands().into_iter().map(|(l, b)| {
        let tag = b.tag(l);
        json!({"type": "local", "tag": tag, "format": "source", "path": set_path(&tag)})
    }));
    allsets.push(json!({"type": "local", "tag": CORP_SET_CIDR, "format": "source", "path": set_path(CORP_SET_CIDR)}));
    route.insert("rule_set".into(), json!(allsets));

    let mut root = Map::new();
    root.insert("log".into(), json!({"level": i.log_level, "timestamp": true}));
    root.insert(
        "dns".into(),
        json!({"servers": dns_servers, "rules": dns_rules, "final": "dns-direct"}),
    );
    // A named port is loopback-only, like "in"; a VM reaches it through its
    // host-side forward, never directly.
    let mut inbounds = vec![json!({"type": "mixed", "tag": "in", "listen": i.listen, "listen_port": i.port})];
    inbounds.extend(i.ports.iter().map(|p| {
        json!({"type": "mixed", "tag": format!("{}-in", p.name), "listen": "127.0.0.1", "listen_port": p.listen_port()})
    }));
    root.insert("inbounds".into(), json!(inbounds));
    root.insert("outbounds".into(), json!(outbounds));
    root.insert("route".into(), Value::Object(route));
    if !i.clash.is_empty() {
        root.insert(
            "experimental".into(),
            json!({"clash_api": {"external_controller": i.clash, "secret": i.secret}}),
        );
    }
    Value::Object(root)
}

/// Main's rules with the named ports' rules inserted — pure insertions, so
/// main's rules are the same list, in the same order, with or without ports.
///
/// Every port shares every rule and differs only in what none matched. A port
/// whose catch-all is escape hands an unmatched NAME to the far end, so its
/// catch-all must come BEFORE the resolve rule (past it, the name would already
/// have been looked up at dns-direct), with the address rules after the resolve
/// repeated for it just above, since its connections to an IP still need them.
/// With no resolve rule its catch-all simply goes last. A direct or corp
/// catch-all shares the lookup and goes last, where main's final applies.
fn port_rules(rules: Vec<Value>, i: &HostInput) -> Vec<Value> {
    let inbound = |n: &str| json!([format!("{n}-in")]);
    let tun = i.ports.iter().filter(|p| p.fin == "escape");
    let mut out = match rules.iter().position(|r| r.get("action") == Some(&json!("resolve"))) {
        None => {
            let mut out = rules;
            out.extend(tun.map(|p| json!({"inbound": inbound(&p.name), "outbound": i.escape_outbound})));
            out
        }
        Some(ri) => {
            let mut out = rules[..ri].to_vec();
            for p in tun {
                out.push(json!({"inbound": inbound(&p.name), "rule_set": [CORP_SET_CIDR], "outbound": "corp"}));
                if i.private_default == "corp" {
                    out.push(json!({"inbound": inbound(&p.name), "ip_cidr": i.private_cidrs, "outbound": "corp"}));
                }
                out.push(json!({"inbound": inbound(&p.name), "outbound": i.escape_outbound}));
            }
            out.extend_from_slice(&rules[ri..]);
            out
        }
    };
    out.extend(
        i.ports.iter().filter(|p| p.fin != "escape")
            .map(|p| json!({"inbound": inbound(&p.name), "outbound": p.fin})),
    );
    out
}

/// The VM guest configuration — everything it receives is escape traffic.
pub fn render_vm(escapes: &Value, listen: &str, port: u64, clash: &str, secret: &str, log_level: &str) -> Value {
    let mut outbounds = escapes.as_array().cloned().unwrap_or_default();
    outbounds.push(json!({"type": "direct", "tag": "direct"}));
    outbounds.push(json!({"type": "block", "tag": "block"}));

    let mut root = Map::new();
    root.insert("log".into(), json!({"level": log_level, "timestamp": true}));
    root.insert("dns".into(), json!({"servers": [{"type": "local", "tag": "local"}]}));
    root.insert(
        "inbounds".into(),
        json!([{"type": "mixed", "tag": "in", "listen": listen, "listen_port": port}]),
    );
    root.insert("outbounds".into(), json!(outbounds));
    root.insert(
        "route".into(),
        json!({
            "rules": [{"action": "sniff"}], "final": "escape",
            "auto_detect_interface": true, "default_domain_resolver": "local"
        }),
    );
    if !clash.is_empty() {
        root.insert(
            "experimental".into(),
            json!({"clash_api": {"external_controller": clash, "secret": secret}}),
        );
    }
    Value::Object(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The env knob wins, then the saved `--every`, then 20m — and an EMPTY
    /// env value counts as unset, as `${ROWT_AUTO_INTERVAL:-}` does.
    #[test]
    fn the_auto_interval_prefers_env_then_state_then_the_default() {
        assert_eq!(auto_interval(Some("90s"), "3m"), "90s");
        assert_eq!(auto_interval(Some(""), "3m"), "3m");
        assert_eq!(auto_interval(None, "3m"), "3m");
        assert_eq!(auto_interval(None, ""), "20m");
    }

    #[test]
    fn durations_read_the_way_go_writes_them() {
        assert_eq!(duration_secs("3m"), Some(180));
        assert_eq!(duration_secs("90s"), Some(90));
        assert_eq!(duration_secs("1m30s"), Some(90));
        assert_eq!(duration_secs("1h"), Some(3600));
        for bad in ["", "3", "m", "3x", "1.5m", "-3m", "3m ", " 3m", "3M"] {
            assert_eq!(duration_secs(bad), None, "{bad:?}");
        }
    }

    fn auto(interval: &str) -> UrlTest {
        UrlTest::new(interval.to_string(), None, None)
    }

    #[test]
    fn the_urltest_carries_the_interval_it_is_given() {
        let servers = vec![json!({"type": "vless", "tag": "a", "server": "192.0.2.1", "server_port": 443})];
        let g = group(&servers, "", "auto", &auto("3m"));
        assert_eq!(g[1]["type"], "urltest");
        assert_eq!(g[1]["interval"], "3m");
        assert_eq!(g[1]["tolerance"], 100);
        assert!(g[1].get("interrupt_exist_connections").is_none(), "interrupting is opt-in");
    }

    #[test]
    fn tolerance_and_interrupt_come_from_their_knobs() {
        let servers = vec![json!({"type": "vless", "tag": "a", "server": "192.0.2.1", "server_port": 443})];
        let g = group(&servers, "", "auto", &UrlTest::new("20m".into(), Some("250"), Some("1")));
        assert_eq!(g[1]["tolerance"], 250);
        assert_eq!(g[1]["interrupt_exist_connections"], true);
        let keys: Vec<&String> = g[1].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["type", "tag", "outbounds", "url", "interval", "tolerance", "interrupt_exist_connections"],
                   "the shell's key order");
        assert!(UrlTest::new("20m".into(), None, Some("yes")) == auto("20m"), "only 1 turns it on");
        for (raw, want) in [(None, Some(100)), (Some(""), Some(100)), (Some("0"), Some(0)), (Some("007"), Some(7)),
                            (Some("99999"), Some(99999)), (Some("100000"), None), (Some("1e3"), None),
                            (Some("-5"), None), (Some(" 50"), None)] {
            assert_eq!(auto_tolerance(raw), want, "{raw:?}");
        }
    }

    #[test]
    fn list_parsing_matches_the_shell_pipeline() {
        let src = "# comment\n\nexample.com\n   spaced.example.org\ngeosite:google\n10.0.0.0/8\n  \n";
        assert_eq!(
            parse_list(src, Filter::Domain),
            vec!["example.com", "spaced.example.org"]
        );
        assert_eq!(parse_list(src, Filter::Cidr), vec!["10.0.0.0/8"]);
        assert_eq!(parse_list(src, Filter::All).len(), 3);
    }

    #[test]
    fn a_line_of_only_whitespace_is_dropped_not_emptied() {
        assert!(parse_list("   \n\t\n", Filter::All).is_empty());
    }

    /// A `*.z.com` on disk — written by an older rowt, or by hand from the
    /// docs — is read as the dot-led `.z.com` that sing-box can match. An exact
    /// `domain:*.z.com` is not a suffix and is left alone, and so are the shapes
    /// with nothing to anchor on.
    #[test]
    fn a_wildcard_entry_reads_as_its_dot_led_form() {
        let src = "*.wild.example\n  *.Spaced.example \n.dot.example\ndomain:*.exact.example\n*.\n*\na.*.example\n";
        assert_eq!(parse_list(src, Filter::Domain),
                   [".wild.example", ".Spaced.example", ".dot.example", "*.", "*", "a.*.example"]);
        assert_eq!(parse_list(src, Filter::Exact), ["*.exact.example"]);
        assert_eq!(unwildcard("*.z.com"), ".z.com");
        assert_eq!(unwildcard(".z.com"), ".z.com");
        assert_eq!(unwildcard("z.com"), "z.com");
    }

    #[test]
    fn geosite_lines_keep_file_order_and_duplicates() {
        let src = "geosite:google\nexample.com\ngeosite:apple\ngeosite:google\n";
        assert_eq!(geosites_of(src), vec!["google", "apple", "google"]);
    }

    #[test]
    fn selector_falls_back_when_the_selection_is_unknown() {
        let servers = vec![json!({"tag": "alpha"}), json!({"tag": "beta"})];
        let g = group(&servers, "en0", "ghost", &auto("20m"));
        let sel = g.as_array().unwrap().last().unwrap();
        assert_eq!(sel["default"], json!("alpha"));
        assert_eq!(g[0]["bind_interface"], json!("en0"));
    }

    #[test]
    fn an_empty_server_list_selects_block() {
        let g = group(&[], "en0", "ghost", &auto("20m"));
        assert_eq!(g.as_array().unwrap().last().unwrap()["default"], json!("block"));
    }

    /// The load-bearing exclusion: a `domain:` line has no `/N`, so it passes
    /// the "isn't a CIDR" test every other filter selects on. If it leaked
    /// through it would render as a `domain_suffix` — the exact leak the flag
    /// exists to prevent, and silent.
    #[test]
    fn exact_entries_never_leak_into_the_suffix_filters() {
        let src = "z.com\ndomain:host.z.com\n10.0.0.0/8\ngeosite:google\n";
        assert_eq!(parse_list(src, Filter::Domain), ["z.com"]);
        assert_eq!(parse_list(src, Filter::All), ["z.com", "10.0.0.0/8"]);
        assert_eq!(parse_list(src, Filter::Cidr), ["10.0.0.0/8"]);
        assert_eq!(parse_list(src, Filter::Exact), ["host.z.com"]);
        // A lane of nothing but exact entries renders NO suffix rules at all.
        let only = "domain:a.com\ndomain:b.com\n";
        assert!(parse_list(only, Filter::Domain).is_empty());
        assert_eq!(parse_list(only, Filter::Exact), ["a.com", "b.com"]);
    }

    #[test]
    fn a_rule_set_file_is_what_jq_writes() {
        assert_eq!(rule_set_json("ip_cidr", &[]), "{\"version\":1,\"rules\":[]}\n");
        assert_eq!(rule_set_json("domain_suffix", &["a.example".into(), "b.example".into()]),
                   "{\"version\":1,\"rules\":[{\"domain_suffix\":[\"a.example\",\"b.example\"]}]}\n");
    }

    #[test]
    fn bands_follow_jq_split_count_with_dot_led_entries_apart() {
        assert_eq!(Band::of("com"), Band::Bare(1));
        assert_eq!(Band::of("z.example"), Band::Bare(2));
        assert_eq!(Band::of(".z.example"), Band::Dot(3), "a leading dot splits to one more");
        assert_eq!(Band::of("x.z.example"), Band::Bare(3));
        assert_eq!(Band::of("a.b.c.d.e.f.g.example"), Band::Bare(8));
        assert_eq!(Band::of("a.b.c.d.e.f.g.h.i.example"), Band::Bare(8), "eight or more");
        assert_eq!(Band::of(".a.b.c.d.e.f.g.h.example"), Band::Dot(8));
        assert_eq!(Band::Exact.tag("corp"), "corp-exact");
        assert_eq!(Band::Bare(3).tag("escape"), "escape-3");
        assert_eq!(Band::Dot(3).tag("block"), "block-3-dot");
    }

    #[test]
    fn the_bands_are_emitted_deepest_first_bare_before_dot_exact_ahead() {
        let b = lane_bands();
        assert_eq!(b.len(), 48);
        let tags: Vec<String> = b.iter().map(|(l, band)| band.tag(l)).collect();
        assert_eq!(&tags[..6], ["escape-exact", "corp-exact", "block-exact", "escape-8", "corp-8", "block-8"]);
        assert_eq!(&tags[6..9], ["escape-8-dot", "corp-8-dot", "block-8-dot"]);
        assert_eq!(&tags[45..], ["escape-1", "corp-1", "block-1"], "no dot set for one label");
        let mut seen = std::collections::BTreeSet::new();
        assert!(tags.iter().all(|t| seen.insert(t.clone())), "every tag once");
    }

    /// How the former inline render ordered the three lanes' suffixes, kept
    /// here as the oracle: jq sorted by `[split count, length]` and reversed,
    /// stable over block, corp, escape — so escape, corp, block on a tie — with
    /// the exact entries first in lane order. First match wins.
    fn inline_lane(host: &str, lists: &Lists) -> Option<&'static str> {
        for lane in SET_LANES {
            if lane_lists(lists, lane).1.iter().any(|e| e == host) {
                return Some(lane);
            }
        }
        let mut pairs: Vec<(&str, &'static str)> = Vec::new();
        for lane in ["block", "corp", "escape"] {
            pairs.extend(lane_lists(lists, lane).0.iter().map(|s| (s.as_str(), lane)));
        }
        pairs.sort_by_key(|(s, _)| (s.split('.').count(), s.chars().count()));
        pairs.reverse();
        pairs.into_iter().find(|(s, _)| suffix_hit(host, s)).map(|(_, l)| l)
    }

    /// sing-box's `domain_suffix`: on a label boundary; a dot-led entry only
    /// below its name (the measured table in `classify::suffix_matches`).
    fn suffix_hit(host: &str, e: &str) -> bool {
        if e.starts_with('.') { host.ends_with(e) && host.len() > e.len() } else { host == e || host.ends_with(&format!(".{e}")) }
    }

    /// The lane the SETS route a host to: walk `lane_bands` in order, read each
    /// set's body back, first set with a match wins.
    fn set_lane(host: &str, lists: &Lists) -> Option<&'static str> {
        let sets = lane_sets(lists);
        for ((lane, band), (tag, body)) in lane_bands().into_iter().zip(sets) {
            assert_eq!(band.tag(lane), tag);
            let v: Value = serde_json::from_str(&body).unwrap();
            let hit = v["rules"].as_array().unwrap().iter().any(|r| match band {
                Band::Exact => r["domain"].as_array().unwrap().iter().any(|d| d == host),
                _ => r["domain_suffix"].as_array().unwrap().iter().any(|d| suffix_hit(host, d.as_str().unwrap())),
            });
            if hit {
                return Some(lane);
            }
        }
        None
    }

    /// The claim the whole design rests on: for every host, the sets route
    /// where the inline order did. The lists hold every shape that can cross
    /// lanes — a bare entry under another lane's apex, a dot-led entry against
    /// a bare one of the same split count, an exact against a suffix, a
    /// synced-looking corp domain between hand entries, a tie on length.
    #[test]
    fn the_sets_route_every_host_where_the_inline_order_did() {
        let lists = Lists {
            escape_domains: vec!["x.z.example".into(), "api.foo.example".into(), "aaa.example".into(), "deep.q.x.z.example".into()],
            corp_domains: vec![".z.example".into(), "foo.example".into(), "bbb.example".into(), "hq.corp.example".into(), "corp.example".into()],
            block_domains: vec!["z.example".into(), "a.example".into(), ".q.x.z.example".into(), "ccc.example".into(), "ads.hq.corp.example".into()],
            escape_exact: vec!["only.a.example".into()],
            corp_exact: vec!["foo.example".into()],
            block_exact: vec!["x.z.example".into()],
            ..Default::default()
        };
        let hosts = ["x.z.example", "a.x.z.example", "z.example", "www.z.example", "q.x.z.example", "t.q.x.z.example",
                     "deep.q.x.z.example", "m.deep.q.x.z.example", "foo.example", "b.foo.example", "api.foo.example",
                     "v2.api.foo.example", "a.example", "only.a.example", "x.only.a.example", "aaa.example", "bbb.example",
                     "ccc.example", "corp.example", "hq.corp.example", "w.hq.corp.example", "ads.hq.corp.example",
                     "pixel.ads.hq.corp.example", "unlisted.example", "example", "xz.example"];
        for h in hosts {
            assert_eq!(set_lane(h, &lists), inline_lane(h, &lists), "{h}");
        }
        // And the two shapes that make the dot sets necessary, named: the bare
        // 3-label entry beats the dot-led 2-label one of another lane (length),
        // and the dot-led one beats the bare 2-label apex of a third (depth).
        assert_eq!(set_lane("x.z.example", &lists), Some("block"), "exact beats both suffixes");
        assert_eq!(set_lane("a.x.z.example", &lists), Some("escape"));
        assert_eq!(set_lane("www.z.example", &lists), Some("corp"));
        assert_eq!(set_lane("z.example", &lists), Some("block"));
    }

    #[test]
    fn the_sets_hold_each_lane_lowercased_in_file_order() {
        let lists = Lists {
            escape_domains: vec!["B.Example".into(), "a.example".into(), ".Sub.Example".into()],
            escape_exact: vec!["Host.Example".into()],
            ..Default::default()
        };
        let sets: std::collections::BTreeMap<String, String> = lane_sets(&lists).into_iter().collect();
        assert_eq!(sets["escape-2"], "{\"version\":1,\"rules\":[{\"domain_suffix\":[\"b.example\",\"a.example\"]}]}\n");
        assert_eq!(sets["escape-3-dot"], "{\"version\":1,\"rules\":[{\"domain_suffix\":[\".sub.example\"]}]}\n");
        assert_eq!(sets["escape-exact"], "{\"version\":1,\"rules\":[{\"domain\":[\"host.example\"]}]}\n");
        assert_eq!(sets["corp-2"], "{\"version\":1,\"rules\":[]}\n", "an empty set is still a file");
        assert_eq!(sets.len(), 48);
    }

    #[test]
    fn the_overflow_band_and_case_twins_are_named() {
        let lists = Lists {
            escape_domains: vec!["a.b.c.d.e.f.g.h.i.example".into(), "Twin.Example".into()],
            corp_domains: vec!["c.d.e.f.g.h.i.example".into(), "twin.example".into()],
            block_domains: vec!["d.e.f.g.h.i.example".into()],
            ..Default::default()
        };
        let pairs = overflow_pairs(&lists);
        assert_eq!(pairs, vec![("a.b.c.d.e.f.g.h.i.example".into(), "escape", "c.d.e.f.g.h.i.example".into(), "corp")],
                   "only a suffix of eight labels or more shares band 8; the seven-label one ranks below it");
        assert_eq!(case_twins(&lists), vec![("twin.example".into(), "escape", "corp")]);
    }
}
