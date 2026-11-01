//! `config export --to shadowrocket` — rowt's lanes as a Shadowrocket config.
//!
//! The shell's `_export_shadowrocket`, with its jq and awk made into functions
//! over already-read inputs: everything here is text in, text out, so the rule
//! order, the wildcard spelling and the counts are unit-tested on inputs the
//! parity sandbox never produces. The IO around it — the lane files, the
//! sing-box decompile of each geosite category, the files written — is the
//! CLI's (`rowt-cli::bundle`).
//!
//! What a rule says, in Shadowrocket's words: an escape entry is `PROXY`
//! (whichever server is selected there), a block entry `REJECT`, corp and
//! hotspot `DIRECT`, and the catch-all `FINAL,DIRECT`. The order follows rowt's
//! precedence — the first matching rule wins — which is why the hand-listed
//! suffixes are sorted most specific first.

use serde_json::Value;

/// A geosite category a lane names, and what the cache had for it.
pub struct Geo {
    pub category: String,
    pub policy: &'static str,
    pub set: GeoSet,
}

pub enum GeoSet {
    /// No `geosite-<c>.srs` in the cache: the category is not exported.
    NotCached,
    /// `sing-box rule-set decompile` failed on it.
    Undecompilable,
    /// The decompiled rule-set, as sing-box prints it.
    Rules(Value),
}

pub struct Input<'a> {
    pub prog: &'a str,
    /// The hotspot lane's suffixes (`list_json … domain`).
    pub hotspot: &'a [String],
    /// `domain:` entries per lane.
    pub escape_exact: &'a [String],
    pub corp_exact: &'a [String],
    pub block_exact: &'a [String],
    /// Suffix entries per lane, duplicates and all — `list_json` does not dedupe.
    pub escape: &'a [String],
    pub corp: &'a [String],
    pub block: &'a [String],
    /// The escape lane's categories (PROXY), then the block lane's (REJECT), in
    /// file order with duplicates kept, as `geosites_of` lists them.
    pub geos: &'a [Geo],
    pub corp_cidrs: &'a [String],
    /// `ROWT_PRIVATE_DEFAULT` is `corp`: the private ranges go DIRECT too.
    pub private_default_corp: bool,
    /// The ad set (`geosite-category-ads-all.srs` in the cache), REJECT.
    pub ads: Option<&'a Geo>,
}

/// `PRIVATE_CIDRS` in bin/rowt.
pub const PRIVATE_CIDRS: [&str; 5] =
    ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "100.64.0.0/10", "169.254.0.0/16"];

pub struct Rules {
    pub lines: Vec<String>,
    /// Regex entries met in the geosite categories, which Shadowrocket cannot
    /// hold — counted, left out, and said in the header and the summary.
    pub regex: usize,
}

/// jq's `sr(p)`: a stored `.z.com` (what `*.z.com` arrives as) is the names
/// UNDER z.com — `DOMAIN-WILDCARD,*.z.com`; anything else is the name and
/// everything under it — `DOMAIN-SUFFIX`.
pub fn sr(entry: &str, policy: &str) -> String {
    let e = entry.to_ascii_lowercase();
    if e.starts_with('.') {
        format!("DOMAIN-WILDCARD,*{e},{policy}")
    } else {
        format!("DOMAIN-SUFFIX,{e},{policy}")
    }
}

/// A sing-box "listable" field: an array, or a bare string when it holds one
/// value, or nothing.
fn each(v: Option<&Value>) -> Vec<&str> {
    match v {
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str()).collect(),
        Some(Value::String(s)) => vec![s.as_str()],
        _ => vec![],
    }
}

fn listable_len(v: Option<&Value>) -> usize {
    match v {
        Some(Value::Array(a)) => a.len(),
        Some(Value::String(_)) => 1,
        _ => 0,
    }
}

