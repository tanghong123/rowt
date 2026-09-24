//! Share links (`vless://`, `vmess://`, `anytls://`, `hysteria2://`, `ss://`,
//! `trojan://`, `tuic://`) into sing-box outbounds — a port of
//! `config/vless-parse.py`.
//!
//! This is the one module in rowt-core that handles credentials: the userinfo of
//! a share link is a UUID or a password. A mis-parse here does not fail loudly,
//! it produces a *plausible* outbound that quietly does not connect, or connects
//! somewhere else. So the rules below follow the Python exactly, including the
//! parts that are accidents of Python semantics rather than decisions:
//!
//!   * `u.port or 443` — port 0 is falsy, so `:0` becomes 443.
//!   * `_tag_for` is evaluated as an argument, so a link that then FAILS to
//!     parse has still consumed its tag; the next link with the same name gets
//!     `-2`.
//!   * `_tag_for`'s index counts input lines, skipped ones included, so the
//!     `server-N` fallbacks have gaps.
//!   * `str(cfg.get("ps", ""))` has no None-guard, so a vmess `"ps": null`
//!     produces the literal name `None`.
//!   * `base64.b64decode` without `validate` DISCARDS characters outside the
//!     alphabet before checking padding, so a link with stray whitespace or a
//!     URL-safe alphabet decodes anyway — and the script's own `pad` is computed
//!     from the length *before* that discarding. A non-ASCII character is the
//!     exception: that one is refused rather than dropped.
//!   * `str.strip()` counts `\x1c`–`\x1f` as whitespace and `str::trim()` does
//!     not, so `strip()` below exists and every `.trim()` here would be a bug.
//!
//! What is deliberately NOT reproduced is noted at each site. The differential
//! gate (`parity vless-diff`) is what holds the rest to the Python.

use crate::pyjson;
use crate::pyurl::{self, first, parse_qs, unquote, urlsplit};
use serde_json::{Map, Value};
use std::collections::HashSet;

/// Tags rowt uses for itself; a server may not claim one.
pub const RESERVED: [&str; 8] =
    ["escape", "auto", "direct", "corp", "block", "in", "local", "dns-out"];

/// The schemes `parse_link` speaks. The importers read it too, to decide which
/// V2Box rows are links.
pub const SCHEMES: [&str; 8] =
    ["vless://", "vmess://", "anytls://", "hysteria2://", "hy2://", "ss://", "trojan://", "tuic://"];

/// The Shadowsocks methods the pinned sing-box (1.13.14) accepts, probed with
/// `sing-box check`, which matches them case-sensitively. One it refuses fails
/// the check for the WHOLE config, and with it every render.
const SS_METHODS: [&str; 18] = [
    "none",
    "aes-128-gcm",
    "aes-192-gcm",
    "aes-256-gcm",
    "chacha20-ietf-poly1305",
    "xchacha20-ietf-poly1305",
    "2022-blake3-aes-128-gcm",
    "2022-blake3-aes-256-gcm",
    "2022-blake3-chacha20-poly1305",
    "aes-128-ctr",
    "aes-192-ctr",
    "aes-256-ctr",
    "aes-128-cfb",
    "aes-192-cfb",
    "aes-256-cfb",
    "rc4-md5",
    "chacha20-ietf",
    "xchacha20",
];

/// The names other clients give three of them.
const SS_ALIASES: [(&str, &str); 3] = [
    ("plain", "none"),
    ("chacha20-poly1305", "chacha20-ietf-poly1305"),
    ("xchacha20-poly1305", "xchacha20-ietf-poly1305"),
];

/// A Shadowsocks 2022 password is a base64 key of exactly this many bytes, or
/// several joined by `:` (identity keys, which only the AES methods take).
const SS_2022_KEY_BYTES: [(&str, i64); 3] = [
    ("2022-blake3-aes-128-gcm", 16),
    ("2022-blake3-aes-256-gcm", 32),
    ("2022-blake3-chacha20-poly1305", 32),
];

fn obj() -> Map<String, Value> {
    Map::new()
}

/// `str.strip()`.
///
/// Python's whitespace set is Unicode White_Space PLUS the four ASCII
/// separators `\x1c`–`\x1f`, which `char::is_whitespace` does not include. That
/// gap is not academic: a subscription body opening with one of them is
/// stripped by the Python and kept here, so `splitlines` yields a leading empty
/// line, every index shifts by one, and every unnamed server is renamed.
pub fn strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}'))
}

/// `[a for a in alpn.split(",") if a]`
fn alpn_list(alpn: &str) -> Value {
    Value::Array(alpn.split(',').filter(|a| !a.is_empty()).map(Value::from).collect())
}

/// `_first(qs,"sni") or _first(qs,"peer") or server`
fn sni_of(qs: &[(String, Vec<String>)], server: &str) -> String {
    let sni = first(qs, "sni", "");
    if !sni.is_empty() {
        return sni;
    }
    let peer = first(qs, "peer", "");
    if !peer.is_empty() {
        return peer;
    }
    server.to_string()
}

/// The userinfo/host/port every URI-shaped protocol shares.
fn endpoint(u: &pyurl::Split) -> Result<(String, String, u32), String> {
    let secret = unquote(u.username().unwrap_or(""));
    let server = u.hostname().unwrap_or_default();
    // `u.port or 443`: None and 0 are both falsy, and the port error is raised
    // before the missing-secret check because Python evaluates it first.
    let port = u.port()?.filter(|p| *p != 0).unwrap_or(443);
    Ok((secret, server, port))
}

pub fn parse_vless(link: &str, tag: &str) -> Result<Value, String> {
    let u = urlsplit(link)?;
    let (uuid, server, port) = endpoint(&u)?;
    if uuid.is_empty() || server.is_empty() {
        return Err("vless link missing uuid or host".into());
    }
    let qs = parse_qs(&u.query);
    let security = first(&qs, "security", "none").to_lowercase();
    let flow = first(&qs, "flow", "");
    let sni = sni_of(&qs, &server);
    let fp = first(&qs, "fp", "chrome");
    let alpn = first(&qs, "alpn", "");

    let mut out = obj();
    out.insert("type".into(), "vless".into());
    out.insert("tag".into(), tag.into());
    out.insert("server".into(), server.into());
    out.insert("server_port".into(), port.into());
    out.insert("uuid".into(), uuid.into());
    if !flow.is_empty() {
        out.insert("flow".into(), flow.into());
    }

    if matches!(security.as_str(), "tls" | "reality" | "xtls") {
        let mut tls = obj();
        tls.insert("enabled".into(), true.into());
        tls.insert("server_name".into(), sni.into());
        if !alpn.is_empty() {
            tls.insert("alpn".into(), alpn_list(&alpn));
        }
        let mut utls = obj();
        utls.insert("enabled".into(), true.into());
        utls.insert("fingerprint".into(), fp.into());
        tls.insert("utls".into(), utls.into());
        if security == "reality" {
            let pbk = first(&qs, "pbk", "");
            if pbk.is_empty() {
                return Err("reality link missing pbk (public key)".into());
            }
            let mut r = obj();
            r.insert("enabled".into(), true.into());
            r.insert("public_key".into(), pbk.into());
            r.insert("short_id".into(), first(&qs, "sid", "").into());
            tls.insert("reality".into(), r.into());
        }
        out.insert("tls".into(), tls.into());
    }

    if let Some(t) = transport(&qs) {
        out.insert("transport".into(), t);
    }
    Ok(out.into())
}

