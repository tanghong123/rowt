//! Which internal/corporate DNS domains does this network advertise?
//!
//! A port of `config/net-detect.py`. Parses `scutil --dns` and reports the
//! search/match domains and the private-range nameservers a corp LAN or a
//! connected corp VPN registers — the raw material for suggesting corp-lane
//! suffixes.
//!
//! It is a SUGGESTION aid, not an auto-apply: a machine can legitimately see
//! many internal domains and only the human knows which belong in the corp lane.
//! And the signal is only observable live — on the corp LAN, or with the VPN up.
//!
//! Pure by construction, exactly as the Python was (`--input FILE` for tests):
//! the caller supplies the text. That is what lets the differential gate replay
//! both implementations over the same fixtures.

/// Nameserver ranges that mean "an internal resolver": RFC1918 + CGNAT +
/// link-local, plus the public-LOOKING /8s that some corporate clouds route
/// privately. Those last three are the reason this is a list and not a call to
/// `is_private()` — 11/8 and 30/8 are globally routable and still internal here.
const INTERNAL_NS: [(&str, u32); 8] = [
    ("10.0.0.0", 8),
    ("172.16.0.0", 12),
    ("192.168.0.0", 16),
    ("100.64.0.0", 10),
    ("169.254.0.0", 16),
    ("11.0.0.0", 8),
    ("30.0.0.0", 8),
    ("6.0.0.0", 12),
];

fn is_internal_ns(ip: &str) -> bool {
    // IPv4 only, and strictly: `ipaddress.ip_address` rejects "1.2.3" and
    // "1.2.3.4.5", where a permissive split would accept both.
    let parts: Vec<&str> = ip.split('.').collect();
    if parts.len() != 4 {
        return false;
    }
    let mut v: u32 = 0;
    for p in &parts {
        if p.is_empty() || p.len() > 3 || !p.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
        match p.parse::<u32>() {
            Ok(o) if o <= 255 => v = (v << 8) | o,
            _ => return false,
        }
    }
    INTERNAL_NS.iter().any(|(net, bits)| {
        let n: u32 = net.split('.').fold(0, |a, o| (a << 8) | o.parse::<u32>().unwrap_or(0));
        let mask: u32 = if *bits == 0 { 0 } else { u32::MAX << (32 - bits) };
        (v & mask) == (n & mask)
    })
}