/// One category, expanded — the shell's `_sr_geo`. Returns the lines and the
/// regex entries it had to leave out.
pub fn geo_lines(g: &Geo, prog: &str) -> (Vec<String>, usize) {
    let c = &g.category;
    match &g.set {
        GeoSet::NotCached => (vec![format!("# geosite:{c} — not cached, so not exported (run '{prog} fetch host')")], 0),
        GeoSet::Undecompilable => (vec![format!("# geosite:{c} — could not be decompiled, so not exported")], 0),
        GeoSet::Rules(doc) => {
            let mut o = vec![format!("# geosite:{c}")];
            let mut regex = 0;
            let rules = doc.get("rules").and_then(|r| r.as_array()).cloned().unwrap_or_default();
            for r in &rules {
                for d in each(r.get("domain")) {
                    o.push(format!("DOMAIN,{},{}", d.to_ascii_lowercase(), g.policy));
                }
                for s in each(r.get("domain_suffix")) {
                    o.push(sr(s, g.policy));
                }
                for k in each(r.get("domain_keyword")) {
                    o.push(format!("DOMAIN-KEYWORD,{},{}", k.to_ascii_lowercase(), g.policy));
                }
                regex += listable_len(r.get("domain_regex"));
            }
            (o, regex)
        }
    }
}

/// The `[Rule]` body, in the shell's order: hotspot, exact hosts, the hand-
/// listed suffixes most specific first, the escape then block categories, the
/// corp and private ranges, the ad set, `FINAL,DIRECT`.
pub fn rules(i: &Input) -> Rules {
    let mut o: Vec<String> = Vec::new();
    let mut regex = 0;
    o.push("# hotspot lane: reached without the proxy".into());
    o.extend(i.hotspot.iter().map(|e| sr(e, "DIRECT")));
    o.push("# exact hosts (--domain)".into());
    o.extend(i.escape_exact.iter().map(|e| format!("DOMAIN,{},PROXY", e.to_ascii_lowercase())));
    o.extend(i.corp_exact.iter().map(|e| format!("DOMAIN,{},DIRECT", e.to_ascii_lowercase())));
    o.extend(i.block_exact.iter().map(|e| format!("DOMAIN,{},REJECT", e.to_ascii_lowercase())));
    o.push("# hand-listed suffixes, most specific first".into());
    // jq: block + corp + escape, `sort_by([labels, length]) | reverse`. sort_by
    // is stable, so among equal keys the LATER input comes first after the
    // reverse — escape before corp before block, and a lane's own entries in
    // reverse file order. Lengths are in characters, as jq counts them.
    let mut suf: Vec<(&String, &str)> = Vec::new();
    suf.extend(i.block.iter().map(|s| (s, "REJECT")));
    suf.extend(i.corp.iter().map(|s| (s, "DIRECT")));
    suf.extend(i.escape.iter().map(|s| (s, "PROXY")));
    suf.sort_by_key(|(s, _)| (s.split('.').count(), s.chars().count()));
    suf.reverse();
    o.extend(suf.iter().map(|(s, p)| sr(s, p)));
    for g in i.geos {
        let (lines, n) = geo_lines(g, i.prog);
        o.extend(lines);
        regex += n;
    }
    o.push("# corp and private ranges".into());
    o.extend(i.corp_cidrs.iter().map(|c| {
        if c.contains(':') { format!("IP-CIDR6,{c},DIRECT,no-resolve") } else { format!("IP-CIDR,{c},DIRECT,no-resolve") }
    }));
    if i.private_default_corp {
        o.extend(PRIVATE_CIDRS.iter().map(|c| format!("IP-CIDR,{c},DIRECT,no-resolve")));
    }
    if let Some(ads) = i.ads {
        let (lines, n) = geo_lines(ads, i.prog);
        o.extend(lines);
        regex += n;
    }
    o.push("FINAL,DIRECT".into());
    Rules { lines: o, regex }
}

/// `skip-proxy`: Shadowrocket's own defaults, the captive-portal probes rowt
/// also bypasses, and the hotspot lane as macOS bypass entries — first
/// occurrence kept (`awk '!seen[$0]++'`), joined ", ".
pub fn skip_proxy(hotspot_bypass: &[String]) -> String {
    let fixed = ["192.168.0.0/16", "10.0.0.0/8", "172.16.0.0/12", "localhost", "*.local", "captive.apple.com",
                 "connectivitycheck.gstatic.com", "detectportal.firefox.com", "www.msftconnecttest.com"];
    let mut seen: Vec<&str> = Vec::new();
    for e in fixed.iter().copied().chain(hotspot_bypass.iter().map(|s| s.as_str())) {
        if !seen.contains(&e) {
            seen.push(e);
        }
    }
    seen.join(", ")
}