/// `_transport` — the `type=` transport VLESS and Trojan links share; tcp (and
/// anything unknown) is none.
fn transport(qs: &[(String, Vec<String>)]) -> Option<Value> {
    let net = first(qs, "type", "tcp").to_lowercase();
    let host = first(qs, "host", "");
    match net.as_str() {
        "ws" | "websocket" => {
            let mut t = obj();
            t.insert("type".into(), "ws".into());
            t.insert("path".into(), first(qs, "path", "/").into());
            if !host.is_empty() {
                let mut h = obj();
                h.insert("Host".into(), host.into());
                t.insert("headers".into(), h.into());
            }
            Some(t.into())
        }
        "grpc" => {
            let mut t = obj();
            t.insert("type".into(), "grpc".into());
            t.insert("service_name".into(), first(qs, "serviceName", "").into());
            Some(t.into())
        }
        "http" | "h2" => {
            let mut t = obj();
            t.insert("type".into(), "http".into());
            t.insert("path".into(), first(qs, "path", "/").into());
            if !host.is_empty() {
                t.insert(
                    "host".into(),
                    Value::Array(
                        host.split(',').filter(|h| !h.is_empty()).map(Value::from).collect(),
                    ),
                );
            }
            Some(t.into())
        }
        _ => None,
    }
}

/// `x or y` over two query values — the first non-empty one.
fn either(a: String, b: impl FnOnce() -> String) -> String {
    if a.is_empty() {
        b()
    } else {
        a
    }
}

/// `parse_trojan`. The WHOLE userinfo is the password, so a `:` in it is not a
/// separator as it is for the uuid-shaped schemes; TLS is on unless the link
/// says `security=none`; the transports are VLESS's.
pub fn parse_trojan(link: &str, tag: &str) -> Result<Value, String> {
    let u = urlsplit(link)?;
    let password = unquote(u.userinfo());
    let server = u.hostname().unwrap_or_default();
    let port = u.port()?.filter(|p| *p != 0).unwrap_or(443);
    if password.is_empty() || server.is_empty() {
        return Err("trojan link missing password or host".into());
    }
    let qs = parse_qs(&u.query);
    let security = first(&qs, "security", "tls").to_lowercase();

    let mut out = obj();
    out.insert("type".into(), "trojan".into());
    out.insert("tag".into(), tag.into());
    out.insert("server".into(), server.clone().into());
    out.insert("server_port".into(), port.into());
    out.insert("password".into(), password.into());
    if security != "none" {
        let insecure = either(first(&qs, "allowInsecure", ""), || first(&qs, "insecure", "0"));
        let mut tls = obj();
        tls.insert("enabled".into(), true.into());
        tls.insert("server_name".into(), sni_of(&qs, &server).into());
        tls.insert("insecure".into(), matches!(insecure.as_str(), "1" | "true" | "True").into());
        let alpn = first(&qs, "alpn", "");
        if !alpn.is_empty() {
            tls.insert("alpn".into(), alpn_list(&alpn));
        }
        let mut utls = obj();
        utls.insert("enabled".into(), true.into());
        utls.insert("fingerprint".into(), first(&qs, "fp", "chrome").into());
        tls.insert("utls".into(), utls.into());
        if security == "reality" {
            let pbk = first(&qs, "pbk", "");
            if pbk.is_empty() {
                return Err("reality link missing pbk (public key)".into());
            }
            let mut r = obj();
            r.insert("enabled".into(), true.into());
            r.insert("public_key".into(), pbk.into());
            r.insert("short_id".into(), first(&qs, "sid", "").into());
            tls.insert("reality".into(), r.into());
        }
        out.insert("tls".into(), tls.into());
    }
    if let Some(t) = transport(&qs) {
        out.insert("transport".into(), t);
    }
    Ok(out.into())
}

/// `_UUID.fullmatch` — 8-4-4-4-12 hex, with every hyphen or none of them.
fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    let hex = |r: &[u8]| r.iter().all(|c| c.is_ascii_hexdigit());
    match b.len() {
        32 => hex(b),
        36 => {
            [8, 13, 18, 23].iter().all(|&i| b[i] == b'-')
                && hex(&b[..8])
                && hex(&b[9..13])
                && hex(&b[14..18])
                && hex(&b[19..23])
                && hex(&b[24..])
        }
        _ => false,
    }
}

/// `parse_tuic` — v5, `uuid:password@host`. QUIC, so TLS is always on, and h3
/// is offered when the link names no ALPN. The uuid is checked because
/// sing-box refuses a malformed one for the whole config; tuning values it does
/// not know are dropped, leaving its defaults.
pub fn parse_tuic(link: &str, tag: &str) -> Result<Value, String> {
    let u = urlsplit(link)?;
    let uuid = unquote(u.username().unwrap_or(""));
    let password = unquote(u.password().unwrap_or(""));
    let server = u.hostname().unwrap_or_default();
    let port = u.port()?.filter(|p| *p != 0).unwrap_or(443);
    if uuid.is_empty() || password.is_empty() || server.is_empty() {
        return Err("tuic link missing uuid, password or host".into());
    }
    if !is_uuid(&uuid) {
        return Err("tuic link has a malformed uuid".into());
    }
    let qs = parse_qs(&u.query);
    let insecure = either(first(&qs, "allow_insecure", ""), || {
        either(first(&qs, "allowInsecure", ""), || first(&qs, "insecure", "0"))
    });
    let mut tls = obj();
    tls.insert("enabled".into(), true.into());
    tls.insert("server_name".into(), sni_of(&qs, &server).into());
    tls.insert("insecure".into(), matches!(insecure.as_str(), "1" | "true" | "True").into());
    let alpn = first(&qs, "alpn", "h3");
    if !alpn.is_empty() {
        tls.insert("alpn".into(), alpn_list(&alpn));
    }

    let mut out = obj();
    out.insert("type".into(), "tuic".into());
    out.insert("tag".into(), tag.into());
    out.insert("server".into(), server.into());
    out.insert("server_port".into(), port.into());
    out.insert("uuid".into(), uuid.into());
    out.insert("password".into(), password.into());
    let cc = either(first(&qs, "congestion_control", ""), || first(&qs, "congestion_controller", ""))
        .to_lowercase()
        .replace('-', "_");
    if matches!(cc.as_str(), "cubic" | "new_reno" | "bbr") {
        out.insert("congestion_control".into(), cc.into());
    }
    let relay = first(&qs, "udp_relay_mode", "").to_lowercase();
    if matches!(relay.as_str(), "native" | "quic") {
        out.insert("udp_relay_mode".into(), relay.into());
    }
    out.insert("tls".into(), tls.into());
    Ok(out.into())
}

/// `_b64_key_bytes` — the bytes `s` holds as padded standard base64, the only
/// form sing-box reads a Shadowsocks 2022 key in, or -1 if it is not that.
fn b64_key_bytes(s: &str) -> i64 {
    let pads = s.len() - s.trim_end_matches('=').len();
    let body = &s[..s.len() - pads];
    let ok = s.chars().count() % 4 == 0
        && pads <= 2
        && body.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'+' || c == b'/');
    if !ok {
        return -1;
    }
    (s.len() / 4 * 3) as i64 - pads as i64
}

/// `_ss_plugin` — a SIP003 `name;opt=v;flag` as sing-box's `plugin` and
/// `plugin_opts`. sing-box builds in obfs-local (simple-obfs) and v2ray-plugin
/// only, and reads their mode case-sensitively, so the mode is lowercased and
/// checked here.
fn ss_plugin(spec: &str) -> Result<(String, String), String> {
    let (name, rest) = spec.split_once(';').unwrap_or((spec, ""));
    let mut name = strip(name).to_lowercase();
    if name == "simple-obfs" {
        name = "obfs-local".into();
    }
    if name != "obfs-local" && name != "v2ray-plugin" {
        let shown = if name.is_empty() { "(empty)" } else { &name };
        return Err(format!("shadowsocks plugin {shown} is not supported by sing-box"));
    }
    let key = if name == "obfs-local" { "obfs" } else { "mode" };
    let mut opts: Vec<String> =
        if rest.is_empty() { Vec::new() } else { rest.split(';').map(String::from).collect() };
    let mut mode: Option<String> = None;
    for opt in opts.iter_mut() {
        if let Some((k, v)) = opt.split_once('=') {
            if k == key {
                let m = v.to_lowercase();
                *opt = format!("{key}={m}");
                mode = Some(m);
            }
        }
    }
    let allowed: [&str; 2] = if name == "obfs-local" { ["http", "tls"] } else { ["websocket", "quic"] };
    if let Some(m) = &mode {
        if !allowed.contains(&m.as_str()) {
            let shown = if m.is_empty() { "(empty)" } else { m };
            return Err(format!("{name} mode {shown} is not supported by sing-box"));
        }
    }
    if mode.as_deref() == Some("quic") && !opts.iter().any(|o| o == "tls") {
        return Err("v2ray-plugin mode quic needs tls".into());
    }
    Ok((name, opts.join(";")))
}

