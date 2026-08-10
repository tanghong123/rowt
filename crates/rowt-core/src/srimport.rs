//! Shadowrocket into rowt's review format — a port of `config/sr-import.py`.
//!
//! Shadowrocket keeps its servers in an NSKeyedArchiver plist and its rules in
//! a Surge-style `.conf`. Neither is documented; both are read the way the
//! Python reads them, which is to walk `$objects` looking for any dict that has
//! both a `type` and a `host` and to dereference each value ONE level through
//! its `CF$UID`.
//!
//! One thing worth keeping in view while reading: the real VLESS id lives in
//! Shadowrocket's generic `password` slot, the same one AnyTLS and Shadowsocks
//! use, while the `uuid` field is Shadowrocket's own config id. Preferring
//! `uuid` would produce outbounds that look completely correct and never
//! connect.
//!
//! Values reach `str()`, `int()` and `bool()` straight off the plist, so the
//! coercions here are Python's, not Rust's: `str()` of a BLOB is `b'…'`, of a
//! date is `2026-08-09 12:00:00`, and of a `CF$UID` that was never
//! dereferenced is `UID(3)`. `bool("0")` is True. `int(port or 443)` takes the
//! default for port 0, truncates a float, and raises on a string that is not a
//! number — which nothing catches.
//!
//! The output is written with **`ensure_ascii=False`**, like `import-merge.py`
//! and unlike `vless-parse.py`.

use crate::bplist::PlVal;
use crate::foreign::{Counter, Exc, R};
use crate::pyurl;
use crate::sharelink::{strip, RESERVED};
use serde_json::{Map, Value};

/// Python's type name, for an exception message.
pub fn type_name(v: &PlVal) -> &'static str {
    match v {
        PlVal::None => "NoneType",
        PlVal::Bool(_) => "bool",
        PlVal::Int(_) => "int",
        PlVal::Real(_) => "float",
        PlVal::Date(_) => "datetime.datetime",
        PlVal::Data(_) => "bytes",
        PlVal::Str(_) => "str",
        PlVal::Uid(_) => "UID",
        PlVal::Array(_) => "list",
        PlVal::Dict(_) => "dict",
    }
}

pub fn truthy(v: Option<&PlVal>) -> bool {
    match v {
        None | Some(PlVal::None) => false,
        Some(PlVal::Bool(b)) => *b,
        Some(PlVal::Int(n)) => *n != 0,
        Some(PlVal::Real(f)) => *f != 0.0,
        // A datetime and a UID have no __bool__ and no __len__, so both are
        // always true — including UID(0).
        Some(PlVal::Date(_)) | Some(PlVal::Uid(_)) => true,
        Some(PlVal::Data(d)) => !d.is_empty(),
        Some(PlVal::Str(s)) => !s.is_empty(),
        Some(PlVal::Array(a)) => !a.is_empty(),
        Some(PlVal::Dict(d)) => !d.is_empty(),
    }
}

/// `str(v)`.
pub fn py_str(v: &PlVal) -> String {
    match v {
        PlVal::Str(s) => s.clone(),
        other => py_repr(other),
    }
}