/// The whole config, as the shell writes it — a trailing newline included.
pub fn config(version: &str, stamp: &str, prog: &str, rules: &Rules, skip_proxy: &str) -> String {
    let mut o: Vec<String> = vec![
        format!("# Shadowrocket config written by rowt {version} ('{prog} config export --to shadowrocket'), {stamp}."),
        "# Routing rules only: PROXY is whichever server is selected in Shadowrocket (no server is in this file).".into(),
        "#   escape -> PROXY   block -> REJECT   corp, hotspot -> DIRECT   everything else -> FINAL,DIRECT".into(),
        "# The order follows rowt's precedence (the first matching rule wins). Regenerate rather than edit.".into(),
    ];
    if rules.regex > 0 {
        o.push(format!("# {} regex entries from the geosite categories are not included: Shadowrocket has no domain-regex rule.", rules.regex));
    }
    o.push(String::new());
    o.push("[General]".into());
    o.push("bypass-system = true".into());
    o.push(format!("skip-proxy = {skip_proxy}"));
    o.push("tun-excluded-routes = 10.0.0.0/8, 100.64.0.0/10, 127.0.0.0/8, 169.254.0.0/16, 172.16.0.0/12, 192.0.0.0/24, 192.0.2.0/24, 192.88.99.0/24, 192.168.0.0/16, 198.51.100.0/24, 203.0.113.0/24, 224.0.0.0/4, 255.255.255.255/32".into());
    o.push("dns-server = system".into());
    o.push("ipv6 = true".into());
    o.push(String::new());
    o.push("[Rule]".into());
    o.extend(rules.lines.iter().cloned());
    o.push(String::new());
    o.push("[Host]".into());
    o.push("localhost = 127.0.0.1".into());
    let mut s = o.join("\n");
    s.push('\n');
    s
}

#[derive(Debug, PartialEq, Eq)]
pub struct Counts {
    pub total: usize,
    pub proxy: usize,
    pub reject: usize,
    pub direct: usize,
}

/// Counted without the FINAL line, which the summary names on its own.
pub fn counts(rules: &Rules) -> Counts {
    let proxy = rules.lines.iter().filter(|l| l.ends_with(",PROXY")).count();
    let reject = rules.lines.iter().filter(|l| l.ends_with(",REJECT")).count();
    let direct = rules.lines.iter()
        .filter(|l| !l.starts_with("FINAL,"))
        .filter(|l| l.ends_with(",DIRECT") || l.ends_with(",DIRECT,no-resolve"))
        .count();
    Counts { total: proxy + reject + direct, proxy, reject, direct }
}