/// `shadowsocks_outbound` — for the ss:// parser and the Shadowrocket importer
/// alike, refusing what the pinned sing-box would refuse. Let through, one bad
/// method or key fails `sing-box check` for the whole config, and with it every
/// render until the server is removed by hand.
pub fn shadowsocks_outbound(
    tag: &str,
    server: &str,
    port: i64,
    method: &str,
    password: &str,
    plugin: &str,
) -> Result<Value, String> {
    let lowered = strip(method).to_lowercase();
    let m = SS_ALIASES.iter().find(|(a, _)| *a == lowered).map_or(lowered.clone(), |(_, t)| t.to_string());
    if !SS_METHODS.contains(&m.as_str()) {
        let shown = if strip(method).is_empty() { "(empty)" } else { strip(method) };
        return Err(format!("shadowsocks method {shown} is not supported by sing-box"));
    }
    if password.is_empty() && m != "none" {
        return Err("shadowsocks server missing password".into());
    }
    if !(0 < port && port < 65536) {
        return Err(format!("shadowsocks port {port} is out of range"));
    }
    if let Some((_, size)) = SS_2022_KEY_BYTES.iter().find(|(n, _)| *n == m) {
        let keys: Vec<&str> = password.split(':').collect();
        if keys.len() > 1 && m == "2022-blake3-chacha20-poly1305" {
            return Err(format!("shadowsocks {m} takes a single key"));
        }
        if keys.iter().any(|k| b64_key_bytes(k) != *size) {
            return Err(format!("shadowsocks {m} needs base64 keys of {size} bytes"));
        }
    }
    let mut out = obj();
    out.insert("type".into(), "shadowsocks".into());
    out.insert("tag".into(), tag.into());
    out.insert("server".into(), server.into());
    out.insert("server_port".into(), port.into());
    out.insert("method".into(), m.into());
    out.insert("password".into(), password.into());
    if !plugin.is_empty() {
        let (name, opts) = ss_plugin(plugin)?;
        out.insert("plugin".into(), name.into());
        out.insert("plugin_opts".into(), opts.into());
    }
    Ok(out.into())
}

/// `_ss_b64` — vmess's forgiving base64 (either alphabet, padding optional),
/// with the failure reduced to one fixed message.
fn ss_b64(s: &str) -> Result<String, String> {
    let pad = "=".repeat((4 - s.chars().count() % 4) % 4);
    let swapped: String = s
        .chars()
        .map(|c| match c {
            '-' => '+',
            '_' => '/',
            c => c,
        })
        .collect();
    let raw = b64decode(&format!("{swapped}{pad}")).map_err(|_| "ss link is not valid base64".to_string())?;
    Ok(String::from_utf8_lossy(&raw).into_owned())
}

