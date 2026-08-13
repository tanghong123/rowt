//! Which lane does a destination take, and why.
//!
//! A reimplementation of `cmd_explain` + `_longest_domain_hit` + `_list_hit` +
//! `_private_hit` + `_cidr_has` from bin/rowt. Held to them by replaying the
//! generated corpus through both implementations and requiring identical
//! `(lane, reason)` — the lane alone is too weak a gate, since a classifier that
//! matched the wrong suffix still lands on the right lane (PORTING.md §6.3).
//!
//! DNS resolution is deliberately absent: it is a platform call. A caller that
//! has an answer passes it in as `resolved_ip`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    Escape,
    Corp,
    Direct,
    Block,
}

impl Lane {
    pub fn as_str(self) -> &'static str {
        match self {
            Lane::Escape => "escape",
            Lane::Corp => "corp",
            Lane::Direct => "direct",
            Lane::Block => "block",
        }
    }
    pub fn upper(self) -> String {
        self.as_str().to_uppercase()
    }
    /// The parenthetical on `explain`'s first line.
    pub fn description(self) -> &'static str {
        match self {
            Lane::Escape => "personal VLESS tunnel — through the proxy",
            Lane::Corp => "into the corp VPN via the OS route",
            Lane::Direct => "straight out the physical NIC — pass-through",
            Lane::Block => "refused — ad/telemetry sinkhole (no DNS, no dial)",
        }
    }
    pub fn parse(s: &str) -> Option<Lane> {
        match s {
            "escape" => Some(Lane::Escape),
            "corp" => Some(Lane::Corp),
            "direct" => Some(Lane::Direct),
            "block" => Some(Lane::Block),
            _ => None,
        }
    }
}

pub struct ClassifyInput<'a> {
    pub escape_list: &'a str,
    pub corp_list: &'a str,
    pub block_list: &'a str,
    pub private_cidrs: &'a [String],
    /// `ROWT_PRIVATE_DEFAULT` — private ranges only fall to corp when this is "corp".
    pub private_default: &'a str,
    pub final_route: Lane,
    /// True in local mode, where an escape verdict really leaves via direct.
    pub local_mode: bool,
    /// A resolved address for a name, when the caller has one. Empty otherwise.
    pub resolved_ip: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    pub dest: String,
    pub ip: String,
    pub lane: Lane,
    pub why: String,
}

impl Classification {
    /// The two lines `rowt explain` prints before its optional note/live lines.
    pub fn render(&self) -> String {
        let ip = if self.ip.is_empty() {
            String::new()
        } else {
            format!(" ({})", self.ip)
        };
        format!(
            "{}{}  ->  {}   ({})\n  matched: {}",
            self.dest,
            ip,
            self.lane.upper(),
            self.lane.description(),
            self.why
        )
    }
}

/// `sed -E 's#^[a-zA-Z]+://##; s#/.*$##; s#:[0-9]+$##' | tr A-Z a-z`, in order.
pub fn normalize_dest(input: &str) -> String {
    let mut s = input.to_string();
    if let Some(i) = s.find("://") {
        if !s[..i].is_empty() && s[..i].chars().all(|c| c.is_ascii_alphabetic()) {
            s = s[i + 3..].to_string();
        }
    }
    if let Some(i) = s.find('/') {
        s.truncate(i);
    }
    if let Some(i) = s.rfind(':') {
        let tail = &s[i + 1..];
        if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) {
            s.truncate(i);
        }
    }
    s.to_ascii_lowercase()
}

/// `grep -qE '^([0-9]{1,3}\.){3}[0-9]{1,3}$'` — deliberately permissive: the
/// shell accepts octets above 255 here, and so must this.
pub fn is_ipv4(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 4
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 3 && p.chars().all(|c| c.is_ascii_digit()))
}

/// `_ipint` — note it does NOT mask to 32 bits, so an octet above 255 spills
/// into the higher bits exactly as bash's arithmetic does. i64 reproduces that.
fn ipint(s: &str) -> i64 {
    let p: Vec<i64> = s.split('.').map(|x| x.parse().unwrap_or(0)).collect();
    let g = |i: usize| p.get(i).copied().unwrap_or(0);
    (g(0) << 24) + (g(1) << 16) + (g(2) << 8) + g(3)
}

/// `_cidr_has` — rc 0 if IPv4 `ip` falls inside `cidr`.
pub fn cidr_has(ip: &str, cidr: &str) -> bool {
    let (net, bits) = match cidr.split_once('/') {
        Some((n, b)) => (n, b.parse::<u32>().unwrap_or(32)),
        None => (cidr, 32),
    };
    if !is_ipv4(net) {
        return false;
    }
    let mask: i64 = if bits == 0 {
        0
    } else if bits >= 32 {
        0xFFFF_FFFF
    } else {
        ((0xFFFF_FFFFu64 << (32 - bits)) & 0xFFFF_FFFF) as i64
    };
    (ipint(ip) & mask) == (ipint(net) & mask)
}