/// The summary the shell prints on stderr. `servers` is the servers file with
/// its share-link and subscription counts, when one was written.
pub fn summary(c: &Counts, regex: usize, servers: Option<(usize, usize, &str)>, to_stdout: bool) -> Vec<String> {
    let mut o = vec![format!(
        "  {} rules: {} PROXY (escape), {} REJECT (block), {} DIRECT (corp, hotspot, ranges) + FINAL,DIRECT",
        c.total, c.proxy, c.reject, c.direct
    )];
    if regex > 0 {
        o.push(format!("  {regex} geosite regex entries left out (Shadowrocket has no domain-regex rule)"));
    }
    match servers {
        Some((nlinks, nsubs, sfile)) => {
            o.push(format!("  servers: {nlinks} share link(s) + {nsubs} subscription URL(s) in {sfile}"));
            o.push("  ⚠ that file holds credentials: move it by AirDrop (encrypted) and delete it after importing.".into());
        }
        None => o.push("  rules only (--routes-only): PROXY uses whichever server is selected in Shadowrocket".into()),
    }
    if !to_stdout {
        o.push("  on the iPhone: AirDrop (or iCloud Drive) the config, open it with Shadowrocket (it".into());
        o.push("  lands in Config), tap it, Use Config. For the servers, copy the servers file's text".into());
        o.push("  and add it in Shadowrocket, which imports share links from the clipboard.".into());
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_dot_led_entry_is_a_wildcard_and_the_rest_a_suffix_lowercased() {
        assert_eq!(sr(".Z.com", "PROXY"), "DOMAIN-WILDCARD,*.z.com,PROXY");
        assert_eq!(sr("Z.com", "REJECT"), "DOMAIN-SUFFIX,z.com,REJECT");
    }

    #[test]
    fn suffixes_sort_most_specific_first_and_ties_break_escape_corp_block_in_reverse_file_order() {
        let i = Input {
            prog: "rowt", hotspot: &[], escape_exact: &[], corp_exact: &[], block_exact: &[],
            escape: &v(&["e.x", "e2.x", "a.b.x"]), corp: &v(&["c.x", "zz.x"]), block: &v(&["b.x"]),
            geos: &[], corp_cidrs: &[], private_default_corp: false, ads: None,
        };
        let r = rules(&i);
        let from = r.lines.iter().position(|l| l == "# hand-listed suffixes, most specific first").unwrap();
        let to = r.lines.iter().position(|l| l == "# corp and private ranges").unwrap();
        // jq: `sort_by([labels, len]) | reverse` over block + corp + escape, checked
        // against jq itself: a.b.x (3 labels), then by length 4 (zz.x, e2.x — the
        // later input first), then the 3-long ties escape, corp, block.
        assert_eq!(&r.lines[from + 1..to], &[
            "DOMAIN-SUFFIX,a.b.x,PROXY", "DOMAIN-SUFFIX,e2.x,PROXY", "DOMAIN-SUFFIX,zz.x,DIRECT",
            "DOMAIN-SUFFIX,e.x,PROXY", "DOMAIN-SUFFIX,c.x,DIRECT", "DOMAIN-SUFFIX,b.x,REJECT",
        ]);
    }

    #[test]
    fn duplicates_in_a_lane_are_emitted_twice_as_list_json_leaves_them() {
        let i = Input {
            prog: "rowt", hotspot: &[], escape_exact: &[], corp_exact: &[], block_exact: &[],
            escape: &v(&["example.com", "example.com"]), corp: &[], block: &[],
            geos: &[], corp_cidrs: &[], private_default_corp: false, ads: None,
        };
        assert_eq!(rules(&i).lines.iter().filter(|l| l.as_str() == "DOMAIN-SUFFIX,example.com,PROXY").count(), 2);
    }

    #[test]
    fn a_category_expands_its_listable_fields_in_rule_order_and_counts_its_regexes() {
        let doc: Value = serde_json::from_str(r#"{"rules":[
            {"domain":["G.example","accounts.g.example"],"domain_suffix":[".apis.example","static.example"],
             "domain_keyword":"google","domain_regex":["^a$","^b$"]},
            {"domain_suffix":".tube.example","domain_regex":"^c$"}]}"#).unwrap();
        let g = Geo { category: "google".into(), policy: "PROXY", set: GeoSet::Rules(doc) };
        let (lines, regex) = geo_lines(&g, "rowt");
        assert_eq!(lines, vec![
            "# geosite:google",
            "DOMAIN,g.example,PROXY", "DOMAIN,accounts.g.example,PROXY",
            "DOMAIN-WILDCARD,*.apis.example,PROXY", "DOMAIN-SUFFIX,static.example,PROXY",
            "DOMAIN-KEYWORD,google,PROXY",
            "DOMAIN-WILDCARD,*.tube.example,PROXY",
        ]);
        assert_eq!(regex, 3);
        let nc = Geo { category: "apple".into(), policy: "REJECT", set: GeoSet::NotCached };
        assert_eq!(geo_lines(&nc, "rowt").0, vec!["# geosite:apple — not cached, so not exported (run 'rowt fetch host')"]);
        let bad = Geo { category: "apple".into(), policy: "REJECT", set: GeoSet::Undecompilable };
        assert_eq!(geo_lines(&bad, "rowt").0, vec!["# geosite:apple — could not be decompiled, so not exported"]);
    }

    #[test]
    fn ranges_private_defaults_ads_and_final_close_the_rules() {
        let ads = Geo { category: "category-ads-all".into(), policy: "REJECT",
                        set: GeoSet::Rules(serde_json::json!({"rules":[{"domain_suffix":".ads.example"}]})) };
        let i = Input {
            prog: "rowt", hotspot: &v(&[".portal.example", "login.example"]),
            escape_exact: &v(&["Exact.Example"]), corp_exact: &[], block_exact: &v(&["bad.example"]),
            escape: &[], corp: &[], block: &[], geos: &[],
            corp_cidrs: &v(&["198.51.100.0/24", "2001:db8::/32"]), private_default_corp: true, ads: Some(&ads),
        };
        let r = rules(&i);
        assert_eq!(r.lines, vec![
            "# hotspot lane: reached without the proxy",
            "DOMAIN-WILDCARD,*.portal.example,DIRECT", "DOMAIN-SUFFIX,login.example,DIRECT",
            "# exact hosts (--domain)",
            "DOMAIN,exact.example,PROXY", "DOMAIN,bad.example,REJECT",
            "# hand-listed suffixes, most specific first",
            "# corp and private ranges",
            "IP-CIDR,198.51.100.0/24,DIRECT,no-resolve", "IP-CIDR6,2001:db8::/32,DIRECT,no-resolve",
            "IP-CIDR,10.0.0.0/8,DIRECT,no-resolve", "IP-CIDR,172.16.0.0/12,DIRECT,no-resolve",
            "IP-CIDR,192.168.0.0/16,DIRECT,no-resolve", "IP-CIDR,100.64.0.0/10,DIRECT,no-resolve",
            "IP-CIDR,169.254.0.0/16,DIRECT,no-resolve",
            "# geosite:category-ads-all", "DOMAIN-WILDCARD,*.ads.example,REJECT",
            "FINAL,DIRECT",
        ]);
        assert_eq!(counts(&r), Counts { total: 12, proxy: 1, reject: 2, direct: 9 });
    }

    #[test]
    fn skip_proxy_keeps_the_first_occurrence_of_each_entry() {
        let s = skip_proxy(&v(&["*.local", "login.example", "*.login.example", "10.0.0.0/8"]));
        assert_eq!(s, "192.168.0.0/16, 10.0.0.0/8, 172.16.0.0/12, localhost, *.local, captive.apple.com, connectivitycheck.gstatic.com, detectportal.firefox.com, www.msftconnecttest.com, login.example, *.login.example");
    }

    #[test]
    fn the_header_names_the_regex_count_only_when_there_is_one() {
        let r = Rules { lines: vec!["FINAL,DIRECT".into()], regex: 0 };
        let c = config("3.8.30", "2026-11-01 12:00:00", "rowt", &r, "localhost");
        assert!(c.starts_with("# Shadowrocket config written by rowt 3.8.30 ('rowt config export --to shadowrocket'), 2026-11-01 12:00:00.\n"));
        assert!(!c.contains("regex entries"));
        assert!(c.ends_with("[Rule]\nFINAL,DIRECT\n\n[Host]\nlocalhost = 127.0.0.1\n"));
        let r2 = Rules { lines: vec![], regex: 4 };
        assert!(config("3.8.30", "t", "rowt", &r2, "x").contains("\n# 4 regex entries from the geosite categories are not included: Shadowrocket has no domain-regex rule.\n"));
    }

    #[test]
    fn the_summary_says_servers_or_rules_only_and_the_iphone_steps_for_a_file() {
        let c = Counts { total: 3, proxy: 1, reject: 1, direct: 1 };
        let s = summary(&c, 0, None, true);
        assert_eq!(s, vec![
            "  3 rules: 1 PROXY (escape), 1 REJECT (block), 1 DIRECT (corp, hotspot, ranges) + FINAL,DIRECT",
            "  rules only (--routes-only): PROXY uses whichever server is selected in Shadowrocket",
        ]);
        let s = summary(&c, 2, Some((4, 1, "x-servers.txt")), false);
        assert_eq!(s[1], "  2 geosite regex entries left out (Shadowrocket has no domain-regex rule)");
        assert_eq!(s[2], "  servers: 4 share link(s) + 1 subscription URL(s) in x-servers.txt");
        assert_eq!(s.len(), 7);
    }
}