/// `parse_ss` — SIP002 `ss://base64(method:password)@host:port`, SIP022's
/// percent-encoded `ss://method:password@host:port`, and the legacy all-base64
/// `ss://base64(method:password@host:port)`. Split by hand, NOT by `urlsplit`:
/// standard base64 carries `/`, which would end the netloc early — and the
/// Python splits by hand too, so the two agree on the same wrong answer only if
/// neither uses it.
pub fn parse_ss(link: &str, tag: &str) -> Result<Value, String> {
    let rest = &link["ss://".len()..];
    let rest = rest.split_once('#').map_or(rest, |(b, _)| b);
    let (body, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (cred, hostport) = match body.rsplit_once('@') {
        Some((userinfo, hostport)) => {
            let cred = unquote(userinfo);
            let cred = if cred.contains(':') { cred } else { ss_b64(&cred)? };
            (cred, hostport.to_string())
        }
        // A trailing `/` is the path separator of `…/?plugin=`, not base64: a
        // well-formed legacy body ends in the encoding of a port digit, never
        // `/`. Stripping it also keeps the decode away from data after a pad,
        // where CPython 3.13+'s decoder (it reads on) and this one (it stops)
        // part ways.
        None => match ss_b64(body.trim_end_matches('/'))?.rsplit_once('@') {
            Some((c, h)) => (c.to_string(), h.to_string()),
            None => return Err("ss link is not method:password@host:port".into()),
        },
    };
    let Some((method, password)) = cred.split_once(':') else {
        return Err("ss link missing method:password".into());
    };
    let hostport = hostport.trim_end_matches('/');
    let (host, port) = match hostport.strip_prefix('[') {
        Some(inner) => {
            let (h, after) = inner.split_once(']').unwrap_or((inner, ""));
            (h, after.strip_prefix(':').unwrap_or(""))
        }
        None => hostport.rsplit_once(':').unwrap_or((hostport, "")),
    };
    if host.is_empty() {
        return Err("ss link missing host".into());
    }
    // `port.isdigit() and 0 < int(port) < 65536` — Python's int is unbounded,
    // so a run of digits too long for u128 is simply out of range.
    let valid = !port.is_empty()
        && port.bytes().all(|c| c.is_ascii_digit())
        && port.parse::<u128>().is_ok_and(|n| n > 0 && n < 65536);
    if !valid {
        return Err("ss link missing a valid port".into());
    }
    let plugin = first(&parse_qs(query), "plugin", "");
    shadowsocks_outbound(tag, &host.to_lowercase(), port.parse::<i64>().unwrap_or(0), method, password, &plugin)
}

pub fn parse_anytls(link: &str, tag: &str) -> Result<Value, String> {
    let u = urlsplit(link)?;
    let (password, server, port) = endpoint(&u)?;
    if password.is_empty() || server.is_empty() {
        return Err("anytls link missing password or host".into());
    }
    let qs = parse_qs(&u.query);
    let sni = sni_of(&qs, &server);
    let insecure = matches!(first(&qs, "insecure", "0").as_str(), "1" | "true" | "True");
    let alpn = first(&qs, "alpn", "");

    let mut tls = obj();
    tls.insert("enabled".into(), true.into());
    tls.insert("server_name".into(), sni.into());
    tls.insert("insecure".into(), insecure.into());
    let mut utls = obj();
    utls.insert("enabled".into(), true.into());
    utls.insert("fingerprint".into(), first(&qs, "fp", "chrome").into());
    tls.insert("utls".into(), utls.into());
    if !alpn.is_empty() {
        tls.insert("alpn".into(), alpn_list(&alpn));
    }

    let mut out = obj();
    out.insert("type".into(), "anytls".into());
    out.insert("tag".into(), tag.into());
    out.insert("server".into(), server.into());
    out.insert("server_port".into(), port.into());
    out.insert("password".into(), password.into());
    out.insert("tls".into(), tls.into());
    Ok(out.into())
}

pub fn parse_hysteria2(link: &str, tag: &str) -> Result<Value, String> {
    let u = urlsplit(link)?;
    let (password, server, port) = endpoint(&u)?;
    if password.is_empty() || server.is_empty() {
        return Err("hysteria2 link missing password or host".into());
    }
    let qs = parse_qs(&u.query);
    let sni = sni_of(&qs, &server);
    let insecure = matches!(first(&qs, "insecure", "0").as_str(), "1" | "true" | "True");
    let alpn = first(&qs, "alpn", "");

    let mut tls = obj();
    tls.insert("enabled".into(), true.into());
    tls.insert("server_name".into(), sni.into());
    tls.insert("insecure".into(), insecure.into());
    if !alpn.is_empty() {
        tls.insert("alpn".into(), alpn_list(&alpn));
    }

    let mut out = obj();
    out.insert("type".into(), "hysteria2".into());
    out.insert("tag".into(), tag.into());
    out.insert("server".into(), server.into());
    out.insert("server_port".into(), port.into());
    out.insert("password".into(), password.into());
    out.insert("tls".into(), tls.into());

    // `str.isdigit()` is true for non-ASCII decimal digits too, and `int()`
    // would then accept them. Only ASCII is honoured here — a Farsi digit in
    // `upmbps` would set the field in Python and leave it unset here.
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    let up = first(&qs, "upmbps", "");
    let down = first(&qs, "downmbps", "");
    if digits(&up) {
        out.insert("up_mbps".into(), up.parse::<u64>().unwrap_or(0).into());
    }
    if digits(&down) {
        out.insert("down_mbps".into(), down.parse::<u64>().unwrap_or(0).into());
    }
    if !first(&qs, "obfs", "").is_empty() {
        let mut o = obj();
        o.insert("type".into(), "salamander".into());
        o.insert("password".into(), first(&qs, "obfs-password", "").into());
        out.insert("obfs".into(), o.into());
    }
    Ok(out.into())
}

pub fn parse_vmess(link: &str, tag: &str) -> Result<Value, String> {
    let cfg = vmess_body(link).map_err(|e| format!("vmess link is not base64-JSON ({e})"))?;
    let Value::Object(cfg) = cfg else {
        return Err("vmess link is not a JSON object".into());
    };

    // `s(key, default)` — absent or null takes the default, anything else is
    // str()'d and stripped.
    let s = |key: &str, default: &str| -> String {
        match cfg.get(key) {
            None | Some(Value::Null) => default.to_string(),
            Some(v) => strip(&py_str(v)).to_string(),
        }
    };

    let server = s("add", "");
    let uuid = s("id", "");
    if server.is_empty() || uuid.is_empty() {
        return Err("vmess link missing id or host".into());
    }

    let net = {
        let n = s("net", "tcp");
        if n.is_empty() { "tcp".to_string() } else { n }.to_lowercase()
    };
    let host = s("host", "");
    let path = s("path", "/");

    let mut out = obj();
    out.insert("type".into(), "vmess".into());
    out.insert("tag".into(), tag.into());
    out.insert("server".into(), server.clone().into());
    out.insert("server_port".into(), py_int(cfg.get("port"), 443)?.into());
    out.insert("uuid".into(), uuid.into());
    out.insert("security".into(), {
        let scy = s("scy", "auto");
        if scy.is_empty() { "auto".to_string() } else { scy }.into()
    });
    // `int(cfg.get("aid", 0) or 0)` — the `or` swallows null and "" alike.
    let aid = match cfg.get("aid") {
        Some(v) if truthy(v) => py_int(Some(v), 0)?,
        _ => 0,
    };
    out.insert("alter_id".into(), aid.into());

    match net.as_str() {
        "ws" | "websocket" => {
            let mut t = obj();
            t.insert("type".into(), "ws".into());
            t.insert("path".into(), if path.is_empty() { "/".into() } else { path.clone() }.into());
            if !host.is_empty() {
                let mut h = obj();
                h.insert("Host".into(), host.clone().into());
                t.insert("headers".into(), h.into());
            }
            out.insert("transport".into(), t.into());
        }
        "grpc" => {
            let mut t = obj();
            t.insert("type".into(), "grpc".into());
            t.insert("service_name".into(), path.clone().into());
            out.insert("transport".into(), t.into());
        }
        "http" | "h2" => {
            let mut t = obj();
            t.insert("type".into(), "http".into());
            t.insert("path".into(), if path.is_empty() { "/".into() } else { path.clone() }.into());
            if !host.is_empty() {
                t.insert(
                    "host".into(),
                    Value::Array(
                        host.split(',').filter(|h| !h.is_empty()).map(Value::from).collect(),
                    ),
                );
            }
            out.insert("transport".into(), t.into());
        }
        "kcp" | "quic" => {
            return Err(format!("vmess {net} transport unsupported by sing-box"));
        }
        _ => {}
    }

    if matches!(s("tls", "").to_lowercase().as_str(), "tls" | "reality" | "xtls" | "1" | "true") {
        let mut tls = obj();
        let sni = s("sni", "");
        let name = if !sni.is_empty() {
            sni
        } else if !host.is_empty() {
            host
        } else {
            server
        };
        tls.insert("enabled".into(), true.into());
        tls.insert("server_name".into(), name.into());
        let alpn = s("alpn", "");
        if !alpn.is_empty() {
            tls.insert("alpn".into(), alpn_list(&alpn));
        }
        let fp = s("fp", "");
        if !fp.is_empty() {
            let mut utls = obj();
            utls.insert("enabled".into(), true.into());
            utls.insert("fingerprint".into(), fp.into());
            tls.insert("utls".into(), utls.into());
        }
        out.insert("tls".into(), tls.into());
    }
    Ok(out.into())
}

/// The base64-JSON body of a `vmess://` link, or the text of the exception the
/// Python would have interpolated.
fn vmess_body(link: &str) -> Result<Value, String> {
    let body = strip(&link["vmess://".len()..]);
    // The pad is computed from the length BEFORE invalid characters are
    // discarded, exactly as the Python does — so it is sometimes the wrong pad.
    let pad = "=".repeat((4 - body.chars().count() % 4) % 4);
    let swapped: String =
        body.chars().map(|c| match c {
            '-' => '+',
            '_' => '/',
            c => c,
        }).collect();
    let raw = b64decode(&format!("{swapped}{pad}"))?;
    let text = String::from_utf8_lossy(&raw);
    py_json_loads(&text)
}

/// `json.loads`, with Python's message for the failure that actually happens:
/// the first token is not a value at all, which is what any non-JSON byte
/// string produces.
///
/// A syntax error further into a well-started document gets serde_json's
/// wording instead. That is deliberate and not chased: `JSONDecodeError` text
/// is interpreter-version-specific (3.13 added "Illegal trailing comma", which
/// 3.12 words differently), so pinning it would pin the wrong Python. It stays
/// safe because it is only ever a parenthetical — a link one side accepts and
/// the other rejects changes the outbound array on stdout, which the gate
/// compares byte for byte.
///
/// Two acceptance differences ride along, both unreachable from a real link:
/// serde rejects `NaN`/`Infinity`, which Python reads as floats, and it rejects
/// a lone surrogate escape, which Python admits into a str that Rust cannot
/// represent at all.
pub fn py_json_loads(text: &str) -> Result<Value, String> {
    match serde_json::from_str::<Value>(text) {
        Ok(v) => Ok(v),
        Err(e) => {
            let idx = text.chars().position(|c| !matches!(c, ' ' | '\t' | '\n' | '\r'));
            let rest = idx.map(|i| &text[text.char_indices().nth(i).unwrap().0..]);
            match rest {
                Some(r) if starts_a_value(r) => Err(e.to_string()),
                _ => {
                    let idx = idx.unwrap_or_else(|| text.chars().count());
                    let (line, col) = line_col(text, idx);
                    Err(format!("Expecting value: line {line} column {col} (char {idx})"))
                }
            }
        }
    }
}

/// Whether `scan_once` would recognise a value here — the literals must match
/// in full, so `not json` is not a `null` that went wrong, it is no value at
/// all, and that is why it reports column 1 rather than where it gave up.
fn starts_a_value(t: &str) -> bool {
    let b = t.as_bytes();
    match b.first() {
        Some(b'"' | b'{' | b'[' | b'0'..=b'9') => true,
        Some(b'-') => matches!(b.get(1), Some(b'0'..=b'9')) || t.starts_with("-Infinity"),
        Some(b't') => t.starts_with("true"),
        Some(b'f') => t.starts_with("false"),
        Some(b'n') => t.starts_with("null"),
        Some(b'N') => t.starts_with("NaN"),
        Some(b'I') => t.starts_with("Infinity"),
        _ => false,
    }
}

/// Python counts lines and columns in code points, both 1-based.
fn line_col(text: &str, idx: usize) -> (usize, usize) {
    let (mut line, mut col) = (1usize, 1usize);
    for (i, c) in text.chars().enumerate() {
        if i == idx {
            break;
        }
        if c == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// `binascii.a2b_base64(s, strict_mode=False)` — non-alphabet characters are
/// discarded rather than rejected, and a pad sequence ends the data.
fn b64decode(s: &str) -> Result<Vec<u8>, String> {
    const PAD: u8 = b'=';
    // `_bytes_from_decode_data` encodes a str as ASCII first, so a single
    // non-ASCII character is refused OUTRIGHT — it is not one of the stray
    // characters the decoder below discards. The difference is not cosmetic: a
    // mangled link whose body picked up one such character is skipped by the
    // Python and would otherwise decode here into a plausible outbound.
    if !s.is_ascii() {
        return Err("string argument should contain only ASCII characters".into());
    }
    let val = |c: u8| -> Option<u8> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    };
    // Bytes are emitted as each character arrives, not once a quad completes —
    // which is what lets `…fQ==` yield its seventh byte before the pad ends the
    // scan. Buffering the quad instead silently truncates every input whose
    // length is not a multiple of 3.
    let (mut out, mut left, mut quad, mut pads) = (Vec::new(), 0u8, 0usize, 0usize);
    for &c in s.as_bytes() {
        if c == PAD {
            pads += 1;
            if quad >= 2 && quad + pads >= 4 {
                return Ok(out);
            }
            continue;
        }
        let Some(v) = val(c) else { continue };
        pads = 0;
        match quad {
            0 => {
                quad = 1;
                left = v;
            }
            1 => {
                quad = 2;
                out.push((left << 2) | (v >> 4));
                left = v & 0x0f;
            }
            2 => {
                quad = 3;
                out.push((left << 4) | (v >> 2));
                left = v & 0x03;
            }
            _ => {
                quad = 0;
                out.push((left << 6) | v);
                left = 0;
            }
        }
    }
    match quad {
        0 => Ok(out),
        1 => Err(format!(
            "Invalid base64-encoded string: number of data characters ({}) cannot be 1 more than a multiple of 4",
            (out.len() / 3) * 4 + 1
        )),
        _ => Err("Incorrect padding".into()),
    }
}

/// `str(v)` — a JSON value the way Python would print it.
pub fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        _ => py_repr(v),
    }
}