/// `repr(v)` — what `str()` falls back to, and what a container prints for its
/// elements.
pub fn py_repr(v: &PlVal) -> String {
    match v {
        PlVal::None => "None".into(),
        PlVal::Bool(b) => if *b { "True" } else { "False" }.into(),
        PlVal::Int(n) => n.to_string(),
        PlVal::Real(f) => crate::foreign::py_float_str(*f),
        PlVal::Date(d) => d.to_py_repr(),
        PlVal::Data(d) => crate::foreign::py_bytes_repr(d),
        PlVal::Str(s) => pyurl::repr(s),
        PlVal::Uid(u) => format!("UID({u})"),
        PlVal::Array(a) => {
            format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", "))
        }
        PlVal::Dict(d) => format!(
            "{{{}}}",
            d.iter()
                .map(|(k, v)| format!("{}: {}", py_repr(k), py_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// `str(v)` where a datetime prints its `str` form, not its `repr` — the two
/// differ, and only the top-level call gets the `str` one.
fn py_str_top(v: &PlVal) -> String {
    match v {
        PlVal::Date(d) => d.to_py_str(),
        other => py_str(other),
    }
}

/// `int(v)`.
pub fn py_int(v: &PlVal) -> R<i128> {
    match v {
        PlVal::Bool(b) => Ok(*b as i128),
        PlVal::Int(n) => Ok(*n),
        PlVal::Real(f) => {
            if f.is_nan() {
                Err(Exc::new("ValueError", "cannot convert float NaN to integer"))
            } else if f.is_infinite() {
                Err(Exc::new("OverflowError", "cannot convert float infinity to integer"))
            } else {
                Ok(f.trunc() as i128)
            }
        }
        PlVal::Str(s) => parse_py_int(s, &pyurl::repr(s)),
        // `int(b"12")` works: bytes are parsed as ASCII digits.
        PlVal::Data(d) => match std::str::from_utf8(d) {
            Ok(s) => parse_py_int(s, &crate::foreign::py_bytes_repr(d)),
            Err(_) => Err(Exc::new(
                "ValueError",
                format!(
                    "invalid literal for int() with base 10: {}",
                    crate::foreign::py_bytes_repr(d)
                ),
            )),
        },
        other => Err(Exc::new(
            "TypeError",
            format!(
                "int() argument must be a string, a bytes-like object or a real number, not '{}'",
                type_name(other)
            ),
        )),
    }
}

fn parse_py_int(s: &str, shown: &str) -> R<i128> {
    let t = strip(s);
    let (sign, digits) = match t.strip_prefix('-') {
        Some(d) => (-1i128, d),
        None => (1i128, t.strip_prefix('+').unwrap_or(t)),
    };
    let clean: String = digits.replace('_', "");
    let ok = !clean.is_empty()
        && clean.chars().all(|c| c.is_ascii_digit())
        && !digits.starts_with('_')
        && !digits.ends_with('_')
        && !digits.contains("__");
    match clean.parse::<i128>() {
        Ok(n) if ok => Ok(sign * n),
        _ => Err(Exc::new(
            "ValueError",
            format!("invalid literal for int() with base 10: {shown}"),
        )),
    }
}

/// A server entry: the plist dict with each value dereferenced one level.
pub type Entry = Vec<(PlVal, PlVal)>;

fn get<'a>(s: &'a Entry, k: &str) -> Option<&'a PlVal> {
    s.iter().find(|(ek, _)| matches!(ek, PlVal::Str(n) if n == k)).map(|(_, v)| v)
}

/// `a or b`, over `.get()` results.
fn or2<'a>(a: Option<&'a PlVal>, b: Option<&'a PlVal>) -> Option<&'a PlVal> {
    if truthy(a) {
        a
    } else {
        b
    }
}

/// `str(x or default)` — TRUTHINESS, so a present-but-empty value takes the
/// default.
fn str_or(a: Option<&PlVal>, default: &str) -> String {
    if truthy(a) {
        py_str_top(a.unwrap())
    } else {
        default.to_string()
    }
}

/// `str(x)` — PRESENCE, so a missing key is the text `None` and a present empty
/// string stays empty. The distinction matters at `str(s.get("type"))`, where a
/// `type` of `""` must not read as `None`.
fn str_of(a: Option<&PlVal>) -> String {
    match a {
        Some(v) => py_str_top(v),
        None => "None".into(),
    }
}

/// `_sanitize` — a display name into a tag no other outbound has taken.
///
/// The character class keeps only `[A-Za-z0-9._-]`, so a Chinese node name
/// collapses to its punctuation and then to `sr-N`. A tag that lands on a
/// reserved lane name is suffixed with the index rather than rejected.
pub fn sanitize(name: &str, index: usize, used: &mut Vec<String>) -> String {
    let stripped = strip(name);
    let mut sub = String::with_capacity(stripped.len());
    let mut in_run = false;
    for c in stripped.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            sub.push(c);
            in_run = false;
        } else if !in_run {
            sub.push('-');
            in_run = true;
        }
    }
    let mut base = sub.trim_matches('-').to_string();
    if base.is_empty() {
        base = format!("sr-{index}");
    }
    if RESERVED.contains(&base.as_str()) {
        base = format!("{base}-{index}");
    }
    let mut tag = base.clone();
    let mut n = 2;
    while used.iter().any(|u| *u == tag) {
        tag = format!("{base}-{n}");
        n += 1;
    }
    used.push(tag.clone());
    tag
}