/// One usable entry per line: comments and blanks dropped, all whitespace
/// stripped from what survives — the same shape `_list_hit` reads.
fn entries(list: &str) -> impl Iterator<Item = String> + '_ {
    list.lines()
        .map(|l| l.chars().filter(|c| !c.is_whitespace()).collect::<String>())
        .filter(|e| !e.is_empty() && !e.starts_with('#'))
}

/// sing-box's `domain_suffix`, as measured with `sing-box rule-set match` on
/// 1.13.14 (the router is the spec; this table is the measurement):
///
/// ```text
/// entry     z.com  a.z.com  b.a.z.com  xz.com  com  xcom
/// z.com       Y       Y         Y        .      .     .
/// .z.com      .       Y         Y        .      .     .
/// com         Y       Y         Y        Y      Y     .
/// .com        Y       Y         Y        Y      .     .
/// ```
///
/// So: a LABEL BOUNDARY, both forms. A bare entry matches the name itself and
/// anything under it; a dot-led entry matches only what is under it.
///
/// This replaced a bare `dest.ends_with(e)`, which claimed `z.com` covered
/// `xz.com` — the explainer promising a lane the router would not take. The old
/// behaviour was characterized as "matching sing-box's domain_suffix"; it never
/// did, and measuring it is what settled the question.
fn suffix_matches(dest: &str, e: &str) -> bool {
    match e.starts_with('.') {
        true => dest.ends_with(e),
        false => dest == e || dest.ends_with(&format!(".{e}")),
    }
}

/// `case "$e" in */[0-9]*)` — a slash followed somewhere by a digit.
fn looks_like_cidr(e: &str) -> bool {
    match e.split_once('/') {
        Some((_, rest)) => rest.chars().any(|c| c.is_ascii_digit()),
        None => false,
    }
}

/// `_list_hit "$val" "$f" cidr` — first matching CIDR in file order.
pub fn cidr_hit(ip: &str, list: &str) -> Option<String> {
    entries(list).find(|e| looks_like_cidr(e) && cidr_has(ip, e))
}

/// `_private_hit` — first PRIVATE_CIDRS entry containing the address, and only
/// when the private default is corp.
pub fn private_hit(ip: &str, private_cidrs: &[String], private_default: &str) -> Option<String> {
    if private_default != "corp" {
        return None;
    }
    private_cidrs.iter().find(|c| cidr_has(ip, c)).cloned()
}

/// Which kind of hand-list rule matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitKind {
    /// A `domain:<host>` entry — sing-box `domain`, whole-host equality.
    Exact,
    /// A plain entry — sing-box `domain_suffix`.
    Suffix,
}

/// `_longest_domain_hit` — an exact hit if there is one, else the longest suffix
/// ACROSS the three lists.
///
/// **Exact beats every suffix, at any length**, because the render emits the
/// `domain` rules first and sing-box is first-match-wins. Among exacts the first
/// lane seen wins, matching that emission order (escape → corp → block).
///
/// The suffix comparison is `suffix_matches` — a LABEL-BOUNDARY test, matching
/// what sing-box actually does. Ranking stays byte length: for two entries that
/// both boundary-match one host, one is a label-suffix of the other, so longer
/// string and more labels agree — which is what keeps this in step with the
/// render's `sort_by([label count, length])`. Ties keep the FIRST lane seen,
/// because the shell replaces the best only on strictly-greater length.
pub fn longest_domain_hit(dest: &str, escape: &str, corp: &str, block: &str) -> Option<(Lane, HitKind, String)> {
    let mut best: Option<(Lane, String)> = None;
    let mut exact: Option<(Lane, String)> = None;
    for (lane, list) in [(Lane::Escape, escape), (Lane::Corp, corp), (Lane::Block, block)] {
        for e in entries(list) {
            if let Some(host) = e.strip_prefix(crate::render::EXACT_PREFIX) {
                if !host.is_empty() && dest == host && exact.is_none() {
                    exact = Some((lane, host.to_string()));
                }
                continue;
            }
            if looks_like_cidr(&e) || !suffix_matches(dest, &e) {
                continue;
            }
            // Byte length: the shell runs under LC_ALL=C, where ${#e} counts bytes.
            let better = match &best {
                Some((_, b)) => e.len() > b.len(),
                None => true,
            };
            if better {
                best = Some((lane, e));
            }
        }
    }
    exact
        .map(|(l, e)| (l, HitKind::Exact, e))
        .or_else(|| best.map(|(l, e)| (l, HitKind::Suffix, e)))
}