/// `repr(v)`, which is what `str()` falls back to and what a container prints
/// for its elements.
fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => pyurl::repr(s),
        Value::Array(a) => {
            format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", "))
        }
        Value::Object(m) => format!(
            "{{{}}}",
            m.iter()
                .map(|(k, v)| format!("{}: {}", pyurl::repr(k), py_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}

/// `int(v)` where v came out of JSON. A missing key takes the default; a float
/// truncates toward zero; a string is parsed the way Python's `int()` parses
/// one, underscores and surrounding whitespace included.
///
/// `int(None)` raises TypeError in Python, which `main` does NOT catch — the
/// script dies with a traceback. That is not reproduced: here a null port is a
/// ValueError like any other bad port, so the link is skipped instead.
fn py_int(v: Option<&Value>, default: i64) -> Result<i64, String> {
    let Some(v) = v else { return Ok(default) };
    match v {
        Value::Null => Err("int() argument must be a string, a bytes-like object or a real number, not 'NoneType'".into()),
        Value::Bool(b) => Ok(*b as i64),
        Value::Number(n) => Ok(n.as_i64().unwrap_or_else(|| n.as_f64().unwrap_or(0.0) as i64)),
        Value::String(s) => {
            let t = strip(s);
            let (sign, digits) = match t.strip_prefix('-') {
                Some(d) => (-1i64, d),
                None => (1i64, t.strip_prefix('+').unwrap_or(t)),
            };
            let clean: String = digits.replace('_', "");
            let ok = !clean.is_empty()
                && clean.chars().all(|c| c.is_ascii_digit())
                && !digits.starts_with('_')
                && !digits.ends_with('_')
                && !digits.contains("__");
            match clean.parse::<i64>() {
                Ok(n) if ok => Ok(sign * n),
                _ => Err(format!("invalid literal for int() with base 10: {}", pyurl::repr(s))),
            }
        }
        other => Err(format!(
            "int() argument must be a string, a bytes-like object or a real number, not {}",
            pyurl::repr(match other {
                Value::Array(_) => "list",
                _ => "dict",
            })
        )),
    }
}

/// Dispatch on the scheme — case-sensitively, so `VLESS://` is unsupported.
pub fn parse_link(link: &str, tag: &str) -> Result<Value, String> {
    if link.starts_with("vless://") {
        parse_vless(link, tag)
    } else if link.starts_with("vmess://") {
        parse_vmess(link, tag)
    } else if link.starts_with("anytls://") {
        parse_anytls(link, tag)
    } else if link.starts_with("hysteria2://") || link.starts_with("hy2://") {
        parse_hysteria2(link, tag)
    } else if link.starts_with("ss://") {
        parse_ss(link, tag)
    } else if link.starts_with("trojan://") {
        parse_trojan(link, tag)
    } else if link.starts_with("tuic://") {
        parse_tuic(link, tag)
    } else {
        Err("unsupported protocol".into())
    }
}

/// The display name of a link: the fragment, or for vmess the `ps` field buried
/// in the base64.
///
/// The two halves fail differently, and the difference is load-bearing. The
/// vmess branch swallows everything (`except Exception`) and yields a nameless
/// link. The URI branch does NOT — `urlsplit` raising there propagates out
/// through `_tag_for` before it can reserve a name, so a link with a bad host
/// leaves the tag free for the next link that wants it.
fn link_name(link: &str) -> Result<String, String> {
    if link.starts_with("vmess://") {
        return Ok(match vmess_body(link) {
            Ok(Value::Object(cfg)) => {
                // No None-guard here, unlike `s()` above: a null `ps` stringifies.
                strip(&py_str(cfg.get("ps").unwrap_or(&Value::String(String::new())))).to_string()
            }
            _ => String::new(),
        });
    }
    Ok(strip(&unquote(&urlsplit(link)?.fragment)).to_string())
}

/// `_tag_for` — a filesystem/API-safe tag, unique within `used`.
fn tag_for(link: &str, index: usize, used: &mut HashSet<String>) -> Result<String, String> {
    let name = link_name(link)?;
    let mut squashed = String::with_capacity(name.len());
    let mut in_run = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            squashed.push(c);
            in_run = false;
        } else if !in_run {
            squashed.push('-');
            in_run = true;
        }
    }
    let trimmed = squashed.trim_matches('-');
    let mut base = if trimmed.is_empty() { format!("server-{index}") } else { trimmed.to_string() };
    if RESERVED.contains(&base.as_str()) {
        base = format!("{base}-{index}");
    }
    let mut tag = base.clone();
    let mut n = 2;
    while used.contains(&tag) {
        tag = format!("{base}-{n}");
        n += 1;
    }
    used.insert(tag.clone());
    Ok(tag)
}

/// What `parse_many`/`combine` produce: the outbounds plus the lines the Python
/// wrote to stderr, in order.
#[derive(Debug, Default)]
pub struct Batch {
    pub outbounds: Vec<Value>,
    pub warnings: Vec<String>,
}

/// `parse_many` — parse a list of links, skipping what cannot be used.
///
/// The index handed to `tag_for` counts INPUT lines, so blank lines and
/// subscription headers leave gaps in the `server-N` fallbacks; and the tag is
/// allocated before the parse, so a link that fails still burns its name.
pub fn parse_many(links: &[String]) -> Result<Batch, (Batch, String)> {
    let mut b = Batch::default();
    let mut used: HashSet<String> = RESERVED.iter().map(|s| s.to_string()).collect();
    for (i, raw) in links.iter().enumerate() {
        let index = i + 1;
        let link = strip(raw);
        if link.is_empty() || link.starts_with('#') {
            continue;
        }
        if !link.contains("://") {
            continue; // subscription header lines (e.g. "REMARKS=…")
        }
        if !SCHEMES.iter().any(|p| link.starts_with(p)) {
            let proto = link.split_once("://").map(|(p, _)| p).unwrap_or("");
            b.warnings.push(format!("warning: skipping unsupported link ({proto}://)"));
            continue;
        }
        // One expression, because the two steps share a failure path: the tag
        // is reserved by the time `parse_link` runs, so a link that parses
        // badly has still taken its name — but a link whose URI will not split
        // at all fails inside `tag_for` and takes nothing.
        match tag_for(link, index, &mut used).and_then(|tag| parse_link(link, &tag)) {
            Ok(o) => b.outbounds.push(o),
            Err(e) => b.warnings.push(format!("warning: skipping a link ({e})")),
        }
    }
    if b.outbounds.is_empty() {
        let msg = "no usable vless:// / vmess:// / anytls:// / hysteria2:// / ss:// / trojan:// / tuic:// links found"
            .to_string();
        return Err((b, msg));
    }
    Ok(b)
}

