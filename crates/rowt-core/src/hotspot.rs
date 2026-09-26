//! The hotspot lane — captive-portal hosts that must reach the network WITHOUT
//! the system proxy, so a venue's login page can load while rowt is up.
//!
//! Not a routing lane: nothing here is rendered. The lane's whole effect is on
//! macOS's proxy bypass list (`_proxy_bypass_want`) and on the CLI-channel
//! `no_proxy` that `proxy env` exports. Both shapes are derived here, from the
//! lane file's contents, so the two implementations cannot disagree on them.

use crate::render::{is_cidr, parse_list, Filter};

/// `_hotspot_bypass_entries` — the lane as macOS bypass entries.
///
/// macOS matches a bare name EXACTLY and needs the `*.` form for its
/// subdomains, so a plain entry becomes both; a dot-led entry (`.x`, or `*.x`
/// as `parse_list` reads it) means the subdomains alone, so it is the `*.` form
/// alone; a `domain:` (exact) entry is the host alone; an address or CIDR
/// passes as written. Lowercased and byte-sorted: the setter hands the list to
/// `networksetup` in this order, and the argv trace is compared across
/// implementations.
pub fn bypass_entries(contents: &str) -> Vec<String> {
    // Lowercased BEFORE the `domain:` / `geosite:` prefixes are read, as the
    // shell's `tr` runs ahead of its `grep`/`awk` — so `DOMAIN:x` is exact too.
    let contents = contents.to_ascii_lowercase();
    let mut v: Vec<String> = parse_list(&contents, Filter::Exact);
    for e in parse_list(&contents, Filter::All) {
        // `.x` → `*.x`. It used to become `.x` and `*..x`, neither of which
        // macOS matches against a real name.
        if e.len() > 1 && e.starts_with('.') {
            v.push(format!("*{e}"));
            continue;
        }
        if !is_address(&e) {
            v.push(format!("*.{e}"));
        }
        v.push(e);
    }
    v.sort();
    v.dedup();
    v
}

/// An IPv4 address or a CIDR — `/^[0-9.]+$/` and `/\/[0-9]+$/` in the shell.
fn is_address(s: &str) -> bool {
    is_cidr(s) || (!s.is_empty() && s.chars().all(|c| c.is_ascii_digit() || c == '.'))
}

/// The `no_proxy` value for the CLI channel — `_proxy_env_noproxy`.
///
/// curl, Go and Python read `no_proxy`, not the macOS list, and their grammar
/// differs from it: a leading dot is the subdomain form (`*.local` matches
/// nothing in any of them), and a CIDR needs all four octets (`169.254/16` is
/// not one). Everything else passes as written. Comma-joined in list order,
/// duplicates dropped.
pub fn no_proxy(bypass: &[String]) -> String {
    let mut v: Vec<String> = Vec::new();
    for e in bypass {
        let n = no_proxy_entry(e);
        if !v.contains(&n) {
            v.push(n);
        }
    }
    v.join(",")
}

fn no_proxy_entry(e: &str) -> String {
    if let Some(rest) = e.strip_prefix("*.") {
        return format!(".{rest}");
    }
    if let Some((addr, bits)) = e.rsplit_once('/') {
        if is_cidr(e) && addr.chars().all(|c| c.is_ascii_digit() || c == '.') {
            let n = addr.split('.').count();
            if n < 4 {
                return format!("{addr}{}/{bits}", ".0".repeat(4 - n));
            }
        }
    }
    e.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_entry_becomes_the_host_and_its_wildcard() {
        let v = bypass_entries("# hdr\nPortal.Example\n");
        assert_eq!(v, vec!["*.portal.example", "portal.example"]);
    }

    #[test]
    fn exact_entries_addresses_and_cidrs_pass_as_one_line() {
        let v = bypass_entries("domain:Login.Hotspot.Example\n203.0.113.0/24\n198.51.100.7\ngeosite:ads\n");
        assert_eq!(v, vec!["198.51.100.7", "203.0.113.0/24", "login.hotspot.example"]);
    }

    #[test]
    fn the_wildcard_of_one_entry_and_the_exact_of_another_dedupe() {
        // `portal.example` yields `portal.example` twice (plain + exact): once.
        let v = bypass_entries("portal.example\ndomain:portal.example\n");
        assert_eq!(v, vec!["*.portal.example", "portal.example"]);
    }

    #[test]
    fn a_dot_led_or_wildcard_entry_is_the_subdomains_alone() {
        let v = bypass_entries(".Dot.example\n*.wild.example\n");
        assert_eq!(v, vec!["*.dot.example", "*.wild.example"]);
        // A lone dot keeps its old treatment (all dots and digits: an address).
        assert_eq!(bypass_entries(".\n"), vec!["."]);
    }

    #[test]
    fn byte_order_puts_the_wildcards_first() {
        // `*` (0x2A) sorts before digits and letters under LC_ALL=C.
        let v = bypass_entries("zeta.example\n1.2.3.4\n");
        assert_eq!(v, vec!["*.zeta.example", "1.2.3.4", "zeta.example"]);
    }

    #[test]
    fn no_proxy_speaks_the_cli_grammar() {
        let want: Vec<String> = ["*.local", "169.254/16", "127.0.0.1", "localhost", "10.0.0.0/8",
                                 "captive.apple.com", "*.portal.example", "portal.example"]
            .iter().map(|s| s.to_string()).collect();
        assert_eq!(no_proxy(&want),
                   ".local,169.254.0.0/16,127.0.0.1,localhost,10.0.0.0/8,captive.apple.com,.portal.example,portal.example");
    }
}
