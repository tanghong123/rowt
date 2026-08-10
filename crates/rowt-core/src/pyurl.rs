//! The corner of Python's stdlib that `vless-parse.py` stands on: `urlsplit`,
//! `parse_qs` and `unquote`.
//!
//! Not a URL library. A share link is a URI only by convention — the userinfo
//! carries a UUID or a password, the fragment carries a display name in whatever
//! encoding the provider felt like — so what matters is not "correct" parsing but
//! parsing *the same way the Python did*, including where that is surprising:
//!
//!   * `_first` unquotes a value `parse_qs` already unquoted, so `%2520` arrives
//!     as `%20`, and `+` means space.
//!   * a blank query value (`sni=`) is dropped by `parse_qs`, so it reads as
//!     absent and the caller's default applies.
//!   * `u.port or 443` — port 0 is falsy, so `:0` silently becomes 443.
//!   * the userinfo is split at the LAST `@` and the FIRST `:`.
//!
//! Every one of those is load-bearing for links real providers hand out.

/// `_WHATWG_C0_CONTROL_OR_SPACE` — what `urlsplit` lstrips off the front.
fn c0_or_space(c: char) -> bool {
    c <= '\u{20}'
}

/// The result of `urlsplit`, minus the parts nothing here reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Split {
    pub scheme: String,
    pub netloc: String,
    pub path: String,
    pub query: String,
    pub fragment: String,
}

/// `urlsplit(url)`.
///
/// Errors are Python's ValueError text, verbatim: they reach the user through
/// `parse_many`'s `warning: skipping a link (…)`.
pub fn urlsplit(url: &str) -> Result<Split, String> {
    let mut url: String = url.trim_start_matches(c0_or_space).to_string();
    for b in ['\t', '\r', '\n'] {
        url = url.replace(b, "");
    }

    let mut scheme = String::new();
    if let Some(i) = url.find(':') {
        if i > 0 {
            let head = &url[..i];
            let valid = head.starts_with(|c: char| c.is_ascii_alphabetic())
                && head.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
            if valid {
                scheme = head.to_ascii_lowercase();
                url = url[i + 1..].to_string();
            }
        }
    }

    let mut netloc = String::new();
    if url.starts_with("//") {
        // `_splitnetloc` — the netloc ends at the first of `/?#`, not the last.
        let delim = ['/', '?', '#']
            .iter()
            .filter_map(|c| url[2..].find(*c).map(|i| i + 2))
            .min()
            .unwrap_or(url.len());
        netloc = url[2..delim].to_string();
        url = url[delim..].to_string();

        let (open, close) = (netloc.contains('['), netloc.contains(']'));
        if open != close {
            return Err("Invalid IPv6 URL".into());
        }
        if open && close {
            let inner = netloc.split_once('[').map(|(_, r)| r).unwrap_or("");
            let host = inner.split_once(']').map(|(l, _)| l).unwrap_or(inner);
            check_bracketed_host(host)?;
        }
    }

    let mut fragment = String::new();
    if let Some((l, r)) = url.split_once('#') {
        fragment = r.to_string();
        url = l.to_string();
    }
    let mut query = String::new();
    if let Some((l, r)) = url.split_once('?') {
        query = r.to_string();
        url = l.to_string();
    }

    checknetloc(&netloc)?;
    Ok(Split { scheme, netloc, path: url, query, fragment })
}

/// The schemes `urlunsplit` will invent a `//` for when it has no netloc but the
/// path is empty or absolute. Without this list `https:?a=1` and `https://?a=1`
/// are the same input rendered two ways, and a subscription would dedup wrong.
const USES_NETLOC: [&str; 26] = [
    "", "ftp", "http", "gopher", "nntp", "telnet", "imap", "wais", "file", "mms", "https",
    "shttp", "snews", "prospero", "rtsp", "rtspu", "rsync", "svn", "svn+ssh", "sftp", "nfs",
    "git", "git+ssh", "ws", "wss", "itms-services",
];