/// `key_of` — identity of an outbound, ignoring its display name.
pub fn key_of(o: &Value) -> String {
    let get = |k: &str| o.get(k).filter(|v| truthy(v));
    let secret = get("uuid").or_else(|| get("password")).map(py_str).unwrap_or_default();
    format!(
        "{}:{}:{}:{}",
        o.get("type").map(py_str).unwrap_or_else(|| "None".into()),
        o.get("server").map(py_str).unwrap_or_else(|| "None".into()),
        o.get("server_port").map(py_str).unwrap_or_else(|| "None".into()),
        secret
    )
}

/// `combine` — dedupe by identity, keeping the first, and uniquify tags.
///
/// The index in a `server-N` fallback is the position in the INPUT array, not
/// in the output. A non-string tag is stringified here, where Python would keep
/// its type; `server dump` only ever writes strings.
pub fn combine(outbounds: &[Value]) -> Batch {
    let mut b = Batch::default();
    let mut used: HashSet<String> = RESERVED.iter().map(|s| s.to_string()).collect();
    let mut kept: Vec<(String, String)> = Vec::new();
    for (i, o) in outbounds.iter().enumerate() {
        let index = i + 1;
        let key = key_of(o);
        let tagged = o.get("tag").filter(|v| truthy(v)).map(py_str);
        if let Some((_, keep_tag)) = kept.iter().find(|(k, _)| *k == key) {
            let dropped = tagged.unwrap_or_else(|| format!("server-{index}"));
            if &dropped != keep_tag {
                b.warnings.push(format!(
                    "note: not importing '{dropped}' — same server as '{keep_tag}' ({}:{})",
                    o.get("server").map(py_str).unwrap_or_else(|| "None".into()),
                    o.get("server_port").map(py_str).unwrap_or_else(|| "None".into()),
                ));
            }
            continue;
        }
        let mut base = tagged.unwrap_or_else(|| format!("server-{index}"));
        if RESERVED.contains(&base.as_str()) {
            base = format!("{base}-{index}");
        }
        let mut tag = base.clone();
        let mut n = 2;
        while used.contains(&tag) {
            tag = format!("{base}-{n}");
            n += 1;
        }
        used.insert(tag.clone());
        let mut o = o.clone();
        if let Some(m) = o.as_object_mut() {
            m.insert("tag".into(), tag.clone().into());
        }
        kept.push((key, tag));
        b.outbounds.push(o);
    }
    b
}

/// The pure half of `fetch_subscription`: what to do with the body once it has
/// arrived. Common v2ray format is base64 over newline-separated links; a
/// plain-text list is accepted as-is.
pub fn decode_subscription(body: &str) -> Result<Vec<String>, String> {
    let body = strip(body);
    let decoded;
    let body = if body.contains("://") {
        body
    } else {
        let pad = "=".repeat((4 - body.chars().count() % 4) % 4);
        let swapped: String = body
            .chars()
            .map(|c| match c {
                '-' => '+',
                '_' => '/',
                c => c,
            })
            .collect();
        let raw = b64decode(&format!("{swapped}{pad}"))
            .map_err(|e| format!("could not decode subscription body: {e}"))?;
        decoded = String::from_utf8_lossy(&raw).into_owned();
        &decoded
    };
    if !body.contains("://") {
        return Err("subscription did not yield any share links (Clash/JSON not supported)".into());
    }
    Ok(splitlines(body))
}

