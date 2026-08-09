//! Which geosite categories cover a domain? — the hint `rowt <lane> add` prints.
//!
//! A port of `config/geosite-lookup.py`. When you add `mail.google.com`, this is
//! what notices that `geosite:google` would cover it and every other Google
//! domain including the ccTLDs, and says so.
//!
//! The IO half — fetching a rule-set, decompiling it with sing-box, globbing the
//! cache — is the caller's (see `rowt-cli`). Everything here is a pure decision
//! over already-loaded category contents, which is what makes the interesting
//! parts testable without a network or a sing-box binary.

/// Labels that identify nothing: TLDs, ccTLDs, and the host prefixes that show
/// up in front of everything. A brand guess of "com" or "www" would fetch a
/// rule-set that cannot exist, on every single `add`.
const GENERIC: [&str; 45] = [
    "com", "org", "net", "io", "co", "me", "app", "dev", "xyz", "tv", "gl", "sk",
    "bz", "la", "vg", "pk", "gd", "sh", "in", "ly", "tt", "mp", "cr", "goo",
    "www", "cdn", "raw", "gist", "mail", "drive", "docs", "chat", "hub", "colab",
    "ne", "or", "research", "static", "cloud", "api", "edu", "gov", "ac", "uk",
    "jp",
];
// `hk` and `cn` are in the Python's set too; kept out of the array above only to
// keep it one screen — see GENERIC_EXTRA.
const GENERIC_EXTRA: [&str; 2] = ["hk", "cn"];

pub const UMBRELLAS: [&str; 12] = [
    "google", "meta", "amazon", "microsoft", "apple", "cloudflare", "akamai",
    "fastly", "netflix", "telegram", "openai", "anthropic",
];

pub const MAX_SHOWN: usize = 6;

pub fn is_generic(label: &str) -> bool {
    GENERIC.contains(&label) || GENERIC_EXTRA.contains(&label)
}

/// One category's contents, as decompiled from its `.srs`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Coverage {
    pub exact: Vec<String>,
    pub suffix: Vec<String>,
}

impl Coverage {
    /// `d in exact or any(d == s or d.endswith("." + s) for s in suf)`.
    ///
    /// Note this is a DOT-BOUNDARY suffix test, unlike the lane lists' bare
    /// `HasSuffix` — so `geosite:google` covering `google.com` does NOT cover
    /// `notgoogle.com`. The two rules differ because they answer different
    /// questions: a rule-set states ownership, a lane entry states a match.
    pub fn covers(&self, d: &str) -> bool {
        self.exact.iter().any(|e| e == d)
            || self.suffix.iter().any(|s| d == s || d.ends_with(&format!(".{s}")))
    }
}

/// Parse a decompiled rule-set. Leading dots are stripped from suffixes, as the
/// Python's `lstrip(".")` does.
pub fn parse_ruleset(json: &serde_json::Value) -> Coverage {
    let mut c = Coverage::default();
    let empty = vec![];
    for r in json.get("rules").and_then(|r| r.as_array()).unwrap_or(&empty) {
        for s in r.get("domain_suffix").and_then(|x| x.as_array()).unwrap_or(&empty) {
            if let Some(s) = s.as_str() {
                let s = s.trim_start_matches('.').to_string();
                if !c.suffix.contains(&s) {
                    c.suffix.push(s);
                }
            }
        }
        for s in r.get("domain").and_then(|x| x.as_array()).unwrap_or(&empty) {
            if let Some(s) = s.as_str() {
                let s = s.to_string();
                if !c.exact.contains(&s) {
                    c.exact.push(s);
                }
            }
        }
    }
    c
}

/// The brand guesses for a domain: its SLD, then the label before it, skipping
/// generics — `mail.google.com` guesses "google", `www.bbc.co.uk` guesses "bbc"
/// (because "co" and "uk" are both generic).
pub fn brands(domain: &str) -> Vec<String> {
    let labels: Vec<&str> = domain.split('.').collect();
    let n = labels.len();
    let mut out: Vec<String> = Vec::new();
    // `dict.fromkeys([...])` — de-duped, insertion-ordered, and the empty
    // string it can contain is filtered out afterwards.
    for cand in [
        if n >= 2 { labels[n - 2] } else { "" },
        if n >= 3 { labels[n - 3] } else { "" },
    ] {
        if cand.is_empty() || is_generic(cand) {
            continue;
        }
        let c = cand.to_string();
        if !out.contains(&c) {
            out.push(c);
        }
    }
    out
}