/// `urlunsplit((scheme, netloc, path, query, fragment))`.
pub fn urlunsplit(scheme: &str, netloc: &str, path: &str, query: &str, fragment: &str) -> String {
    let mut url = path.to_string();
    if !netloc.is_empty() {
        if !url.is_empty() && !url.starts_with('/') {
            url = format!("/{url}");
        }
        url = format!("//{netloc}{url}");
    } else if url.starts_with("//") {
        url = format!("//{url}");
    } else if !scheme.is_empty()
        && USES_NETLOC.contains(&scheme)
        && (url.is_empty() || url.starts_with('/'))
    {
        url = format!("//{url}");
    }
    if !scheme.is_empty() {
        url = format!("{scheme}:{url}");
    }
    if !query.is_empty() {
        url = format!("{url}?{query}");
    }
    if !fragment.is_empty() {
        url = format!("{url}#{fragment}");
    }
    url
}

/// `quote_plus(s, safe="")` — everything outside the always-safe set becomes
/// `%XX` with UPPERCASE hex, and a space becomes `+`. (Decoding accepts either
/// case; encoding only ever emits upper.)
fn quote_plus(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `quote(s, safe="/")` — the OTHER encoder. It differs from `quote_plus` in
/// two ways that both show up in a share link: a space becomes `%20` rather than
/// `+`, and the characters in `safe` survive — so a `/` inside a node name or a
/// password is left alone.
///
/// `foreign-import.py` builds its URIs with this one and its query strings with
/// `urlencode` (i.e. `quote_plus`), in the same f-string. Using either for both
/// produces a link that still parses and still connects, with a different
/// password.
pub fn quote(s: &str, safe: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        // A non-ASCII byte in `safe` is dropped by Python (`safe.encode('ascii',
        // 'ignore')`), so only ASCII members of it can keep a byte literal.
        let kept = matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~')
            || (b.is_ascii() && safe.as_bytes().contains(&b));
        if kept {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `urlencode(pairs)`.
pub fn urlencode(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", quote_plus(k), quote_plus(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// `_check_bracketed_host` — what goes in brackets must be an IPv6 literal.
fn check_bracketed_host(host: &str) -> Result<(), String> {
    if let Some(rest) = host.strip_prefix('v') {
        // `\Av[a-fA-F0-9]+\..+\Z`
        let ok = match rest.split_once('.') {
            Some((hex, tail)) => {
                !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()) && !tail.is_empty()
            }
            None => false,
        };
        return if ok { Ok(()) } else { Err("IPvFuture address is invalid".into()) };
    }
    // Python's `ipaddress.ip_address` accepts a scoped literal (`fe80::1%en0`);
    // Rust's parser does not, so the zone is split off before the check.
    let bare = host.split_once('%').map_or(host, |(h, _)| h);
    match bare.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V6(_)) => Ok(()),
        Ok(std::net::IpAddr::V4(_)) => Err("An IPv4 address cannot be in brackets".into()),
        Err(_) => Err(format!("{} does not appear to be an IPv4 or IPv6 address", repr(host))),
    }
}

/// `_checknetloc` — reject a netloc that NFKC normalization would turn into one
/// containing a URL delimiter, because IDNA will normalize it later and the host
/// you validated would not be the host you connect to.
///
/// Done without a normalization table, on two facts:
///
///   * compatibility decomposition is defined per code point, so an ASCII
///     delimiter can only appear in the normalized form if it was already there
///     or if one of `DELIM_PRODUCERS` was;
///   * composition never produces ASCII.
///
/// So "does netloc2 contain a delimiter" is exact. Only "did NFKC change
/// anything" is approximate — `NFKC_UNSTABLE` also lists every combining mark,
/// including ones that will not actually compose with what precedes them. The
/// approximation can only reject a netloc Python would have accepted, never the
/// reverse, which is the safe direction for something that names a server you
/// are about to send credentials to.
fn checknetloc(netloc: &str) -> Result<(), String> {
    if netloc.is_empty() || netloc.is_ascii() {
        return Ok(());
    }
    if !netloc.chars().any(|c| unstable(c as u32)) {
        return Ok(());
    }
    let delim = netloc
        .chars()
        .any(|c| matches!(c, '/' | '?' | '#' | '@' | ':') || DELIM_PRODUCERS.contains(&(c as u32)));
    if delim {
        return Err(format!(
            "netloc '{netloc}' contains invalid characters under NFKC normalization"
        ));
    }
    Ok(())
}

fn unstable(cp: u32) -> bool {
    NFKC_UNSTABLE.binary_search_by(|&(lo, hi)| {
        if cp < lo {
            std::cmp::Ordering::Greater
        } else if cp > hi {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Equal
        }
    })
    .is_ok()
}

impl Split {
    /// `SplitResult.username` — `None` when the netloc has no `@` at all.
    /// The userinfo is everything before the LAST `@`; the name is everything
    /// before the FIRST `:` of that.
    pub fn username(&self) -> Option<&str> {
        let (userinfo, _) = self.netloc.rsplit_once('@')?;
        Some(userinfo.split_once(':').map_or(userinfo, |(u, _)| u))
    }

    /// `(hostname, port)` before either is interpreted — `_hostinfo`.
    fn hostinfo(&self) -> (&str, Option<&str>) {
        let hostinfo = self.netloc.rsplit_once('@').map_or(&self.netloc[..], |(_, h)| h);
        let (host, port) = match hostinfo.split_once('[') {
            Some((_, bracketed)) => {
                let (h, after) = bracketed.split_once(']').unwrap_or((bracketed, ""));
                (h, after.split_once(':').map(|(_, p)| p).unwrap_or(""))
            }
            None => match hostinfo.split_once(':') {
                Some((h, p)) => (h, p),
                None => (hostinfo, ""),
            },
        };
        (host, (!port.is_empty()).then_some(port))
    }

    /// `SplitResult.hostname` — lowercased, except for an IPv6 zone id.
    pub fn hostname(&self) -> Option<String> {
        let (host, _) = self.hostinfo();
        if host.is_empty() {
            return None;
        }
        Some(match host.split_once('%') {
            Some((h, zone)) => format!("{}%{zone}", h.to_lowercase()),
            None => host.to_lowercase(),
        })
    }

    /// `SplitResult.port`. Both failures are ValueErrors the caller reports.
    pub fn port(&self) -> Result<Option<u32>, String> {
        let Some(p) = self.hostinfo().1 else { return Ok(None) };
        if !p.chars().all(|c| c.is_ascii_digit()) {
            return Err(format!("Port could not be cast to integer value as {}", repr(p)));
        }
        // Python's int is unbounded, so an absurdly long run of digits is a
        // range error, not a parse error.
        match p.parse::<u128>() {
            Ok(n) if n <= 65535 => Ok(Some(n as u32)),
            _ => Err("Port out of range 0-65535".into()),
        }
    }
}

/// `repr()` of a str, for the two error messages that interpolate one.
///
/// ASCII is exact. A non-ASCII char is passed through, which is what Python does
/// for every printable one; an unprintable one would be escaped there and is not
/// here — a divergence confined to error text for a port or a bracketed host.
pub fn repr(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

/// `unquote(string)` — percent-decoding, UTF-8, `errors="replace"`.
///
/// Python splits the input into ASCII and non-ASCII runs and only decodes the
/// ASCII ones. Since `%` is ASCII an escape can never straddle a run, so the
/// only visible effect is that a stray non-ASCII byte sequence passes through
/// untouched instead of being folded into the surrounding decode.
pub fn unquote(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while !rest.is_empty() {
        let ascii_len = rest.find(|c: char| !c.is_ascii()).unwrap_or(rest.len());
        if ascii_len > 0 {
            out.push_str(&String::from_utf8_lossy(&unquote_to_bytes(&rest[..ascii_len])));
            rest = &rest[ascii_len..];
        }
        let raw_len = rest.find(|c: char| c.is_ascii()).unwrap_or(rest.len());
        out.push_str(&rest[..raw_len]);
        rest = &rest[raw_len..];
    }
    out
}

/// `unquote_to_bytes` — split on `%`, and a piece that does not start with two
/// hex digits keeps its literal `%`.
fn unquote_to_bytes(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut parts = b.split(|&c| c == b'%');
    let mut out: Vec<u8> = parts.next().unwrap_or(&[]).to_vec();
    for item in parts {
        let hex = |c: u8| (c as char).to_digit(16);
        match (item.first().copied().and_then(hex), item.get(1).copied().and_then(hex)) {
            (Some(hi), Some(lo)) => {
                out.push((hi * 16 + lo) as u8);
                out.extend_from_slice(&item[2..]);
            }
            _ => {
                out.push(b'%');
                out.extend_from_slice(item);
            }
        }
    }
    out
}

/// `parse_qsl(query, keep_blank_values=…)` — the ordered pair list.
///
/// With `keep_blank` false (what `vless-parse` uses) a field with an empty
/// value is DROPPED, so `sni=` reads as absent. With it true (what
/// `import-merge` uses) the pair survives, and so does a field with no `=` at
/// all — which then re-encodes as `flag=`, gaining an equals sign it never had.
pub fn parse_qsl(query: &str, keep_blank: bool) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for field in query.split('&') {
        if field.is_empty() {
            continue;
        }
        let (name, value) = match field.split_once('=') {
            Some((n, v)) => (n, v),
            None if keep_blank => (field, ""),
            None => continue,
        };
        if value.is_empty() && !keep_blank {
            continue;
        }
        out.push((unquote(&name.replace('+', " ")), unquote(&value.replace('+', " "))));
    }
    out
}

/// `parse_qs(query)` with `vless-parse`'s defaults, grouped by first-seen key.
pub fn parse_qs(query: &str) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for (name, value) in parse_qsl(query, false) {
        match out.iter_mut().find(|(k, _)| *k == name) {
            Some((_, v)) => v.push(value),
            None => out.push((name, vec![value])),
        }
    }
    out
}