/// `_load_servers` — every dict in `$objects` that has both a `type` and a
/// `host`, with its values dereferenced.
pub fn load_servers(root: &PlVal) -> R<Vec<Entry>> {
    let objs = index_str(root, "$objects")?;
    let items = iterate(&objs)?;
    let mut out = Vec::new();
    for o in &items {
        let PlVal::Dict(d) = o else { continue };
        let has = |k: &str| d.iter().any(|(ek, _)| matches!(ek, PlVal::Str(n) if n == k));
        if !has("type") || !has("host") {
            continue;
        }
        let mut e: Entry = Vec::new();
        for (k, v) in d {
            e.push((k.clone(), deref(&objs, v)?));
        }
        out.push(e);
    }
    Ok(out)
}

/// `plist["$objects"]`.
fn index_str(v: &PlVal, key: &str) -> R<PlVal> {
    match v {
        PlVal::Dict(d) => match d.iter().find(|(k, _)| matches!(k, PlVal::Str(n) if n == key)) {
            Some((_, val)) => Ok(val.clone()),
            None => Err(Exc::new("KeyError", format!("'{key}'"))),
        },
        PlVal::Array(_) => Err(Exc::new(
            "TypeError",
            "list indices must be integers or slices, not str",
        )),
        PlVal::Str(_) => Err(Exc::new("TypeError", "string indices must be integers, not 'str'")),
        other => Err(Exc::new(
            "TypeError",
            format!("'{}' object is not subscriptable", type_name(other)),
        )),
    }
}

/// `for o in objs` — a mapping yields its keys, a string its characters, a
/// bytes its integers, and anything else is a TypeError.
fn iterate(v: &PlVal) -> R<Vec<PlVal>> {
    match v {
        PlVal::Array(a) => Ok(a.clone()),
        PlVal::Dict(d) => Ok(d.iter().map(|(k, _)| k.clone()).collect()),
        PlVal::Str(s) => Ok(s.chars().map(|c| PlVal::Str(c.to_string())).collect()),
        PlVal::Data(b) => Ok(b.iter().map(|&x| PlVal::Int(x as i128)).collect()),
        other => Err(Exc::new(
            "TypeError",
            format!("'{}' object is not iterable", type_name(other)),
        )),
    }
}

/// `d(x)` — one level through a `CF$UID`, then one more if what it points at is
/// an `NSString` wrapper. NOT recursive: a UID whose target is a dict of UIDs
/// leaves those alone, which is why `str()` of one can be `UID(3)`.
fn deref(objs: &PlVal, x: &PlVal) -> R<PlVal> {
    let v = match x {
        PlVal::Uid(u) => index_int(objs, *u)?,
        other => other.clone(),
    };
    if let PlVal::Dict(d) = &v {
        if let Some((_, inner)) =
            d.iter().find(|(k, _)| matches!(k, PlVal::Str(n) if n == "NS.string"))
        {
            return Ok(inner.clone());
        }
    }
    Ok(v)
}