/// The normalisation `main()` applies before anything else, and the cases where
/// it declines to answer at all: no dot, or an input that is already a
/// `geosite:` reference.
pub fn normalize(input: &str) -> Option<String> {
    let d = input.trim().to_ascii_lowercase();
    let d = d.trim_end_matches('.').to_string();
    if d.is_empty() || !d.contains('.') || d.starts_with("geosite:") {
        return None;
    }
    Some(d)
}

/// The order categories are tried in: brand first (most relevant), then whatever
/// is already cached (free — no fetch), then the umbrellas. De-duped, and
/// anything already on the lane is skipped because it would be a no-op suggestion.
///
/// Returns `(name, allow_fetch)`: only brand and umbrella guesses may reach the
/// network, so a plain `add` stays offline-safe when the cache is cold.
pub fn candidates(domain: &str, cached: &[String], have: &[String]) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let brand = brands(domain);
    let seq = brand.iter().map(|b| (b.clone(), true))
        .chain(cached.iter().map(|c| (c.clone(), false)))
        .chain(UMBRELLAS.iter().map(|u| (u.to_string(), true)));
    for (name, allow) in seq {
        if seen.contains(&name) || have.contains(&name) {
            continue;
        }
        seen.push(name.clone());
        out.push((name, allow));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_brand_guess_skips_generic_labels() {
        assert_eq!(brands("mail.google.com"), ["google"]);
        // co and uk are both generic, so the guess walks past them.
        assert_eq!(brands("www.bbc.co.uk"), ["bbc"]);
        assert_eq!(brands("a.b.example.com"), ["example", "b"]);
        // Nothing usable: every candidate label is generic.
        assert!(brands("www.com").is_empty());
    }

    #[test]
    fn coverage_is_a_dot_boundary_test_unlike_the_lane_lists() {
        let c = Coverage { exact: vec!["exact.example".into()], suffix: vec!["google.com".into()] };
        assert!(c.covers("google.com"));
        assert!(c.covers("mail.google.com"));
        assert!(c.covers("exact.example"));
        // The difference that matters: a lane entry would match this, a
        // rule-set does not.
        assert!(!c.covers("notgoogle.com"));
        assert!(!c.covers("google.com.evil.example"));
    }

    #[test]
    fn a_leading_dot_on_a_suffix_is_stripped() {
        let v = serde_json::json!({"rules": [{"domain_suffix": [".google.com"], "domain": ["g.co"]}]});
        let c = parse_ruleset(&v);
        assert_eq!(c.suffix, ["google.com"]);
        assert!(c.covers("mail.google.com"));
        assert!(c.covers("g.co"));
    }

    #[test]
    fn normalize_declines_what_cannot_be_a_domain() {
        assert_eq!(normalize("  Mail.Google.COM. ").as_deref(), Some("mail.google.com"));
        assert_eq!(normalize("localhost"), None);       // no dot
        assert_eq!(normalize("geosite:google"), None);  // already a category
        assert_eq!(normalize(""), None);
    }

    #[test]
    fn candidates_are_brand_then_cached_then_umbrella_and_never_repeat() {
        let cached = vec!["apple".to_string(), "zzz".to_string()];
        let have = vec!["zzz".to_string()];
        let c = candidates("mail.google.com", &cached, &have);
        let names: Vec<&str> = c.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names[0], "google", "the brand guess comes first");
        assert_eq!(names[1], "apple", "then what is already cached — no fetch");
        assert!(!names.contains(&"zzz"), "a category already on the lane is not re-suggested");
        // google appears as both a brand and an umbrella; it must be tried once.
        assert_eq!(names.iter().filter(|n| **n == "google").count(), 1);
        // Only brand and umbrella guesses may fetch.
        assert!(c[0].1, "brand may fetch");
        assert!(!c[1].1, "a cached set is read, never fetched");
    }
}