/// `str.splitlines()` — more boundaries than `\n`, and no trailing empty piece.
///
/// Public because `--multi` splits its stdin the same way. Rust's `.lines()`
/// breaks on `\n` alone, so a subscription pasted with `\u{2028}` separators
/// would arrive as one unparseable line.
pub fn splitlines(s: &str) -> Vec<String> {
    let brk = |c: char| {
        matches!(c, '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}')
    };
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if brk(c) {
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `json.dump(result, sys.stdout, indent=2)` + the newline the script adds.
pub fn render(v: &Value) -> String {
    format!("{}\n", pyjson::dumps(v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn one(link: &str, tag: &str) -> Value {
        parse_link(link, tag).expect("should parse")
    }

    // The behaviours config/test_parse.py pins, kept here so the Python's own
    // checklist survives the port.

    #[test]
    fn hysteria2_carries_tls_and_the_mbps_hints() {
        let o = one("hysteria2://pw@h.example:443?insecure=0&sni=s.example&upmbps=20&downmbps=80#JP", "JP");
        assert_eq!(o["type"], "hysteria2");
        assert_eq!(o["password"], "pw");
        assert_eq!(o["tls"]["server_name"], "s.example");
        assert_eq!(o["tls"]["insecure"], false);
        assert_eq!(o["up_mbps"], 20);
        assert_eq!(o["down_mbps"], 80);
        assert!(o.get("obfs").is_none());
        assert!(o["tls"].get("alpn").is_none());
    }

    #[test]
    fn sni_falls_back_to_the_host_and_the_port_to_443() {
        let o = one("hysteria2://pw@h.example?insecure=1", "t");
        assert_eq!(o["tls"]["insecure"], true);
        assert_eq!(o["server_port"], 443);
        assert_eq!(o["tls"]["server_name"], "h.example");
    }

    #[test]
    fn hy2_is_an_alias() {
        assert_eq!(one("hy2://pw@h.example:443#X", "X"), one("hysteria2://pw@h.example:443#X", "X"));
    }

    #[test]
    fn a_zero_port_is_falsy_and_becomes_443() {
        assert_eq!(one("hysteria2://pw@h.example:0", "t")["server_port"], 443);
    }

    #[test]
    fn reality_needs_a_public_key() {
        let e = parse_link("vless://u@h.example:443?security=reality&pbk=", "t").unwrap_err();
        assert_eq!(e, "reality link missing pbk (public key)");
        let o = one("vless://u@h.example:443?security=reality&pbk=KEY&sid=ab", "t");
        assert_eq!(o["tls"]["reality"]["public_key"], "KEY");
        assert_eq!(o["tls"]["reality"]["short_id"], "ab");
        assert_eq!(o["tls"]["utls"]["fingerprint"], "chrome");
    }

    #[test]
    fn a_missing_secret_is_refused() {
        assert_eq!(
            parse_link("hysteria2://@h.example:443", "t").unwrap_err(),
            "hysteria2 link missing password or host"
        );
        assert_eq!(
            parse_link("vless://@h.example:443", "t").unwrap_err(),
            "vless link missing uuid or host"
        );
    }

    fn vmess(cfg: Value) -> String {
        format!("vmess://{}", b64(&serde_json::to_string(&cfg).unwrap()))
    }

    /// base64 without a dependency: the tests only need the standard alphabet.
    fn b64(s: &str) -> String {
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let b = s.as_bytes();
        let mut out = String::new();
        for c in b.chunks(3) {
            let n = ((c[0] as u32) << 16)
                | ((*c.get(1).unwrap_or(&0) as u32) << 8)
                | (*c.get(2).unwrap_or(&0) as u32);
            for k in 0..4 {
                if k <= c.len() {
                    out.push(A[((n >> (18 - 6 * k)) & 0x3f) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    #[test]
    fn vmess_ws_over_tls() {
        let link = vmess(json!({"ps":"Tokyo","add":"cdn.example.com","port":"443",
            "id":"b831381d-6324-4d53-ad4f-8cda48b30811","aid":"0","scy":"auto","net":"ws",
            "host":"cdn.example.com","path":"/ray","tls":"tls"}));
        let o = one(&link, "Tokyo");
        assert_eq!(o["server_port"], 443);
        assert_eq!(o["alter_id"], 0);
        assert_eq!(o["transport"], json!({"type":"ws","path":"/ray","headers":{"Host":"cdn.example.com"}}));
        assert_eq!(o["tls"]["server_name"], "cdn.example.com");
    }

    #[test]
    fn vmess_tcp_has_neither_transport_nor_tls() {
        let o = one(&vmess(json!({"ps":"p","add":"1.2.3.4","port":"8080","id":"u","net":"tcp","tls":""})), "t");
        assert!(o.get("transport").is_none());
        assert!(o.get("tls").is_none());
        assert_eq!(o["server_port"], 8080);
    }

    #[test]
    fn vmess_kcp_is_rejected() {
        let e = parse_link(&vmess(json!({"ps":"p","add":"h","port":"443","id":"u","net":"kcp"})), "t")
            .unwrap_err();
        assert_eq!(e, "vmess kcp transport unsupported by sing-box");
    }

    #[test]
    fn a_vmess_tag_comes_from_ps_not_a_fragment() {
        let b = parse_many(&[vmess(json!({"ps":"My Node","add":"h.example","port":"443","id":"u","net":"tcp"}))])
            .unwrap();
        assert_eq!(b.outbounds[0]["tag"], "My-Node");
    }

    #[test]
    fn key_of_ignores_the_name() {
        let a = json!({"type":"vless","tag":"Elm","server":"h","server_port":443,"uuid":"u"});
        let b = json!({"type":"vless","tag":"Other-Name","server":"h","server_port":443,"uuid":"u"});
        assert_eq!(key_of(&a), key_of(&b));
        // An empty uuid falls through to password, as `or` does.
        let c = json!({"type":"anytls","server":"h","server_port":443,"uuid":"","password":"p"});
        assert_eq!(key_of(&c), "anytls:h:443:p");
        // A missing type prints as None, which a hand-edited dump will hit.
        assert_eq!(key_of(&json!({"server":"h"})), "None:h:None:");
    }

    #[test]
    fn combine_keeps_the_first_of_a_duplicate_pair() {
        let outs = vec![
            json!({"type":"vless","tag":"First","server":"h","server_port":443,"uuid":"u"}),
            json!({"type":"vless","tag":"Alias","server":"h","server_port":443,"uuid":"u"}),
            json!({"type":"vless","tag":"Other","server":"h2","server_port":443,"uuid":"u"}),
        ];
        let b = combine(&outs);
        let tags: Vec<&str> = b.outbounds.iter().map(|o| o["tag"].as_str().unwrap()).collect();
        assert_eq!(tags, ["First", "Other"]);
        assert_eq!(b.warnings.len(), 1);
        assert!(b.warnings[0].starts_with("note: not importing 'Alias' — same server as 'First'"));
    }

    // The traps that no Python test covers.

    #[test]
    fn a_header_line_still_consumes_its_index() {
        // Line 1 is a header; the link on line 2 has no name, so its fallback
        // tag is server-2 — the gap is the point.
        let b = parse_many(&[
            "REMARKS=foo".into(),
            "hysteria2://pw@h1.example:443".into(),
            "hysteria2://pw2@h2.example:443".into(),
        ])
        .unwrap();
        let tags: Vec<&str> = b.outbounds.iter().map(|o| o["tag"].as_str().unwrap()).collect();
        assert_eq!(tags, ["server-2", "server-3"]);
    }

    #[test]
    fn a_link_that_fails_to_parse_has_already_taken_its_tag() {
        let b = parse_many(&[
            // Same name; the first is unparseable (no password) but claims "JP".
            "hysteria2://@h1.example:443#JP".into(),
            "hysteria2://pw@h2.example:443#JP".into(),
        ])
        .unwrap();
        assert_eq!(b.outbounds.len(), 1);
        assert_eq!(b.outbounds[0]["tag"], "JP-2");
        assert_eq!(b.warnings, ["warning: skipping a link (hysteria2 link missing password or host)"]);
    }

    #[test]
    fn a_link_whose_uri_will_not_split_takes_no_tag_at_all() {
        // The bracketed host makes `urlsplit` itself raise, which happens
        // INSIDE _tag_for and before it reserves anything — so the second link
        // gets the plain name rather than JP-2. The distinction only shows when
        // two links share a name and the first one is malformed this way.
        let b = parse_many(&[
            "vless://u@[bad]:443#JP".into(),
            "hysteria2://pw@h.example:443#JP".into(),
        ])
        .unwrap();
        assert_eq!(b.outbounds[0]["tag"], "JP");
        assert_eq!(
            b.warnings,
            ["warning: skipping a link ('bad' does not appear to be an IPv4 or IPv6 address)"]
        );
    }

    #[test]
    fn a_reserved_name_gets_its_index_appended() {
        let b = parse_many(&["hysteria2://pw@h.example:443#escape".into()]).unwrap();
        assert_eq!(b.outbounds[0]["tag"], "escape-1");
    }

    #[test]
    fn an_unsupported_scheme_is_named_in_the_warning() {
        let e = parse_many(&["wireguard://whatever@h.example:443".into()]).unwrap_err();
        assert_eq!(e.0.warnings, ["warning: skipping unsupported link (wireguard://)"]);
        assert_eq!(
            e.1,
            "no usable vless:// / vmess:// / anytls:// / hysteria2:// / ss:// / trojan:// / tuic:// links found"
        );
    }

    fn refused(link: &str, why: &str) {
        match parse_link(link, "t") {
            Ok(o) => panic!("{link}: parsed as {o}, should be refused ({why})"),
            Err(e) => assert!(e.contains(why), "{link}: refused with {e:?}, not {why:?}"),
        }
    }

    #[test]
    fn ss_sip002_base64_userinfo_may_carry_a_slash() {
        let cred = b64("aes-256-gcm:pa/ss?w");
        assert!(cred.contains('/'), "the case needs a '/' in the base64");
        let o = parse_link(&format!("ss://{cred}@H.Example:8388#N"), "N").unwrap();
        assert_eq!(o["type"], "shadowsocks");
        assert_eq!(o["server"], "h.example");
        assert_eq!(o["server_port"], 8388);
        assert_eq!(o["method"], "aes-256-gcm");
        assert_eq!(o["password"], "pa/ss?w");
        assert!(o.get("plugin").is_none());
    }

    #[test]
    fn ss_urlsafe_unpadded_with_an_obfs_plugin() {
        let cred: String = b64("chacha20-ietf-poly1305:p@ss")
            .replace('+', "-")
            .replace('/', "_")
            .trim_end_matches('=')
            .to_string();
        let o = parse_link(
            &format!("ss://{cred}@h.example:443/?plugin=simple-obfs%3Bobfs%3DHTTP%3Bobfs-host%3Dcdn.example#N"),
            "N",
        )
        .unwrap();
        assert_eq!(o["password"], "p@ss");
        assert_eq!(o["plugin"], "obfs-local");
        assert_eq!(o["plugin_opts"], "obfs=http;obfs-host=cdn.example");
    }

    #[test]
    fn ss_sip022_plaintext_ipv6_and_the_legacy_form() {
        let key = b64(&"k".repeat(32));
        let o = parse_link(
            &format!("ss://2022-blake3-aes-256-gcm:{}@[2001:DB8::1]:8388", key.replace('=', "%3D")),
            "t",
        )
        .unwrap();
        assert_eq!(o["password"], key.as_str());
        assert_eq!(o["server"], "2001:db8::1");
        let o = parse_link(&format!("ss://{}", b64("aes-128-gcm:p:w@legacy.example:8389")), "t").unwrap();
        assert_eq!((o["server"].as_str(), o["server_port"].as_i64()), (Some("legacy.example"), Some(8389)));
        assert_eq!(o["password"], "p:w", "the first ':' is the separator");
    }

    #[test]
    fn ss_refuses_what_sing_box_would() {
        let k16 = b64(&"k".repeat(16));
        let k32 = b64(&"k".repeat(32));
        let pw = b64("aes-256-gcm:pw");
        refused(&format!("ss://{}@h.example:1", b64("rc4:pw")), "method rc4 is not supported");
        refused(&format!("ss://2022-blake3-aes-128-gcm:{k32}@h.example:1"), "keys of 16 bytes");
        refused("ss://2022-blake3-aes-256-gcm:notbase64!@h.example:1", "keys of 32");
        refused(&format!("ss://2022-blake3-chacha20-poly1305:{k32}:{k32}@h.example:1"), "single key");
        refused(&format!("ss://{}@h.example:1", b64("aes-256-gcm:")), "missing password");
        refused(&format!("ss://{pw}@h.example"), "missing a valid port");
        refused(&format!("ss://{pw}@h.example:65536"), "missing a valid port");
        refused(&format!("ss://{pw}@h.example:{}", "9".repeat(60)), "missing a valid port");
        refused(&format!("ss://{}@h.example:1", b64("aes-256-gcm")), "missing method:password");
        refused("ss://!!!!", "not method:password@host:port");
        let base = format!("ss://{pw}@h.example:1?plugin=");
        refused(&format!("{base}shadow-tls%3Bhost%3Dx"), "plugin shadow-tls is not supported");
        refused(&format!("{base}obfs-local%3Bobfs%3Dbogus"), "obfs-local mode bogus");
        refused(&format!("{base}v2ray-plugin%3Bmode%3Dquic"), "mode quic needs tls");
        // And what it does take, so the refusals above do not refuse everything.
        parse_link(&format!("ss://2022-blake3-aes-128-gcm:{k16}:{k16}@h.example:1"), "t").unwrap();
        parse_link(&format!("{base}v2ray-plugin%3Bmode%3Dquic%3Btls"), "t").unwrap();
        let o = parse_link(&format!("ss://{}@h.example:1", b64("CHACHA20-POLY1305:pw")), "t").unwrap();
        assert_eq!(o["method"], "chacha20-ietf-poly1305", "alias, case-folded");
    }

    #[test]
    fn trojan_is_tls_by_default_and_the_userinfo_is_the_password() {
        let o = parse_link("trojan://p%40ss:w@T.example:443?allowInsecure=1#N", "N").unwrap();
        assert_eq!(o["password"], "p@ss:w");
        assert_eq!(o["tls"]["server_name"], "t.example");
        assert_eq!(o["tls"]["insecure"], true);
        assert_eq!(o["tls"]["utls"], json!({"enabled": true, "fingerprint": "chrome"}));
        assert!(o.get("transport").is_none());
        let o = parse_link("trojan://pw@t.example:80?security=none", "t").unwrap();
        assert!(o.get("tls").is_none());
        let o = parse_link(
            "trojan://pw@t.example?security=reality&pbk=K&sid=ab&type=ws&path=%2Fws&host=cdn.example",
            "t",
        )
        .unwrap();
        assert_eq!(o["tls"]["reality"], json!({"enabled": true, "public_key": "K", "short_id": "ab"}));
        assert_eq!(o["transport"], json!({"type": "ws", "path": "/ws", "headers": {"Host": "cdn.example"}}));
        refused("trojan://pw@t.example?security=reality", "missing pbk");
        refused("trojan://t.example:443", "missing password or host");
    }

    #[test]
    fn tuic_defaults_and_tuning() {
        let u = "2DD61D93-75D8-4DA4-AC0E-6AECE7EAC365";
        let o = parse_link(&format!("tuic://{u}:p%3Aw@u.example:8443#N"), "N").unwrap();
        assert_eq!((o["uuid"].as_str(), o["password"].as_str()), (Some(u), Some("p:w")));
        assert_eq!(o["tls"]["alpn"], json!(["h3"]));
        assert!(o.get("congestion_control").is_none());
        let o = parse_link(
            &format!("tuic://{u}:pw@u.example?congestion_control=New-Reno&udp_relay_mode=QUIC&allow_insecure=1"),
            "t",
        )
        .unwrap();
        assert_eq!(o["congestion_control"], "new_reno");
        assert_eq!(o["udp_relay_mode"], "quic");
        assert_eq!(o["tls"]["insecure"], true);
        let o = parse_link(&format!("tuic://{u}:pw@u.example?congestion_control=vegas"), "t").unwrap();
        assert!(o.get("congestion_control").is_none(), "unknown is dropped, not fatal");
        parse_link(&format!("tuic://{}:pw@u.example", u.replace('-', "")), "t").unwrap();
        refused("tuic://not-a-uuid:pw@u.example", "malformed uuid");
        refused(&format!("tuic://{u}@u.example"), "missing uuid, password or host");
    }

    #[test]
    fn a_doubly_escaped_query_value_decodes_twice() {
        // %2520 -> %20 (parse_qs) -> space (_first). The path is what ends up
        // in the transport, so this is a real routing difference.
        let o = one("vless://u@h.example:443?type=ws&path=%252Fa", "t");
        assert_eq!(o["transport"]["path"], "/a");
    }

    #[test]
    fn a_blank_query_value_reads_as_absent() {
        let o = one("vless://u@h.example:443?security=tls&fp=", "t");
        assert_eq!(o["tls"]["utls"]["fingerprint"], "chrome");
    }

    #[test]
    fn base64_discards_what_is_not_in_the_alphabet() {
        // A URL-safe body with stray whitespace still decodes.
        let ok = b64decode("e yJ hIjox fQ==").unwrap();
        assert_eq!(String::from_utf8(ok).unwrap(), r#"{"a":1}"#);
        assert_eq!(b64decode("QUJDRA=").unwrap_err(), "Incorrect padding");
        assert_eq!(b64decode("QUJDREU").unwrap_err(), "Incorrect padding");
        // One character past a quad is its own error, and the count it reports
        // is derived from the bytes already emitted, not from the input length.
        assert_eq!(
            b64decode("QUJDR").unwrap_err(),
            "Invalid base64-encoded string: number of data characters (5) cannot be 1 more than a multiple of 4"
        );
        assert_eq!(b64decode("!!!!").unwrap(), b"");
        // A non-ASCII character is refused rather than discarded.
        assert_eq!(
            b64decode("QUJD\u{4e2d}").unwrap_err(),
            "string argument should contain only ASCII characters"
        );
    }

    #[test]
    fn subscription_bodies_come_in_two_shapes() {
        assert_eq!(
            decode_subscription("hy2://pw@h.example\nhy2://pw2@h2.example").unwrap().len(),
            2
        );
        // base64 of the same two lines
        let b64 = "aHkyOi8vcHdAaC5leGFtcGxlCmh5MjovL3B3MkBoMi5leGFtcGxl";
        assert_eq!(decode_subscription(b64).unwrap().len(), 2);
        assert_eq!(
            decode_subscription("bm90aGluZyBoZXJl").unwrap_err(),
            "subscription did not yield any share links (Clash/JSON not supported)"
        );
    }

    #[test]
    fn python_whitespace_includes_the_ascii_separators() {
        // U+001C is whitespace to Python and not to Rust. A body that opens
        // with one would otherwise keep it, `splitlines` would yield a leading
        // empty line, and every index — so every unnamed server's name —
        // shifts by one.
        assert_eq!(strip("   \u{1c}x\u{1f} "), "x");
        let lines =
            decode_subscription("  \u{1c}hy2://pw@h1.example\nhy2://pw2@h2.example").unwrap();
        assert_eq!(lines.len(), 2);
        let b = parse_many(&lines).unwrap();
        assert_eq!(b.outbounds[0]["tag"], "server-1");
    }

    #[test]
    fn a_null_ps_becomes_the_literal_name_none() {
        let b = parse_many(&[vmess(json!({"ps":null,"add":"h.example","port":"443","id":"u","net":"tcp"}))])
            .unwrap();
        assert_eq!(b.outbounds[0]["tag"], "None");
    }
}