/// `objs[i]`.
fn index_int(objs: &PlVal, i: u64) -> R<PlVal> {
    match objs {
        PlVal::Array(a) => match a.get(i as usize) {
            Some(v) => Ok(v.clone()),
            None => Err(Exc::new("IndexError", "list index out of range")),
        },
        PlVal::Dict(d) => match d.iter().find(|(k, _)| matches!(k, PlVal::Int(n) if *n == i as i128))
        {
            Some((_, v)) => Ok(v.clone()),
            None => Err(Exc::new("KeyError", i.to_string())),
        },
        PlVal::Str(s) => match s.chars().nth(i as usize) {
            Some(c) => Ok(PlVal::Str(c.to_string())),
            None => Err(Exc::new("IndexError", "string index out of range")),
        },
        PlVal::Data(b) => match b.get(i as usize) {
            Some(&x) => Ok(PlVal::Int(x as i128)),
            None => Err(Exc::new("IndexError", "index out of range")),
        },
        other => Err(Exc::new(
            "TypeError",
            format!("'{}' object is not subscriptable", type_name(other)),
        )),
    }
}

/// `int(port or 443)` — port 0 is falsy, so it becomes 443 rather than 0.
fn port_of(s: &Entry) -> R<i64> {
    let p = get(s, "port");
    let n = if truthy(p) { py_int(p.unwrap())? } else { 443 };
    // A port outside i64 is a Python bignum in the review file; the review is
    // built with serde_json, so it clamps. No plist writes one.
    Ok(n.clamp(i64::MIN as i128, i64::MAX as i128) as i64)
}

/// `_to_vless`.
pub fn to_vless(s: &Entry, tag: &str) -> R<Option<Value>> {
    let host = get(s, "host");
    // The real VLESS id is in `password`, the generic slot; `uuid` is
    // Shadowrocket's own config id, and is only the fallback.
    let vid = or2(get(s, "password"), get(s, "uuid"));
    if !truthy(host) || !truthy(vid) {
        return Ok(None);
    }
    let host = host.unwrap();
    let mut out = Map::new();
    out.insert("type".into(), Value::String("vless".into()));
    out.insert("tag".into(), Value::String(tag.into()));
    out.insert("server".into(), Value::String(py_str_top(host)));
    out.insert("server_port".into(), Value::from(port_of(s)?));
    out.insert("uuid".into(), Value::String(py_str_top(vid.unwrap())));
    if str_of(get(s, "xtls")) == "2" {
        out.insert("flow".into(), Value::String("xtls-rprx-vision".into()));
    }
    let sni = py_str_top(if truthy(get(s, "peer")) { get(s, "peer").unwrap() } else { host });
    let pbk = get(s, "publicKey");
    if truthy(pbk) {
        let mut reality = Map::new();
        reality.insert("enabled".into(), Value::Bool(true));
        reality.insert("public_key".into(), Value::String(py_str_top(pbk.unwrap())));
        reality.insert("short_id".into(), Value::String(str_or(get(s, "shortId"), "")));
        out.insert("tls".into(), tls_block(&sni, Some(reality), None));
    } else if truthy(get(s, "tls")) {
        out.insert("tls".into(), tls_block(&sni, None, None));
    }
    let obfs = str_or(get(s, "obfs"), "none").to_lowercase();
    if obfs == "ws" || obfs == "websocket" {
        let mut t = Map::new();
        t.insert("type".into(), Value::String("ws".into()));
        t.insert(
            "path".into(),
            Value::String(str_or(or2(get(s, "obfsParam"), get(s, "pluginParam")), "/")),
        );
        out.insert("transport".into(), Value::Object(t));
    } else if obfs == "grpc" {
        let mut t = Map::new();
        t.insert("type".into(), Value::String("grpc".into()));
        t.insert("service_name".into(), Value::String(str_or(get(s, "obfsParam"), "")));
        out.insert("transport".into(), Value::Object(t));
    }
    Ok(Some(Value::Object(out)))
}