/// `local`, reverse-DNS zones and Tailscale MagicDNS are noise, not corp signal.
fn skip_domain(d: &str) -> bool {
    let d = d.to_ascii_lowercase();
    d.is_empty()
        || d == "local"
        || d.ends_with(".arpa")
        || d.ends_with(".ts.net")
        || d.contains("tailscale")
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Detected {
    pub internal_domains: Vec<String>,
    pub physical_search: Vec<String>,
    pub corp_nameservers: Vec<String>,
}

/// `if_index : 14 (en0)` — the parenthesised device, and whether it is `en\d+`.
fn parse_if_index(line: &str) -> Option<String> {
    let rest = line.strip_prefix("if_index")?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let rest = rest[digits.len()..].trim_start();
    let inner = rest.strip_prefix('(')?;
    let end = inner.find(')')?;
    Some(inner[..end].to_string())
}

/// `en\d+$` — anchored at BOTH ends, so `en0:1` and `bridge0` are not physical.
fn is_physical(dev: &str) -> bool {
    dev.strip_prefix("en")
        .map(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or(false)
}

/// `(search )?domain(?:\[\d+\])?\s*:\s*(\S+)` → (is_search, value).
fn parse_domain(line: &str) -> Option<(bool, String)> {
    let (is_search, rest) = match line.strip_prefix("search ") {
        Some(r) => (true, r),
        None => (false, line),
    };
    let rest = rest.strip_prefix("domain")?;
    // The optional `[N]` index.
    let rest = match rest.strip_prefix('[') {
        Some(r) => {
            let end = r.find(']')?;
            if r[..end].is_empty() || !r[..end].chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            &r[end + 1..]
        }
        None => rest,
    };
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let val: String = rest.split_whitespace().next()?.to_string();
    Some((is_search, val))
}

fn parse_nameserver(line: &str) -> Option<String> {
    let rest = line.strip_prefix("nameserver")?;
    let rest = match rest.strip_prefix('[') {
        Some(r) => {
            let end = r.find(']')?;
            if r[..end].is_empty() || !r[..end].chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            &r[end + 1..]
        }
        None => rest,
    };
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    Some(rest.split_whitespace().next()?.to_string())
}

pub fn parse(text: &str) -> Detected {
    let mut out = Detected::default();
    let mut seen: Vec<String> = Vec::new();
    let mut ns_seen: Vec<String> = Vec::new();

    // `scutil --dns` prints a resolver's `search domain` BEFORE its `if_index`,
    // so each block is buffered and the NIC it is scoped to is resolved at
    // flush. Parsing forward and tagging on sight would attribute every search
    // domain to the PREVIOUS resolver's interface — the bug the Python's own
    // fixture is built to catch.
    let mut cur_search: Vec<String> = Vec::new();
    let mut cur_phys = false;

    macro_rules! flush {
        () => {
            if cur_phys {
                for d in &cur_search {
                    if !out.physical_search.contains(d) {
                        out.physical_search.push(d.clone());
                    }
                }
            }
        };
    }

    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with("resolver #") {
            flush!();
            cur_search.clear();
            cur_phys = false;
            continue;
        }
        if let Some(dev) = parse_if_index(line) {
            cur_phys = is_physical(&dev);
            continue;
        }
        if let Some((is_search, dom)) = parse_domain(line) {
            let dom = dom.to_ascii_lowercase();
            if !skip_domain(&dom) {
                if !seen.contains(&dom) {
                    seen.push(dom.clone());
                    out.internal_domains.push(dom.clone());
                }
                if is_search {
                    cur_search.push(dom);
                }
            }
            continue;
        }
        if let Some(ip) = parse_nameserver(line) {
            if is_internal_ns(&ip) && !ns_seen.contains(&ip) {
                ns_seen.push(ip.clone());
                out.corp_nameservers.push(ip);
            }
        }
    }
    flush!();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Python's own fixture, verbatim — a resolver block that puts `search
    /// domain` before `if_index`, which is the ordering bug it guards.
    const FIXTURE: &str = "DNS configuration

resolver #1
  search domain[0] : hz.corp.example
  nameserver[0] : 30.1.2.3
  nameserver[1] : fd00:1::1
  if_index : 14 (en0)
  flags    : Request A records, Request AAAA records

resolver #2
  domain   : local
  flags    : Request A records

resolver #3
  domain   : 254.169.in-addr.arpa
  flags    : Request A records

resolver #8
  domain   : corp.example
  nameserver[0] : 223.5.5.5
  flags    : Request A records

resolver #9
  domain   : vpn.corp.example
  nameserver[0] : 10.0.0.53
  flags    : Request A records

resolver #11
  domain   : search.tailscale
  search domain[0] : tail1234.ts.net
  search domain[1] : hz.corp.example
  flags    : Request A records
";

    #[test]
    fn a_search_domain_before_its_if_index_still_lands_on_the_nic() {
        let d = parse(FIXTURE);
        assert_eq!(d.physical_search, ["hz.corp.example"]);
    }

    #[test]
    fn noise_zones_are_dropped_and_order_is_first_seen() {
        let d = parse(FIXTURE);
        assert_eq!(d.internal_domains,
                   ["hz.corp.example", "corp.example", "vpn.corp.example"]);
        // local, *.arpa, *.ts.net and anything containing "tailscale" are noise.
        assert!(!d.internal_domains.iter().any(|x| x.contains("tailscale")));
    }

    #[test]
    fn only_private_looking_nameservers_count() {
        let d = parse(FIXTURE);
        // 30.1.2.3 is globally ROUTABLE and still internal here — some corporate
        // clouds route 11/8 and 30/8 privately, which is why the list is a list.
        assert_eq!(d.corp_nameservers, ["30.1.2.3", "10.0.0.53"]);
        // 223.5.5.5 is public; fd00::/8 is v6 and out of scope.
    }

    #[test]
    fn a_malformed_address_is_not_an_internal_nameserver() {
        assert!(!is_internal_ns("10.0.0"));
        assert!(!is_internal_ns("10.0.0.1.5"));
        assert!(!is_internal_ns("10.0.0.256"));
        assert!(!is_internal_ns("ten.oh.oh.one"));
        assert!(is_internal_ns("10.0.0.1"));
    }

    #[test]
    fn a_sub_interface_is_not_a_physical_nic() {
        assert!(is_physical("en0"));
        assert!(is_physical("en12"));
        assert!(!is_physical("en"));
        assert!(!is_physical("en0:1"));
        assert!(!is_physical("utun4"));
        assert!(!is_physical("bridge0"));
    }

    #[test]
    fn a_resolver_with_no_if_index_contributes_no_search_domain() {
        // resolver #11's search domains must NOT leak onto en0 from resolver #1.
        let d = parse(FIXTURE);
        assert_eq!(d.physical_search.len(), 1);
    }
}
