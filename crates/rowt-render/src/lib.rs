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
    pub corp_domains: Vec<String>,
    pub corp_cidrs: Vec<String>,
    pub block_domains: Vec<String>,
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
}

/// Parse one lane list exactly as `list_json` does.
///
/// The order matters and is not obvious: comment/blank lines are dropped on the
/// RAW line, then *all* whitespace is removed from what survives (not merely
/// trimmed — an inner space would close up too), then `geosite:` meta lines go,
/// then the CIDR/domain filter runs, and finally empties are dropped.
pub fn parse_list(contents: &str, filter: Filter) -> Vec<String> {
    contents
        .lines()
        .filter(|raw| {
            let t = raw.trim_start_matches([' ', '\t']);
            !(t.is_empty() || t.starts_with('#'))
        })
        .map(|raw| raw.chars().filter(|c| !c.is_whitespace()).collect::<String>())
        .filter(|s| !s.starts_with("geosite:"))
        .filter(|s| match filter {
            Filter::All => true,
            Filter::Cidr => is_cidr(s),
            Filter::Domain => !is_cidr(s),
        })
        .filter(|s| !s.is_empty())
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    All,
    Cidr,
    Domain,
}

/// `grep -E '/[0-9]+$'` — a trailing slash-and-digits marks a CIDR.
fn is_cidr(s: &str) -> bool {
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

/// The escape outbound group: members (optionally interface-bound), then either
/// a urltest + selector (`auto`) or a plain selector pinned to one server.
pub fn group(servers: &[Value], iface: &str, selected: &str, interval: &str) -> Value {
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
        out.push(json!({
            "type": "urltest", "tag": "auto", "outbounds": tags,
            "url": "https://www.gstatic.com/generate_204", "interval": interval
        }));
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

/// Suffix rules across the three hand lists, most-specific first.
///
/// jq sorts by `[label count, string length]` ascending and then reverses the
/// WHOLE array. jq's sort is stable, so equal keys keep block/corp/escape order
/// — and the reverse then flips that to escape/corp/block. Rust's `sort_by` is
/// stable too, so the same two steps reproduce it exactly, ties included.
fn suffix_rules(lists: &Lists) -> Vec<Value> {
    let mut pairs: Vec<(&str, &str)> = Vec::new();
    pairs.extend(lists.block_domains.iter().map(|s| (s.as_str(), "block")));
    pairs.extend(lists.corp_domains.iter().map(|s| (s.as_str(), "corp")));
    pairs.extend(lists.escape_domains.iter().map(|s| (s.as_str(), "escape")));

    // jq's `length` on a string counts codepoints, not bytes.
    pairs.sort_by_key(|(s, _)| (s.split('.').count(), s.chars().count()));
    pairs.reverse();

    pairs
        .into_iter()
        .map(|(s, o)| json!({"domain_suffix": [s], "outbound": o}))
        .collect()
}

/// The host configuration — what lands in `host.json`.
pub fn render_host(i: &HostInput) -> Value {
    let mut rules: Vec<Value> = vec![json!({"action": "sniff"})];
    rules.extend(suffix_rules(&i.lists));
    rules.extend(
        i.geo.escape.iter()
            .map(|n| json!({"rule_set": [format!("geosite-{n}")], "outbound": "escape"})),
    );
    rules.extend(
        i.geo.block.iter()
            .map(|n| json!({"rule_set": [format!("geosite-{n}")], "outbound": "block"})),
    );
    if !i.lists.corp_cidrs.is_empty() {
        rules.push(json!({"ip_cidr": i.lists.corp_cidrs, "outbound": "corp"}));
    }
    if i.private_default == "corp" {
        rules.push(json!({"ip_cidr": i.private_cidrs, "outbound": "corp"}));
    }
    if !i.geo.ads_path.is_empty() {
        rules.push(json!({"rule_set": ["geosite-ads"], "outbound": "block"}));
    }

    let dns_servers = json!([
        {"type": "local", "tag": "local"},
        {"type": "https", "tag": "dns-direct", "server": i.dns_direct, "detour": "direct"}
    ]);
    let dns_rules: Vec<Value> = if i.lists.corp_domains.is_empty() {
        vec![]
    } else {
        vec![json!({"domain_suffix": i.lists.corp_domains, "server": "local"})]
    };

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
    if !allsets.is_empty() {
        route.insert("rule_set".into(), json!(allsets));
    }

    let mut root = Map::new();
    root.insert("log".into(), json!({"level": i.log_level, "timestamp": true}));
    root.insert(
        "dns".into(),
        json!({"servers": dns_servers, "rules": dns_rules, "final": "dns-direct"}),
    );
    root.insert(
        "inbounds".into(),
        json!([{"type": "mixed", "tag": "in", "listen": i.listen, "listen_port": i.port}]),
    );
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

    #[test]
    fn geosite_lines_keep_file_order_and_duplicates() {
        let src = "geosite:google\nexample.com\ngeosite:apple\ngeosite:google\n";
        assert_eq!(geosites_of(src), vec!["google", "apple", "google"]);
    }

    #[test]
    fn suffix_rules_are_most_specific_first() {
        let lists = Lists {
            escape_domains: vec!["api.foo.com".into(), "z.com".into()],
            block_domains: vec!["foo.com".into()],
            ..Default::default()
        };
        let got: Vec<String> = suffix_rules(&lists)
            .iter()
            .map(|r| r["domain_suffix"][0].as_str().unwrap().to_string())
            .collect();
        assert_eq!(got, vec!["api.foo.com", "foo.com", "z.com"]);
    }

    #[test]
    fn ties_resolve_escape_then_corp_then_block() {
        // Same label count and length in all three lanes: jq sorts stably and
        // then reverses the whole array, which flips the lane order.
        let lists = Lists {
            escape_domains: vec!["aaa.com".into()],
            corp_domains: vec!["bbb.com".into()],
            block_domains: vec!["ccc.com".into()],
            ..Default::default()
        };
        let got: Vec<String> = suffix_rules(&lists)
            .iter()
            .map(|r| r["outbound"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(got, vec!["escape", "corp", "block"]);
    }

    #[test]
    fn selector_falls_back_when_the_selection_is_unknown() {
        let servers = vec![json!({"tag": "alpha"}), json!({"tag": "beta"})];
        let g = group(&servers, "en0", "ghost", "20m");
        let sel = g.as_array().unwrap().last().unwrap();
        assert_eq!(sel["default"], json!("alpha"));
        assert_eq!(g[0]["bind_interface"], json!("en0"));
    }

    #[test]
    fn an_empty_server_list_selects_block() {
        let g = group(&[], "en0", "ghost", "20m");
        assert_eq!(g.as_array().unwrap().last().unwrap()["default"], json!("block"));
    }
}