/// `_first(qs, key, default)`. Note the second `unquote`: `parse_qs` already
/// decoded the value, so a doubly-escaped one decodes twice.
pub fn first(qs: &[(String, Vec<String>)], key: &str, default: &str) -> String {
    match qs.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.first()) {
        Some(v) => unquote(v),
        None => default.to_string(),
    }
}

/// The code points whose compatibility decomposition contains a URL delimiter.
const DELIM_PRODUCERS: [u32; 19] = [
    0x2047, 0x2048, 0x2049, 0x2100, 0x2101, 0x2105, 0x2106, 0x2a74, 0xfe13, 0xfe16, 0xfe55, 0xfe56,
    0xfe5f, 0xfe6b, 0xff03, 0xff0f, 0xff1a, 0xff1f, 0xff20,
];

// Every code point that NFKC changes, plus every combining mark (which may
// compose with what precedes it). Unicode 16.0.0. Regenerate with:
//
//   for cp in range(0x80, 0x110000):
//       c = chr(cp)
//       if unicodedata.normalize("NFKC", c) != c or unicodedata.combining(c): ...
#[rustfmt::skip]
const NFKC_UNSTABLE: [(u32, u32); 437] = [
    (0x00a0,0x00a0), (0x00a8,0x00a8), (0x00aa,0x00aa), (0x00af,0x00af), (0x00b2,0x00b5),
    (0x00b8,0x00ba), (0x00bc,0x00be), (0x0132,0x0133), (0x013f,0x0140), (0x0149,0x0149),
    (0x017f,0x017f), (0x01c4,0x01cc), (0x01f1,0x01f3), (0x02b0,0x02b8), (0x02d8,0x02dd),
    (0x02e0,0x02e4), (0x0300,0x034e), (0x0350,0x036f), (0x0374,0x0374), (0x037a,0x037a),
    (0x037e,0x037e), (0x0384,0x0385), (0x0387,0x0387), (0x03d0,0x03d6), (0x03f0,0x03f2),
    (0x03f4,0x03f5), (0x03f9,0x03f9), (0x0483,0x0487), (0x0587,0x0587), (0x0591,0x05bd),
    (0x05bf,0x05bf), (0x05c1,0x05c2), (0x05c4,0x05c5), (0x05c7,0x05c7), (0x0610,0x061a),
    (0x064b,0x065f), (0x0670,0x0670), (0x0675,0x0678), (0x06d6,0x06dc), (0x06df,0x06e4),
    (0x06e7,0x06e8), (0x06ea,0x06ed), (0x0711,0x0711), (0x0730,0x074a), (0x07eb,0x07f3),
    (0x07fd,0x07fd), (0x0816,0x0819), (0x081b,0x0823), (0x0825,0x0827), (0x0829,0x082d),
    (0x0859,0x085b), (0x0897,0x089f), (0x08ca,0x08e1), (0x08e3,0x08ff), (0x093c,0x093c),
    (0x094d,0x094d), (0x0951,0x0954), (0x0958,0x095f), (0x09bc,0x09bc), (0x09cd,0x09cd),
    (0x09dc,0x09dd), (0x09df,0x09df), (0x09fe,0x09fe), (0x0a33,0x0a33), (0x0a36,0x0a36),
    (0x0a3c,0x0a3c), (0x0a4d,0x0a4d), (0x0a59,0x0a5b), (0x0a5e,0x0a5e), (0x0abc,0x0abc),
    (0x0acd,0x0acd), (0x0b3c,0x0b3c), (0x0b4d,0x0b4d), (0x0b5c,0x0b5d), (0x0bcd,0x0bcd),
    (0x0c3c,0x0c3c), (0x0c4d,0x0c4d), (0x0c55,0x0c56), (0x0cbc,0x0cbc), (0x0ccd,0x0ccd),
    (0x0d3b,0x0d3c), (0x0d4d,0x0d4d), (0x0dca,0x0dca), (0x0e33,0x0e33), (0x0e38,0x0e3a),
    (0x0e48,0x0e4b), (0x0eb3,0x0eb3), (0x0eb8,0x0eba), (0x0ec8,0x0ecb), (0x0edc,0x0edd),
    (0x0f0c,0x0f0c), (0x0f18,0x0f19), (0x0f35,0x0f35), (0x0f37,0x0f37), (0x0f39,0x0f39),
    (0x0f43,0x0f43), (0x0f4d,0x0f4d), (0x0f52,0x0f52), (0x0f57,0x0f57), (0x0f5c,0x0f5c),
    (0x0f69,0x0f69), (0x0f71,0x0f7d), (0x0f80,0x0f84), (0x0f86,0x0f87), (0x0f93,0x0f93),
    (0x0f9d,0x0f9d), (0x0fa2,0x0fa2), (0x0fa7,0x0fa7), (0x0fac,0x0fac), (0x0fb9,0x0fb9),
    (0x0fc6,0x0fc6), (0x1037,0x1037), (0x1039,0x103a), (0x108d,0x108d), (0x10fc,0x10fc),
    (0x135d,0x135f), (0x1714,0x1715), (0x1734,0x1734), (0x17d2,0x17d2), (0x17dd,0x17dd),
    (0x18a9,0x18a9), (0x1939,0x193b), (0x1a17,0x1a18), (0x1a60,0x1a60), (0x1a75,0x1a7c),
    (0x1a7f,0x1a7f), (0x1ab0,0x1abd), (0x1abf,0x1ace), (0x1b34,0x1b34), (0x1b44,0x1b44),
    (0x1b6b,0x1b73), (0x1baa,0x1bab), (0x1be6,0x1be6), (0x1bf2,0x1bf3), (0x1c37,0x1c37),
    (0x1cd0,0x1cd2), (0x1cd4,0x1ce0), (0x1ce2,0x1ce8), (0x1ced,0x1ced), (0x1cf4,0x1cf4),
    (0x1cf8,0x1cf9), (0x1d2c,0x1d2e), (0x1d30,0x1d3a), (0x1d3c,0x1d4d), (0x1d4f,0x1d6a),
    (0x1d78,0x1d78), (0x1d9b,0x1dff), (0x1e9a,0x1e9b), (0x1f71,0x1f71), (0x1f73,0x1f73),
    (0x1f75,0x1f75), (0x1f77,0x1f77), (0x1f79,0x1f79), (0x1f7b,0x1f7b), (0x1f7d,0x1f7d),
    (0x1fbb,0x1fbb), (0x1fbd,0x1fc1), (0x1fc9,0x1fc9), (0x1fcb,0x1fcb), (0x1fcd,0x1fcf),
    (0x1fd3,0x1fd3), (0x1fdb,0x1fdb), (0x1fdd,0x1fdf), (0x1fe3,0x1fe3), (0x1feb,0x1feb),
    (0x1fed,0x1fef), (0x1ff9,0x1ff9), (0x1ffb,0x1ffb), (0x1ffd,0x1ffe), (0x2000,0x200a),
    (0x2011,0x2011), (0x2017,0x2017), (0x2024,0x2026), (0x202f,0x202f), (0x2033,0x2034),
    (0x2036,0x2037), (0x203c,0x203c), (0x203e,0x203e), (0x2047,0x2049), (0x2057,0x2057),
    (0x205f,0x205f), (0x2070,0x2071), (0x2074,0x208e), (0x2090,0x209c), (0x20a8,0x20a8),
    (0x20d0,0x20dc), (0x20e1,0x20e1), (0x20e5,0x20f0), (0x2100,0x2103), (0x2105,0x2107),
    (0x2109,0x2113), (0x2115,0x2116), (0x2119,0x211d), (0x2120,0x2122), (0x2124,0x2124),
    (0x2126,0x2126), (0x2128,0x2128), (0x212a,0x212d), (0x212f,0x2131), (0x2133,0x2139),
    (0x213b,0x2140), (0x2145,0x2149), (0x2150,0x217f), (0x2189,0x2189), (0x222c,0x222d),
    (0x222f,0x2230), (0x2329,0x232a), (0x2460,0x24ea), (0x2a0c,0x2a0c), (0x2a74,0x2a76),
    (0x2adc,0x2adc), (0x2c7c,0x2c7d), (0x2cef,0x2cf1), (0x2d6f,0x2d6f), (0x2d7f,0x2d7f),
    (0x2de0,0x2dff), (0x2e9f,0x2e9f), (0x2ef3,0x2ef3), (0x2f00,0x2fd5), (0x3000,0x3000),
    (0x302a,0x302f), (0x3036,0x3036), (0x3038,0x303a), (0x3099,0x309c), (0x309f,0x309f),
    (0x30ff,0x30ff), (0x3131,0x318e), (0x3192,0x319f), (0x3200,0x321e), (0x3220,0x3247),
    (0x3250,0x327e), (0x3280,0x33ff), (0xa66f,0xa66f), (0xa674,0xa67d), (0xa69c,0xa69f),
    (0xa6f0,0xa6f1), (0xa770,0xa770), (0xa7f2,0xa7f4), (0xa7f8,0xa7f9), (0xa806,0xa806),
    (0xa82c,0xa82c), (0xa8c4,0xa8c4), (0xa8e0,0xa8f1), (0xa92b,0xa92d), (0xa953,0xa953),
    (0xa9b3,0xa9b3), (0xa9c0,0xa9c0), (0xaab0,0xaab0), (0xaab2,0xaab4), (0xaab7,0xaab8),
    (0xaabe,0xaabf), (0xaac1,0xaac1), (0xaaf6,0xaaf6), (0xab5c,0xab5f), (0xab69,0xab69),
    (0xabed,0xabed), (0xf900,0xfa0d), (0xfa10,0xfa10), (0xfa12,0xfa12), (0xfa15,0xfa1e),
    (0xfa20,0xfa20), (0xfa22,0xfa22), (0xfa25,0xfa26), (0xfa2a,0xfa6d), (0xfa70,0xfad9),
    (0xfb00,0xfb06), (0xfb13,0xfb17), (0xfb1d,0xfb36), (0xfb38,0xfb3c), (0xfb3e,0xfb3e),
    (0xfb40,0xfb41), (0xfb43,0xfb44), (0xfb46,0xfbb1), (0xfbd3,0xfd3d), (0xfd50,0xfd8f),
    (0xfd92,0xfdc7), (0xfdf0,0xfdfc), (0xfe10,0xfe19), (0xfe20,0xfe44), (0xfe47,0xfe52),
    (0xfe54,0xfe66), (0xfe68,0xfe6b), (0xfe70,0xfe72), (0xfe74,0xfe74), (0xfe76,0xfefc),
    (0xff01,0xffbe), (0xffc2,0xffc7), (0xffca,0xffcf), (0xffd2,0xffd7), (0xffda,0xffdc),
    (0xffe0,0xffe6), (0xffe8,0xffee), (0x101fd,0x101fd), (0x102e0,0x102e0), (0x10376,0x1037a),
    (0x10781,0x10785), (0x10787,0x107b0), (0x107b2,0x107ba), (0x10a0d,0x10a0d), (0x10a0f,0x10a0f),
    (0x10a38,0x10a3a), (0x10a3f,0x10a3f), (0x10ae5,0x10ae6), (0x10d24,0x10d27), (0x10d69,0x10d6d),
    (0x10eab,0x10eac), (0x10efd,0x10eff), (0x10f46,0x10f50), (0x10f82,0x10f85), (0x11046,0x11046),
    (0x11070,0x11070), (0x1107f,0x1107f), (0x110b9,0x110ba), (0x11100,0x11102), (0x11133,0x11134),
    (0x11173,0x11173), (0x111c0,0x111c0), (0x111ca,0x111ca), (0x11235,0x11236), (0x112e9,0x112ea),
    (0x1133b,0x1133c), (0x1134d,0x1134d), (0x11366,0x1136c), (0x11370,0x11374), (0x113ce,0x113d0),
    (0x11442,0x11442), (0x11446,0x11446), (0x1145e,0x1145e), (0x114c2,0x114c3), (0x115bf,0x115c0),
    (0x1163f,0x1163f), (0x116b6,0x116b7), (0x1172b,0x1172b), (0x11839,0x1183a), (0x1193d,0x1193e),
    (0x11943,0x11943), (0x119e0,0x119e0), (0x11a34,0x11a34), (0x11a47,0x11a47), (0x11a99,0x11a99),
    (0x11c3f,0x11c3f), (0x11d42,0x11d42), (0x11d44,0x11d45), (0x11d97,0x11d97), (0x11f41,0x11f42),
    (0x1612f,0x1612f), (0x16af0,0x16af4), (0x16b30,0x16b36), (0x16ff0,0x16ff1), (0x1bc9e,0x1bc9e),
    (0x1ccd6,0x1ccf9), (0x1d15e,0x1d169), (0x1d16d,0x1d172), (0x1d17b,0x1d182), (0x1d185,0x1d18b),
    (0x1d1aa,0x1d1ad), (0x1d1bb,0x1d1c0), (0x1d242,0x1d244), (0x1d400,0x1d454), (0x1d456,0x1d49c),
    (0x1d49e,0x1d49f), (0x1d4a2,0x1d4a2), (0x1d4a5,0x1d4a6), (0x1d4a9,0x1d4ac), (0x1d4ae,0x1d4b9),
    (0x1d4bb,0x1d4bb), (0x1d4bd,0x1d4c3), (0x1d4c5,0x1d505), (0x1d507,0x1d50a), (0x1d50d,0x1d514),
    (0x1d516,0x1d51c), (0x1d51e,0x1d539), (0x1d53b,0x1d53e), (0x1d540,0x1d544), (0x1d546,0x1d546),
    (0x1d54a,0x1d550), (0x1d552,0x1d6a5), (0x1d6a8,0x1d7cb), (0x1d7ce,0x1d7ff), (0x1e000,0x1e006),
    (0x1e008,0x1e018), (0x1e01b,0x1e021), (0x1e023,0x1e024), (0x1e026,0x1e02a), (0x1e030,0x1e06d),
    (0x1e08f,0x1e08f), (0x1e130,0x1e136), (0x1e2ae,0x1e2ae), (0x1e2ec,0x1e2ef), (0x1e4ec,0x1e4ef),
    (0x1e5ee,0x1e5ef), (0x1e8d0,0x1e8d6), (0x1e944,0x1e94a), (0x1ee00,0x1ee03), (0x1ee05,0x1ee1f),
    (0x1ee21,0x1ee22), (0x1ee24,0x1ee24), (0x1ee27,0x1ee27), (0x1ee29,0x1ee32), (0x1ee34,0x1ee37),
    (0x1ee39,0x1ee39), (0x1ee3b,0x1ee3b), (0x1ee42,0x1ee42), (0x1ee47,0x1ee47), (0x1ee49,0x1ee49),
    (0x1ee4b,0x1ee4b), (0x1ee4d,0x1ee4f), (0x1ee51,0x1ee52), (0x1ee54,0x1ee54), (0x1ee57,0x1ee57),
    (0x1ee59,0x1ee59), (0x1ee5b,0x1ee5b), (0x1ee5d,0x1ee5d), (0x1ee5f,0x1ee5f), (0x1ee61,0x1ee62),
    (0x1ee64,0x1ee64), (0x1ee67,0x1ee6a), (0x1ee6c,0x1ee72), (0x1ee74,0x1ee77), (0x1ee79,0x1ee7c),
    (0x1ee7e,0x1ee7e), (0x1ee80,0x1ee89), (0x1ee8b,0x1ee9b), (0x1eea1,0x1eea3), (0x1eea5,0x1eea9),
    (0x1eeab,0x1eebb), (0x1f100,0x1f10a), (0x1f110,0x1f12e), (0x1f130,0x1f14f), (0x1f16a,0x1f16c),
    (0x1f190,0x1f190), (0x1f200,0x1f202), (0x1f210,0x1f23b), (0x1f240,0x1f248), (0x1f250,0x1f251),
    (0x1fbf0,0x1fbf9), (0x2f800,0x2fa1d),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn userinfo_splits_at_the_last_at_and_the_first_colon() {
        let s = urlsplit("vless://a:b@c@h.example:443/").unwrap();
        assert_eq!(s.username(), Some("a"));
        assert_eq!(s.hostname().as_deref(), Some("h.example"));
        assert_eq!(s.port(), Ok(Some(443)));
    }

    #[test]
    fn a_fragment_swallows_a_query_that_follows_it() {
        let s = urlsplit("vless://u@h.example#Name?a=1").unwrap();
        assert_eq!(s.fragment, "Name?a=1");
        assert_eq!(s.query, "");
        // The other way round the query is real.
        let s = urlsplit("vless://u@h.example:443?a=1#Name").unwrap();
        assert_eq!(s.query, "a=1");
        assert_eq!(s.fragment, "Name");
    }

    #[test]
    fn port_failures_carry_pythons_wording() {
        assert_eq!(urlsplit("v://u@h:99999/").unwrap().port(), Err("Port out of range 0-65535".into()));
        assert_eq!(
            urlsplit("v://u@h:abc/").unwrap().port(),
            Err("Port could not be cast to integer value as 'abc'".into())
        );
        // Unbounded ints in Python: a long digit run is out of range, not unparseable.
        assert_eq!(
            urlsplit("v://u@h:99999999999999999999999999/").unwrap().port(),
            Err("Port out of range 0-65535".into())
        );
        assert_eq!(urlsplit("v://u@h:00443/").unwrap().port(), Ok(Some(443)));
    }

    #[test]
    fn brackets_must_hold_an_ipv6_literal() {
        assert!(urlsplit("v://u@[::1]:443/").is_ok());
        assert_eq!(urlsplit("v://u@[bad:443/"), Err("Invalid IPv6 URL".into()));
        assert_eq!(
            urlsplit("v://u@[1.2.3.4]:443/"),
            Err("An IPv4 address cannot be in brackets".into())
        );
        assert_eq!(
            urlsplit("v://u@[bad]:443/"),
            Err("'bad' does not appear to be an IPv4 or IPv6 address".into())
        );
        // A zone id is Python-legal and must not be handed to Rust's parser.
        assert!(urlsplit("v://u@[fe80::1%25en0]:443/").is_ok());
    }

    #[test]
    fn a_host_is_lowercased_but_a_zone_id_is_not() {
        assert_eq!(urlsplit("v://u@HOST.Example/").unwrap().hostname().as_deref(), Some("host.example"));
        assert_eq!(
            urlsplit("v://u@[FE80::1%EN0]/").unwrap().hostname().as_deref(),
            Some("fe80::1%EN0")
        );
    }

    #[test]
    fn nfkc_rejects_only_what_normalizes_into_a_delimiter() {
        // U+2100 decomposes to "a/c" — a delimiter appears from nowhere.
        assert!(urlsplit("v://u@h.example\u{2100}:443/").is_err());
        // U+FF20 is a fullwidth @.
        assert!(urlsplit("v://u@h.example\u{ff20}x:443/").is_err());
        // Á is already composed, so NFKC is a no-op and the netloc stands.
        assert!(urlsplit("v://u@h.example\u{c1}:443/").is_ok());
        // A plain CJK host is NFKC-stable too.
        assert!(urlsplit("v://u@\u{4e2d}\u{6587}.example:443/").is_ok());
    }

    #[test]
    fn blank_query_values_are_dropped_and_plus_is_a_space() {
        let qs = parse_qs("sni=&path=%2Fa+b&x=%2520&flag");
        assert_eq!(first(&qs, "sni", "fallback"), "fallback");
        assert_eq!(first(&qs, "path", ""), "/a b");
        // parse_qs decoded once, `_first` decodes again.
        assert_eq!(first(&qs, "x", ""), " ");
        assert_eq!(first(&qs, "flag", "none"), "none");
    }

    #[test]
    fn unquote_leaves_a_malformed_escape_alone() {
        assert_eq!(unquote("%zz"), "%zz");
        assert_eq!(unquote("%2"), "%2");
        assert_eq!(unquote("%E4%B8%AD"), "\u{4e2d}");
        // Truncated UTF-8 becomes the replacement character, not an error.
        assert_eq!(unquote("%E4%B8"), "\u{fffd}");
        assert_eq!(unquote("a%4Ab"), "aJb");
    }

    #[test]
    fn a_non_ascii_run_passes_through_undecoded() {
        assert_eq!(unquote("\u{4e2d}%20\u{6587}"), "\u{4e2d} \u{6587}");
    }
}