/// `_to_anytls`.
pub fn to_anytls(s: &Entry, tag: &str) -> R<Option<Value>> {
    let host = get(s, "host");
    let pw = get(s, "password");
    if !truthy(host) || !truthy(pw) {
        return Ok(None);
    }
    let host = host.unwrap();
    let sni = py_str_top(if truthy(get(s, "peer")) { get(s, "peer").unwrap() } else { host });
    let mut out = Map::new();
    out.insert("type".into(), Value::String("anytls".into()));
    out.insert("tag".into(), Value::String(tag.into()));
    out.insert("server".into(), Value::String(py_str_top(host)));
    out.insert("server_port".into(), Value::from(port_of(s)?));
    out.insert("password".into(), Value::String(py_str_top(pw.unwrap())));
    // `bool("0")` is True — a string flag in the store means "on" whatever it
    // says.
    out.insert("tls".into(), tls_block(&sni, None, Some(truthy(get(s, "allowInsecure")))));
    Ok(Some(Value::Object(out)))
}

/// The shared `tls` object, in the key order each caller writes it.
fn tls_block(sni: &str, reality: Option<Map<String, Value>>, insecure: Option<bool>) -> Value {
    let mut tls = Map::new();
    tls.insert("enabled".into(), Value::Bool(true));
    tls.insert("server_name".into(), Value::String(sni.into()));
    if let Some(i) = insecure {
        tls.insert("insecure".into(), Value::Bool(i));
    }
    let mut utls = Map::new();
    utls.insert("enabled".into(), Value::Bool(true));
    utls.insert("fingerprint".into(), Value::String("chrome".into()));
    tls.insert("utls".into(), Value::Object(utls));
    if let Some(r) = reality {
        tls.insert("reality".into(), Value::Object(r));
    }
    Value::Object(tls)
}

/// `_parse_rules` — the `[Rule]` section's `…,PROXY` domain rules, deduped,
/// order kept.
pub fn parse_rules(text: &str) -> Vec<String> {
    let mut doms: Vec<String> = Vec::new();
    let mut in_rules = false;
    for raw in universal_lines(text) {
        let line = strip(&raw).to_string();
        if line.starts_with('[') {
            in_rules = line.to_lowercase().starts_with("[rule]");
            continue;
        }
        if !in_rules || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split(',').map(strip).collect();
        if parts.len() < 3 {
            continue;
        }
        let typ = parts[0].to_uppercase();
        let value = parts[1];
        let action = parts[parts.len() - 1].to_uppercase();
        if action == "PROXY" && (typ == "DOMAIN-SUFFIX" || typ == "DOMAIN") {
            doms.push(value.to_lowercase());
        }
    }
    let mut seen: Vec<String> = Vec::new();
    doms.into_iter()
        .filter(|d| {
            if seen.contains(d) {
                false
            } else {
                seen.push(d.clone());
                true
            }
        })
        .collect()
}

/// Iterating a text-mode file splits on `\n`, `\r\n` AND a bare `\r`.
fn universal_lines(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' => {
                out.push(std::mem::take(&mut cur));
            }
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push(std::mem::take(&mut cur));
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// What one pass over the store yields.
#[derive(Default)]
pub struct Scan {
    pub subscriptions: Vec<Value>,
    pub servers: Vec<Value>,
    pub skipped: Counter,
}

/// The loop over `$objects`: subscriptions, convertible servers, and a count of
/// everything else by its `type`.
///
/// Kept apart from [`assemble`] because the Python interleaves I/O with output:
/// the "no server store" warning is printed BEFORE the rule file is opened, so
/// a store that is missing and a rule file that cannot be read produce a
/// warning and then a traceback, in that order.
pub fn scan_store(root: &PlVal) -> R<Scan> {
    let mut out = Scan::default();
    let entries = load_servers(root)?;
    let mut used: Vec<String> = RESERVED.iter().map(|s| s.to_string()).collect();
    for (i, s) in entries.iter().enumerate() {
        let index = i + 1;
        let typ = str_of(get(s, "type"));
        if typ == "Subscribe" {
            let url = str_or(get(s, "host"), "");
            if url.starts_with("http") {
                let mut e = Map::new();
                e.insert("url".into(), Value::String(url));
                e.insert("info".into(), Value::String(str_or(get(s, "data"), "")));
                e.insert("title".into(), Value::String(str_or(get(s, "title"), "")));
                out.subscriptions.push(Value::Object(e));
            }
        } else if typ == "VLESS" || typ == "AnyTLS" {
            // The tag is taken BEFORE the conversion, so a server that cannot
            // be converted still burns its name — the same coupling
            // `sharelink::parse_many` has.
            let name = str_of(or2(get(s, "title"), get(s, "host")));
            let tag = sanitize(&name, index, &mut used);
            let v = if typ == "VLESS" { to_vless(s, &tag)? } else { to_anytls(s, &tag)? };
            match v {
                Some(v) => out.servers.push(v),
                None => out.skipped.incr(&typ),
            }
        } else {
            out.skipped.incr(&typ);
        }
    }
    Ok(out)
}