/// Would this destination reach the branch that needs a DNS answer?
///
/// The shell resolves lazily — `resolve_ip` runs only in the `else` after both
/// the IP-literal and the suffix-match branches have missed. A caller that
/// resolved eagerly would still print the right verdict but would dial DNS for
/// destinations the shell never looks up, and `cli-diff` compares the argv
/// trace, so that difference is a failure. Hence the question is asked here,
/// where the branch structure lives, rather than guessed at the call site.
pub fn needs_resolution(raw_dest: &str, escape: &str, corp: &str, block: &str) -> bool {
    let dest = normalize_dest(raw_dest);
    !is_ipv4(&dest) && longest_domain_hit(&dest, escape, corp, block).is_none()
}

pub fn classify(raw_dest: &str, i: &ClassifyInput) -> Classification {
    let dest = normalize_dest(raw_dest);
    let mut ip = String::new();
    let (mut lane, mut why);

    if is_ipv4(&dest) {
        ip = dest.clone();
        if let Some(m) = cidr_hit(&dest, i.corp_list) {
            lane = Lane::Corp;
            why = format!("corp CIDR '{m}'");
        } else if let Some(m) = private_hit(&dest, i.private_cidrs, i.private_default) {
            lane = Lane::Corp;
            why = format!("private/overlay range '{m}' — defaults to unbound (OS routing)");
        } else {
            lane = i.final_route;
            why = format!(
                "no corp CIDR matched; final route = {}",
                i.final_route.as_str()
            );
        }
    } else if let Some((l, k, m)) = longest_domain_hit(&dest, i.escape_list, i.corp_list, i.block_list) {
        lane = l;
        why = match k {
            HitKind::Exact => format!("exact {}-domains match '{m}'", l.as_str()),
            HitKind::Suffix => format!("longest-match {}-domains suffix '{m}'", l.as_str()),
        };
    } else {
        ip = i.resolved_ip.to_string();
        if !ip.is_empty() {
            if let Some(m) = cidr_hit(&ip, i.corp_list) {
                lane = Lane::Corp;
                why = format!("resolves to {ip} — in corp CIDR '{m}'");
            } else if let Some(m) = private_hit(&ip, i.private_cidrs, i.private_default) {
                lane = Lane::Corp;
                why = format!("resolves to {ip} — private/overlay range '{m}' (unbound, OS routing)");
            } else {
                lane = i.final_route;
                why = format!(
                    "no block/corp/escape rule; final route = {}",
                    i.final_route.as_str()
                );
            }
        } else {
            lane = i.final_route;
            why = format!(
                "no block/corp/escape rule; final route = {}",
                i.final_route.as_str()
            );
        }
    }

    if i.local_mode && lane == Lane::Escape {
        lane = Lane::Direct;
        why = format!("{why} — local mode: the escape lane routes direct");
    }

    Classification { dest, ip, lane, why }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(escape: &'a str, corp: &'a str, block: &'a str, priv_cidrs: &'a [String]) -> ClassifyInput<'a> {
        ClassifyInput {
            escape_list: escape,
            corp_list: corp,
            block_list: block,
            private_cidrs: priv_cidrs,
            private_default: "corp",
            final_route: Lane::Direct,
            local_mode: false,
            resolved_ip: "",
        }
    }

    #[test]
    fn urls_are_reduced_to_a_host() {
        assert_eq!(normalize_dest("https://Example.COM:8443/a/b?q=1"), "example.com");
        assert_eq!(normalize_dest("example.com"), "example.com");
    }

    #[test]
    fn suffix_matching_is_on_a_label_boundary() {
        // The router's rule, measured — see `suffix_matches`. This test used to
        // assert the opposite and call it "matching sing-box's domain_suffix";
        // it never did, and `rowt explain` was promising lanes the router would
        // not take.
        let p: Vec<String> = vec![];
        let i = input("example.com\n", "", "", &p);
        assert_eq!(classify("example.com", &i).lane, Lane::Escape);
        assert_eq!(classify("api.example.com", &i).lane, Lane::Escape);
        assert_ne!(classify("xexample.com", &i).lane, Lane::Escape);
        // A dot-led entry covers what is under it, not the apex.
        let d = input(".example.com\n", "", "", &p);
        assert_eq!(classify("api.example.com", &d).lane, Lane::Escape);
        assert_ne!(classify("example.com", &d).lane, Lane::Escape);
        // A bare TLD entry still matches every name under it, `xz.com` included
        // — that name DOES end with `.com`, so this is the boundary rule, not a
        // special case.
        let t = input("com\n", "", "", &p);
        assert_eq!(classify("xz.com", &t).lane, Lane::Escape);
        assert_ne!(classify("xcom", &t).lane, Lane::Escape);
    }

    #[test]
    fn the_longest_suffix_wins_across_lanes() {
        let p: Vec<String> = vec![];
        let i = input("api.foo.com\n", "", "foo.com\n", &p);
        let c = classify("api.foo.com", &i);
        assert_eq!(c.lane, Lane::Escape);
        assert!(c.why.contains("'api.foo.com'"));
        assert_eq!(classify("other.foo.com", &i).lane, Lane::Block);
    }

    #[test]
    fn equal_length_suffixes_break_toward_escape_then_corp() {
        let p: Vec<String> = vec![];
        let i = input("aaa.com\n", "aaa.com\n", "aaa.com\n", &p);
        assert_eq!(classify("x.aaa.com", &i).lane, Lane::Escape);
        let i2 = input("", "bbb.com\n", "bbb.com\n", &p);
        assert_eq!(classify("x.bbb.com", &i2).lane, Lane::Corp);
    }

    #[test]
    fn cidrs_and_the_private_fall_through() {
        let p = vec!["10.0.0.0/8".to_string()];
        let i = input("", "198.51.100.0/24\n", "", &p);
        assert_eq!(classify("198.51.100.7", &i).lane, Lane::Corp);
        // outside the corp CIDR but inside RFC1918 -> still corp
        let c = classify("10.99.0.1", &i);
        assert_eq!(c.lane, Lane::Corp);
        assert!(c.why.contains("private/overlay range '10.0.0.0/8'"));
        assert_eq!(classify("203.0.113.1", &i).lane, Lane::Direct);
    }

    #[test]
    fn private_default_direct_disables_the_fall_through() {
        let p = vec!["10.0.0.0/8".to_string()];
        let mut i = input("", "", "", &p);
        i.private_default = "direct";
        assert_eq!(classify("10.99.0.1", &i).lane, Lane::Direct);
    }

    #[test]
    fn local_mode_reports_direct_for_an_escape_hit() {
        let p: Vec<String> = vec![];
        let mut i = input("example.com\n", "", "", &p);
        i.local_mode = true;
        let c = classify("example.com", &i);
        assert_eq!(c.lane, Lane::Direct);
        assert!(c.why.ends_with("local mode: the escape lane routes direct"));
    }

    #[test]
    fn cidr_boundaries() {
        assert!(cidr_has("10.0.0.0", "10.0.0.0/8"));
        assert!(cidr_has("10.255.255.255", "10.0.0.0/8"));
        assert!(!cidr_has("11.0.0.0", "10.0.0.0/8"));
        assert!(cidr_has("1.2.3.4", "0.0.0.0/0"));
    }

    #[test]
    fn an_exact_entry_beats_every_suffix_at_any_length() {
        let p: Vec<String> = vec![];
        // A LONGER suffix on escape vs a short exact on block: exact still wins,
        // because the render emits `domain` rules first and sing-box takes the
        // first match. Getting this backwards would make `--domain` a no-op.
        let i = input("api.foo.com\n", "", "domain:api.foo.com\n", &p);
        let c = classify("api.foo.com", &i);
        assert_eq!(c.lane, Lane::Block);
        assert_eq!(c.why, "exact block-domains match 'api.foo.com'");
        // …but only for that one host. A subdomain falls back to the suffix.
        assert_eq!(classify("x.api.foo.com", &i).lane, Lane::Escape);
    }

    #[test]
    fn an_exact_entry_does_not_match_subdomains_or_partials() {
        let p: Vec<String> = vec![];
        let i = input("domain:z.com\n", "", "", &p);
        assert_eq!(classify("z.com", &i).lane, Lane::Escape);
        // No suffix behaviour at all — these fall through to the final route.
        assert_ne!(classify("a.z.com", &i).lane, Lane::Escape);
        assert_ne!(classify("xz.com", &i).lane, Lane::Escape);
        // And the marker itself is never treated as a hostname.
        assert_ne!(classify("domain:z.com", &i).lane, Lane::Escape);
    }

    #[test]
    fn among_exacts_the_first_lane_wins_matching_the_render_order() {
        let p: Vec<String> = vec![];
        // Only reachable by hand-editing (`_lane_dedupe` prevents it), but the
        // explainer must still agree with which rule sing-box would hit first.
        let i = input("domain:z.com\n", "domain:z.com\n", "domain:z.com\n", &p);
        assert_eq!(classify("z.com", &i).lane, Lane::Escape);
    }
}