/// The review literal. All four keys exist from the start in this order, and
/// `skipped` is only assigned in the store branch — which an empty counter
/// renders the same way the initial `{}` does.
pub fn assemble(scan: Option<Scan>, proxy_domains: Vec<String>) -> Value {
    let scan = scan.unwrap_or_default();
    let mut out = Map::new();
    out.insert("subscriptions".into(), Value::Array(scan.subscriptions));
    out.insert("servers".into(), Value::Array(scan.servers));
    out.insert(
        "proxy_domains".into(),
        Value::Array(proxy_domains.into_iter().map(Value::String).collect()),
    );
    out.insert("skipped".into(), scan.skipped.to_value());
    Value::Object(out)
}

/// `json.dump(result, sys.stdout, indent=2, ensure_ascii=False)` — readable
/// CJK, unlike `vless-parse.py`'s writer.
pub fn render(v: &Value) -> String {
    format!("{}\n", serde_json::to_string_pretty(v).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bplist::Dt;
    use serde_json::json;

    fn entry(pairs: &[(&str, PlVal)]) -> Entry {
        pairs.iter().map(|(k, v)| (PlVal::Str(k.to_string()), v.clone())).collect()
    }
    fn s(x: &str) -> PlVal {
        PlVal::Str(x.into())
    }

    #[test]
    fn the_vless_id_comes_from_password_not_uuid() {
        let e = entry(&[
            ("type", s("VLESS")),
            ("host", s("192.0.2.10")),
            ("password", s("the-real-id")),
            ("uuid", s("shadowrockets-own-id")),
        ]);
        let o = to_vless(&e, "T").unwrap().unwrap();
        assert_eq!(o["uuid"], json!("the-real-id"));
        // …and `uuid` is the fallback, not dead code.
        let e = entry(&[("type", s("VLESS")), ("host", s("h")), ("uuid", s("fallback"))]);
        assert_eq!(to_vless(&e, "T").unwrap().unwrap()["uuid"], json!("fallback"));
    }

    #[test]
    fn port_zero_takes_the_default() {
        let e = entry(&[("host", s("h")), ("password", s("p")), ("port", PlVal::Int(0))]);
        assert_eq!(to_anytls(&e, "T").unwrap().unwrap()["server_port"], json!(443));
        let e = entry(&[("host", s("h")), ("password", s("p")), ("port", s("8443"))]);
        assert_eq!(to_anytls(&e, "T").unwrap().unwrap()["server_port"], json!(8443));
        // A float truncates rather than rounds…
        let e = entry(&[("host", s("h")), ("password", s("p")), ("port", PlVal::Real(8443.9))]);
        assert_eq!(to_anytls(&e, "T").unwrap().unwrap()["server_port"], json!(8443));
        // …and a string that is not a number ends the run.
        let e = entry(&[("host", s("h")), ("password", s("p")), ("port", s("abc"))]);
        assert_eq!(to_anytls(&e, "T").unwrap_err().name, "ValueError");
    }

    #[test]
    fn allow_insecure_is_truthiness_so_the_string_zero_means_yes() {
        let e = entry(&[("host", s("h")), ("password", s("p")), ("allowInsecure", s("0"))]);
        assert_eq!(to_anytls(&e, "T").unwrap().unwrap()["tls"]["insecure"], json!(true));
        let e = entry(&[("host", s("h")), ("password", s("p")), ("allowInsecure", PlVal::Int(0))]);
        assert_eq!(to_anytls(&e, "T").unwrap().unwrap()["tls"]["insecure"], json!(false));
    }

    #[test]
    fn xtls_two_is_a_string_comparison() {
        let e = entry(&[("host", s("h")), ("password", s("p")), ("xtls", PlVal::Int(2))]);
        assert_eq!(to_vless(&e, "T").unwrap().unwrap()["flow"], json!("xtls-rprx-vision"));
        // `str(2.0)` is "2.0", so a real never matches.
        let e = entry(&[("host", s("h")), ("password", s("p")), ("xtls", PlVal::Real(2.0))]);
        assert!(to_vless(&e, "T").unwrap().unwrap().get("flow").is_none());
    }

    #[test]
    fn the_sni_falls_back_from_peer_to_the_host() {
        let e = entry(&[
            ("host", s("192.0.2.10")),
            ("password", s("p")),
            ("publicKey", s("PBK")),
            ("peer", s("")),
        ]);
        let o = to_vless(&e, "T").unwrap().unwrap();
        assert_eq!(o["tls"]["server_name"], json!("192.0.2.10"));
        assert_eq!(o["tls"]["reality"]["short_id"], json!(""));
    }

    #[test]
    fn a_reality_key_wins_over_plain_tls() {
        let e = entry(&[
            ("host", s("h")),
            ("password", s("p")),
            ("publicKey", s("PBK")),
            ("tls", PlVal::Bool(true)),
        ]);
        let o = to_vless(&e, "T").unwrap().unwrap();
        assert!(o["tls"]["reality"]["enabled"] == json!(true));
    }

    #[test]
    fn the_websocket_path_falls_back_through_two_fields() {
        let e = entry(&[
            ("host", s("h")),
            ("password", s("p")),
            ("obfs", s("WebSocket")),
            ("pluginParam", s("/plugin")),
        ]);
        let o = to_vless(&e, "T").unwrap().unwrap();
        assert_eq!(o["transport"], json!({"type": "ws", "path": "/plugin"}));
        let e = entry(&[("host", s("h")), ("password", s("p")), ("obfs", s("grpc"))]);
        assert_eq!(
            to_vless(&e, "T").unwrap().unwrap()["transport"],
            json!({"type": "grpc", "service_name": ""})
        );
    }

    #[test]
    fn sanitize_collapses_runs_and_falls_back_to_the_index() {
        let mut used: Vec<String> = RESERVED.iter().map(|s| s.to_string()).collect();
        assert_eq!(sanitize("HK 01 !!", 1, &mut used), "HK-01");
        assert_eq!(sanitize("东京", 3, &mut used), "sr-3");
        assert_eq!(sanitize("HK 01 !!", 4, &mut used), "HK-01-2");
        // A reserved name takes its index, then uniquifies from there.
        assert_eq!(sanitize("escape", 5, &mut used), "escape-5");
        assert_eq!(sanitize("--x--", 6, &mut used), "x");
    }

    #[test]
    fn str_of_a_plist_value_is_pythons_str() {
        assert_eq!(py_str(&PlVal::Uid(3)), "UID(3)");
        assert_eq!(py_str(&PlVal::Data(b"ab".to_vec())), "b'ab'");
        assert_eq!(py_str(&PlVal::None), "None");
        assert_eq!(py_str(&PlVal::Real(2.0)), "2.0");
        assert_eq!(
            py_str_top(&PlVal::Date(Dt { y: 2026, mo: 8, d: 9, h: 1, mi: 2, s: 3, us: 0 })),
            "2026-08-09 01:02:03"
        );
        assert_eq!(
            py_str(&PlVal::Array(vec![s("a"), PlVal::Uid(1)])),
            "['a', UID(1)]"
        );
    }

    #[test]
    fn load_servers_dereferences_one_level_and_unwraps_nsstring() {
        // One array serves as both the thing walked and the thing indexed, as
        // in a real archive: the server dict sits at index 4, its values are
        // UIDs into the same array, and index 3 is an NSString wrapper.
        let root = PlVal::Dict(vec![(
            s("$objects"),
            PlVal::Array(vec![
                PlVal::None,
                s("VLESS"),
                PlVal::Dict(vec![(s("inner"), PlVal::Uid(1))]),
                PlVal::Dict(vec![(s("NS.string"), s("wrapped.example.com"))]),
                PlVal::Dict(vec![
                    (s("type"), PlVal::Uid(1)),
                    (s("host"), PlVal::Uid(3)),
                    (s("nested"), PlVal::Uid(2)),
                ]),
            ]),
        )]);
        let got = load_servers(&root).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(get(&got[0], "type"), Some(&s("VLESS")));
        // The NSString wrapper is unwrapped…
        assert_eq!(get(&got[0], "host"), Some(&s("wrapped.example.com")));
        // …but the dereference is ONE level, so a UID inside the target stays a
        // UID and would print as `UID(1)`.
        assert_eq!(
            get(&got[0], "nested"),
            Some(&PlVal::Dict(vec![(s("inner"), PlVal::Uid(1))]))
        );
    }

    #[test]
    fn a_uid_pointing_off_the_end_of_objects_is_an_index_error() {
        let root = PlVal::Dict(vec![(
            s("$objects"),
            PlVal::Array(vec![PlVal::Dict(vec![
                (s("type"), PlVal::Uid(9)),
                (s("host"), s("h")),
            ])]),
        )]);
        assert_eq!(load_servers(&root).unwrap_err().name, "IndexError");
    }

    #[test]
    fn a_missing_objects_key_is_a_key_error() {
        let e = load_servers(&PlVal::Dict(vec![(s("$top"), PlVal::None)])).unwrap_err();
        assert_eq!(e.name, "KeyError");
        let e = load_servers(&PlVal::Array(vec![])).unwrap_err();
        assert_eq!(e.name, "TypeError");
    }

    #[test]
    fn rules_take_only_proxy_domains_from_the_rule_section() {
        let text = "[General]\nDOMAIN,skip.example.com,PROXY\n[Rule]\n# c\n\
                    DOMAIN-SUFFIX,Example.COM,PROXY\nDOMAIN,a.example.net,DIRECT\n\
                    IP-CIDR,192.0.2.0/24,PROXY\ndomain,b.example.org,proxy\n\
                    DOMAIN,example.com,PROXY\n[Host]\nDOMAIN,after.example,PROXY\n";
        assert_eq!(parse_rules(text), ["example.com", "b.example.org"]);
    }

    #[test]
    fn a_bare_carriage_return_still_ends_a_line() {
        assert_eq!(parse_rules("[Rule]\rDOMAIN,a.example,PROXY\r"), ["a.example"]);
    }

    #[test]
    fn a_missing_store_leaves_every_section_empty_in_the_declared_order() {
        assert_eq!(
            assemble(None, vec![]),
            json!({"subscriptions": [], "servers": [], "proxy_domains": [], "skipped": {}})
        );
    }

    #[test]
    fn a_skipped_conversion_still_consumes_its_tag() {
        // Two VLESS entries with the same title; the first has no id, so it is
        // counted under `skipped` — but it has already taken the name, and the
        // second one gets the `-2`.
        let objs = PlVal::Array(vec![
            PlVal::Dict(vec![(s("type"), s("VLESS")), (s("host"), s("h")), (s("title"), s("Node"))]),
            PlVal::Dict(vec![
                (s("type"), s("VLESS")),
                (s("host"), s("h")),
                (s("title"), s("Node")),
                (s("password"), s("p")),
            ]),
        ]);
        let root = PlVal::Dict(vec![(s("$objects"), objs)]);
        let v = assemble(Some(scan_store(&root).unwrap()), vec![]);
        assert_eq!(v["skipped"], json!({"VLESS": 1}));
        let tags: Vec<&str> =
            v["servers"].as_array().unwrap().iter().map(|x| x["tag"].as_str().unwrap()).collect();
        assert_eq!(tags, ["Node-2"]);
    }
}
