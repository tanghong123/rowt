//! Other proxy clients into rowt's review format — a port of
//! `config/foreign-import.py`.
//!
//! Three sources, one shape: Clash Verge and FlClash keep Clash YAML profiles
//! (read through `yq`), V2Box keeps share links in a Core Data SQLite store.
//! Everything converges on a share-link string, which `sharelink::parse_many`
//! turns into an outbound — so the protocols rowt does not speak never need a
//! second parser, they are counted under `skipped` and dropped.
//!
//! The I/O lives in `bin/rowt-foreign-import.rs` behind [`Fs`]; what is here is
//! the part with semantics: the Clash-proxy → URI conversion, the review build,
//! and the Python coercions the two sit on.
//!
//! Two things this module does NOT unify, on purpose (PORTING.md §6.7):
//!
//!   * `norm_sub` is a near-copy of [`crate::importmerge::norm_sub`]. The two
//!     Pythons differ by one token — `except Exception` here, `except ValueError`
//!     there — and sharing one function would be a port deciding which of the
//!     two the author meant.
//!   * the review is written with **`ensure_ascii=True`** (`pyjson::dumps`),
//!     while `import-merge.py`, which reads what this writes, uses `False`. The
//!     pipeline really does escape a CJK node name here and unescape it one step
//!     later.
//!
//! ## Exceptions are part of the contract
//!
//! Plenty of paths here die with a traceback rather than an error message: a
//! `reality-opts:` that is a string, a `ZURL` column holding a BLOB, a
//! `profiles.yaml` whose top level is a list. Those are reproduced as [`Exc`]
//! rather than tidied into a clean failure, because a port that *succeeds* where
//! the Python crashes imports garbage into a server pool. The gate compares exit
//! status and stdout exactly and the exception TYPE by name; the traceback body
//! itself is normalized away on both sides.

use crate::pyjson;
use crate::pypath;
use crate::pyurl;
use crate::sharelink::{self, key_of, py_str, strip};
use serde_json::{Map, Value};
use std::collections::HashSet;

/// An exception that reached the top level. `name` is the qualified class name
/// exactly as Python prints it on the last line of a traceback.
#[derive(Debug, Clone)]
pub struct Exc {
    pub name: String,
    pub msg: String,
}

pub type R<T> = Result<T, Exc>;

impl Exc {
    pub fn new(name: &str, msg: impl Into<String>) -> Self {
        Exc { name: name.to_string(), msg: msg.into() }
    }
    /// `'<type>' object has no attribute '<attr>'` — what `.get` on a
    /// non-mapping raises, which is how a hand-written `ws-opts: "/path"` ends
    /// the run.
    pub fn attr(ty: &str, attr: &str) -> Self {
        Exc::new("AttributeError", format!("'{ty}' object has no attribute '{attr}'"))
    }
    pub fn not_iterable(ty: &str) -> Self {
        Exc::new("TypeError", format!("'{ty}' object is not iterable"))
    }
    /// `main` catches only this one, and turns it into `error: …` + exit 1.
    pub fn is_runtime(&self) -> bool {
        self.name == "RuntimeError"
    }
}

/// The Python type name of a JSON value, for an exception message.
pub fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) => {
            if n.is_f64() {
                "float"
            } else {
                "int"
            }
        }
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// Truthiness, with a missing key reading as `None`.
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(m)) => !m.is_empty(),
    }
}

/// `a or b`, where a missing key is `None`.
fn or2<'a>(a: Option<&'a Value>, b: Option<&'a Value>) -> Option<&'a Value> {
    if truthy(a) {
        a
    } else {
        b
    }
}

/// `str(d.get(k, default))`.
fn str_or(p: &Map<String, Value>, k: &str, default: &str) -> String {
    match p.get(k) {
        Some(v) => py_str(v),
        None => default.to_string(),
    }
}

/// `(x or {}).get(k)` — including the `AttributeError` when `x` is truthy but
/// has no `.get`, which is the only way a malformed `reality-opts:` is ever
/// noticed.
fn dget(x: Option<&Value>, k: &str) -> R<Option<Value>> {
    if !truthy(x) {
        return Ok(None);
    }
    match x.unwrap() {
        Value::Object(m) => Ok(m.get(k).cloned()),
        other => Err(Exc::attr(type_name(other), "get")),
    }
}

/// `(x or {}).get(k, default)`.
fn dget_or(x: Option<&Value>, k: &str, default: Value) -> R<Value> {
    if !truthy(x) {
        return Ok(default);
    }
    match x.unwrap() {
        Value::Object(m) => Ok(m.get(k).cloned().unwrap_or(default)),
        other => Err(Exc::attr(type_name(other), "get")),
    }
}

/// A `Counter`, which is a dict: `dict(counter)` comes out in the order the
/// keys were first counted, and the review file's `skipped` is compared as
/// bytes.
#[derive(Debug, Default, Clone)]
pub struct Counter(Vec<(String, i64)>);

impl Counter {
    pub fn new() -> Self {
        Counter(Vec::new())
    }
    pub fn incr(&mut self, k: &str) {
        match self.0.iter_mut().find(|(n, _)| n == k) {
            Some((_, c)) => *c += 1,
            None => self.0.push((k.to_string(), 1)),
        }
    }
    pub fn get(&self, k: &str) -> i64 {
        self.0.iter().find(|(n, _)| n == k).map(|(_, c)| *c).unwrap_or(0)
    }
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        for (k, c) in &self.0 {
            m.insert(k.clone(), Value::from(*c));
        }
        Value::Object(m)
    }
}

// ---------------------------------------------------------------------------
// Clash proxy → share link
// ---------------------------------------------------------------------------

/// `q_str` — the query dict, minus the entries that are `None` or `""`, through
/// `urlencode` (i.e. `quote_plus`, so a space is `+` here and `%20` in the
/// fragment two characters later).
fn q_str(q: &[(&str, Option<Value>)]) -> String {
    let pairs: Vec<(String, String)> = q
        .iter()
        .filter(|(_, v)| match v {
            None => false,
            Some(Value::String(s)) => !s.is_empty(),
            Some(_) => true,
        })
        .map(|(k, v)| (k.to_string(), py_str(v.as_ref().unwrap())))
        .collect();
    pyurl::urlencode(&pairs)
}

/// `v` as a query value: a missing key and an explicit `null` are both Python's
/// `None`, which `q_str` drops.
fn qv(v: Option<&Value>) -> Option<Value> {
    match v {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.clone()),
    }
}

fn qvo(v: Option<Value>) -> Option<Value> {
    match v {
        None | Some(Value::Null) => None,
        Some(v) => Some(v),
    }
}

/// A Clash/Mihomo proxy dict → a share-link URI, or `None` for a protocol rowt
/// cannot use.
pub fn clash_proxy_to_link(p: &Map<String, Value>) -> R<Option<String>> {
    let t = str_or(p, "type", "").to_lowercase();
    let name = strip(&str_or(p, "name", "")).to_string();
    let server = str_or(p, "server", "");
    let port = str_or(p, "port", "");
    // `quote`, not `quote_plus`: a space in a node name is `%20`.
    let frag = if name.is_empty() { String::new() } else { format!("#{}", pyurl::quote(&name, "/")) };

    if t == "vless" {
        let net = str_or(p, "network", "tcp").to_lowercase();
        let ro = p.get("reality-opts");
        let security = if truthy(ro) {
            "reality"
        } else if truthy(p.get("tls")) {
            "tls"
        } else {
            "none"
        };
        let mut q: Vec<(&str, Option<Value>)> = vec![
            ("type", Some(Value::String(net.clone()))),
            ("security", Some(Value::String(security.into()))),
            ("sni", qv(or2(p.get("servername"), p.get("sni")))),
            ("fp", qv(p.get("client-fingerprint"))),
            ("flow", qv(p.get("flow"))),
            ("pbk", qvo(dget(ro, "public-key")?)),
            ("sid", qvo(dget(ro, "short-id")?)),
        ];
        if truthy(p.get("skip-cert-verify")) {
            q.push(("insecure", Some(Value::String("1".into()))));
        }
        if net == "ws" || net == "websocket" {
            let ws = p.get("ws-opts");
            q.push(("path", qvo(Some(dget_or(ws, "path", Value::String("/".into()))?))));
            let hdr = dget(ws, "headers")?;
            let host = {
                let a = dget(hdr.as_ref(), "Host")?;
                if truthy(a.as_ref()) {
                    a
                } else {
                    dget(hdr.as_ref(), "host")?
                }
            };
            q.push(("host", qvo(host)));
        } else if net == "grpc" {
            q.push(("serviceName", qvo(dget(p.get("grpc-opts"), "grpc-service-name")?)));
        }
        let uuid = str_or(p, "uuid", "");
        return Ok(Some(format!(
            "vless://{}@{server}:{port}?{}{frag}",
            pyurl::quote(&uuid, "/"),
            q_str(&q)
        )));
    }

    if t == "vmess" {
        let net = str_or(p, "network", "tcp").to_lowercase();
        let mut j = Map::new();
        j.insert("v".into(), Value::String("2".into()));
        j.insert("ps".into(), Value::String(name.clone()));
        j.insert("add".into(), Value::String(server.clone()));
        j.insert("port".into(), Value::String(port.clone()));
        // NOT stringified — a numeric `uuid:` in the YAML stays a JSON number
        // inside the vmess payload.
        j.insert("id".into(), p.get("uuid").cloned().unwrap_or(Value::String(String::new())));
        // The default of the OUTER get is evaluated either way; only a missing
        // `alterId` reaches for `alter-id`.
        let aid = match p.get("alterId") {
            Some(v) => v.clone(),
            None => p.get("alter-id").cloned().unwrap_or(Value::from(0)),
        };
        let aid = if truthy(Some(&aid)) { aid } else { Value::from(0) };
        j.insert("aid".into(), Value::String(py_str(&aid)));
        let cipher = p.get("cipher").cloned().unwrap_or(Value::String("auto".into()));
        let cipher = if truthy(Some(&cipher)) { cipher } else { Value::String("auto".into()) };
        j.insert("scy".into(), cipher);
        j.insert("net".into(), Value::String(net.clone()));
        if net == "ws" || net == "websocket" {
            let ws = p.get("ws-opts");
            j.insert("path".into(), dget_or(ws, "path", Value::String("/".into()))?);
            let hdr = dget(ws, "headers")?;
            let a = dget(hdr.as_ref(), "Host")?;
            let host = if truthy(a.as_ref()) {
                a.unwrap()
            } else {
                let b = dget(hdr.as_ref(), "host")?;
                if truthy(b.as_ref()) {
                    b.unwrap()
                } else {
                    Value::String(String::new())
                }
            };
            j.insert("host".into(), host);
        } else if net == "grpc" {
            j.insert(
                "path".into(),
                dget_or(p.get("grpc-opts"), "grpc-service-name", Value::String(String::new()))?,
            );
        }
        if truthy(p.get("tls")) {
            j.insert("tls".into(), Value::String("tls".into()));
            let sni = or2(p.get("servername"), p.get("sni"));
            let sni = if truthy(sni) { sni.unwrap().clone() } else { Value::String(String::new()) };
            j.insert("sni".into(), sni);
        }
        let payload = pyjson::dumps_flat(&Value::Object(j));
        return Ok(Some(format!("vmess://{}", b64encode(payload.as_bytes()))));
    }

    if t == "hysteria2" {
        let pw = or2(p.get("password"), p.get("auth"));
        let pw = if truthy(pw) { py_str(pw.unwrap()) } else { String::new() };
        // `sni` first here, `servername` first in the vless branch — the two
        // orders are the Python's, not a typo being carried forward blind.
        let mut q: Vec<(&str, Option<Value>)> = vec![("sni", qv(or2(p.get("sni"), p.get("servername"))))];
        if truthy(p.get("skip-cert-verify")) {
            q.push(("insecure", Some(Value::String("1".into()))));
        }
        if truthy(p.get("obfs")) {
            q.push(("obfs", qv(p.get("obfs"))));
            let ob = p.get("obfs-password").cloned().unwrap_or(Value::String(String::new()));
            q.push(("obfs-password", qvo(Some(ob))));
        }
        return Ok(Some(format!(
            "hysteria2://{}@{server}:{port}?{}{frag}",
            pyurl::quote(&pw, "/"),
            q_str(&q)
        )));
    }

    if t == "anytls" {
        let pw = p.get("password");
        let pw = if truthy(pw) { py_str(pw.unwrap()) } else { String::new() };
        let mut q: Vec<(&str, Option<Value>)> = vec![("sni", qv(or2(p.get("sni"), p.get("servername"))))];
        if truthy(p.get("skip-cert-verify")) {
            q.push(("insecure", Some(Value::String("1".into()))));
        }
        return Ok(Some(format!(
            "anytls://{}@{server}:{port}?{}{frag}",
            pyurl::quote(&pw, "/"),
            q_str(&q)
        )));
    }

    Ok(None) // trojan / ss / ssr / tuic / wireguard / … — rowt cannot use these
}

/// `base64.b64encode` — standard alphabet, padded.
fn b64encode(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(A[(n >> 18) as usize & 63] as char);
        out.push(A[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { A[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { A[n as usize & 63] as char } else { '=' });
    }
    out
}

/// `_links_from_clash_proxies` — the `proxies:` list, with what rowt cannot use
/// counted by protocol instead of dropped silently.
pub fn links_from_clash_proxies(proxies: Option<&Value>, skipped: &mut Counter) -> R<Vec<String>> {
    let mut links = Vec::new();
    if !truthy(proxies) {
        return Ok(links); // `or []`
    }
    // Iterating a mapping yields its keys and a string yields its characters;
    // neither is a dict, so both come out as zero links rather than an error.
    let items: Vec<Value> = match proxies.unwrap() {
        Value::Array(a) => a.clone(),
        Value::Object(m) => m.keys().map(|k| Value::String(k.clone())).collect(),
        Value::String(s) => s.chars().map(|c| Value::String(c.to_string())).collect(),
        other => return Err(Exc::not_iterable(type_name(other))),
    };
    for p in &items {
        let Value::Object(m) = p else { continue };
        match clash_proxy_to_link(m)? {
            Some(link) => links.push(link),
            None => skipped.incr(&str_or(m, "type", "unknown").to_lowercase()),
        }
    }
    Ok(links)
}

// ---------------------------------------------------------------------------
// The Clash profile directory
// ---------------------------------------------------------------------------

/// What the importer needs from the machine. Real implementation in the binary;
/// the tests below drive it from a table so the directory shapes are pinned
/// without a temp tree.
pub trait Fs {
    fn exists(&self, path: &str) -> bool;
    fn is_dir(&self, path: &str) -> bool;
    /// `sorted(root.rglob("*.y*ml"))` — directories whose own name matches are
    /// included, as `rglob` includes them.
    fn rglob_yaml(&self, root: &str) -> Vec<String>;
    /// `_yq_json(path)`.
    fn yq_json(&self, path: &str) -> R<Value>;
}

/// `PurePosixPath.__truediv__` — an absolute right-hand side replaces the left.
pub fn join(a: &str, b: &str) -> String {
    if b.starts_with('/') {
        return pypath::path_str(b);
    }
    let a = pypath::path_str(a);
    if a == "." {
        return pypath::path_str(b);
    }
    pypath::path_str(&format!("{a}/{b}"))
}

/// `import_clash_dir` — a Clash Verge / FlClash profiles dir → (links, subs).
///
/// Two branches with different failure behaviour: with a `profiles.yaml` index
/// a `yq` problem is fatal, without one it is `except Exception: continue` — so
/// the same missing `yq` either stops the import or silently yields nothing,
/// depending on which client wrote the directory.
pub fn import_clash_dir<F: Fs>(
    fs: &F,
    root: &str,
    skipped: &mut Counter,
) -> R<(Vec<String>, Vec<Value>)> {
    let mut links: Vec<String> = Vec::new();
    let mut subs: Vec<Value> = Vec::new();
    let index = join(root, "profiles.yaml");
    let prof_dir = join(root, "profiles");

    if fs.exists(&index) && fs.is_dir(&prof_dir) {
        let doc = fs.yq_json(&index)?;
        let items = dget(Some(&doc), "items")?;
        let items = if truthy(items.as_ref()) { items.unwrap() } else { Value::Array(vec![]) };
        let iter: Vec<Value> = match &items {
            Value::Array(a) => a.clone(),
            Value::Object(m) => m.keys().map(|k| Value::String(k.clone())).collect(),
            Value::String(s) => s.chars().map(|c| Value::String(c.to_string())).collect(),
            other => return Err(Exc::not_iterable(type_name(other))),
        };
        for it in &iter {
            let Value::Object(m) = it else { continue };
            let typ = str_or(m, "type", "").to_lowercase();
            if typ == "remote" && truthy(m.get("url")) {
                let mut e = Map::new();
                e.insert("url".into(), m.get("url").cloned().unwrap());
                e.insert(
                    "name".into(),
                    or2(m.get("name"), m.get("uid")).cloned().unwrap_or(Value::Null),
                );
                subs.push(Value::Object(e));
            } else if typ == "local" && truthy(m.get("file")) {
                let file = m.get("file").unwrap();
                let Value::String(fname) = file else {
                    return Err(Exc::new(
                        "TypeError",
                        format!(
                            "unsupported operand type(s) for /: 'PosixPath' and '{}'",
                            type_name(file)
                        ),
                    ));
                };
                let f = join(&prof_dir, fname);
                if fs.exists(&f) {
                    let doc = fs.yq_json(&f)?;
                    let proxies = dget(Some(&doc), "proxies")?;
                    links.extend(links_from_clash_proxies(proxies.as_ref(), skipped)?);
                }
            }
        }
    } else {
        for f in fs.rglob_yaml(root) {
            // Only the load is guarded — a proxy that blows up the conversion
            // still ends the run.
            let Ok(doc) = fs.yq_json(&f) else { continue };
            if let Value::Object(m) = &doc {
                if truthy(m.get("proxies")) {
                    links.extend(links_from_clash_proxies(m.get("proxies"), skipped)?);
                }
            }
        }
    }
    Ok((links, subs))
}

// ---------------------------------------------------------------------------
// V2Box
// ---------------------------------------------------------------------------

/// A value out of SQLite, in Python's mapping of the five storage classes.
#[derive(Debug, Clone, PartialEq)]
pub enum PyVal {
    None,
    Int(i64),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
}

impl PyVal {
    fn truthy(&self) -> bool {
        match self {
            PyVal::None => false,
            PyVal::Int(n) => *n != 0,
            PyVal::Float(f) => *f != 0.0,
            PyVal::Str(s) => !s.is_empty(),
            PyVal::Bytes(b) => !b.is_empty(),
        }
    }
    fn type_name(&self) -> &'static str {
        match self {
            PyVal::None => "NoneType",
            PyVal::Int(_) => "int",
            PyVal::Float(_) => "float",
            PyVal::Str(_) => "str",
            PyVal::Bytes(_) => "bytes",
        }
    }
    /// `str(v)`.
    pub fn to_py_str(&self) -> String {
        match self {
            PyVal::None => "None".into(),
            PyVal::Int(n) => n.to_string(),
            PyVal::Float(f) => py_float_str(*f),
            PyVal::Str(s) => s.clone(),
            PyVal::Bytes(b) => py_bytes_repr(b),
        }
    }
}

/// `str(float)` — Python's shortest round-trip repr, which switches to the
/// exponent form outside `1e-5 .. 1e16` and always pads the exponent to two
/// digits with an explicit sign. Rust's own `{}` never uses an exponent and
/// drops the `.0`, so neither end of the range agrees by accident.
pub fn py_float_str(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    let neg = f < 0.0;
    let sci = format!("{:e}", f.abs());
    let (mant, exp) = sci.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let d = digits.len() as i32;
    let out = if exp < -4 || exp >= 16 {
        let mut m = String::new();
        m.push(digits.as_bytes()[0] as char);
        if d > 1 {
            m.push('.');
            m.push_str(&digits[1..]);
        }
        format!("{m}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
    } else if exp >= 0 {
        if d > exp + 1 {
            format!("{}.{}", &digits[..(exp + 1) as usize], &digits[(exp + 1) as usize..])
        } else {
            format!("{digits}{}.0", "0".repeat((exp + 1 - d) as usize))
        }
    } else {
        format!("0.{}{digits}", "0".repeat((-exp - 1) as usize))
    };
    if neg {
        format!("-{out}")
    } else {
        out
    }
}

/// `repr(bytes)` — which is what `str()` of a BLOB column gives you, `b'…'` and
/// all, and it goes straight into the `skipped` key.
pub fn py_bytes_repr(b: &[u8]) -> String {
    let quote = if b.contains(&b'\'') && !b.contains(&b'"') { '"' } else { '\'' };
    let mut out = String::from("b");
    out.push(quote);
    for &c in b {
        match c {
            b'\\' => out.push_str("\\\\"),
            c if c as char == quote => {
                out.push('\\');
                out.push(quote);
            }
            b'\t' => out.push_str("\\t"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            0x20..=0x7e => out.push(c as char),
            c => out.push_str(&format!("\\x{c:02x}")),
        }
    }
    out.push(quote);
    out
}

/// `(v or "").strip()` — which is a `str` unless the column held a BLOB, in
/// which case it is `bytes` and the `startswith` two lines later is a TypeError.
enum Stripped {
    Str(String),
    Bytes,
}

fn or_empty_strip(v: &PyVal) -> R<Stripped> {
    if !v.truthy() {
        return Ok(Stripped::Str(String::new()));
    }
    match v {
        PyVal::Str(s) => Ok(Stripped::Str(strip(s).to_string())),
        PyVal::Bytes(_) => Ok(Stripped::Bytes),
        other => Err(Exc::attr(other.type_name(), "strip")),
    }
}

fn startswith_str(s: &Stripped, prefixes: &[&str]) -> R<bool> {
    match s {
        Stripped::Str(s) => Ok(prefixes.iter().any(|p| s.starts_with(p))),
        Stripped::Bytes => Err(Exc::new(
            "TypeError",
            "startswith first arg must be bytes or a tuple of bytes, not str",
        )),
    }
}

const V2BOX_LINKS: [&str; 7] =
    ["vless://", "vmess://", "anytls://", "hysteria2://", "hy2://", "trojan://", "ss://"];

/// `import_v2box`, given the rows the caller read out of `ZCDV2RAYITEM`.
pub fn v2box_rows(
    rows: &[(PyVal, PyVal, PyVal)],
    skipped: &mut Counter,
) -> R<(Vec<String>, Vec<Value>)> {
    let mut links: Vec<String> = Vec::new();
    let mut subs: Vec<Value> = Vec::new();
    for (ztype, zurl, zsub) in rows {
        let url = or_empty_strip(zurl)?;
        if startswith_str(&url, &V2BOX_LINKS)? {
            let Stripped::Str(u) = &url else { unreachable!() };
            if startswith_str(&url, &["trojan://", "ss://"])? {
                skipped.incr(u.split_once("://").map(|(p, _)| p).unwrap_or(u));
            } else {
                links.push(u.clone());
            }
        } else {
            let t = if ztype.truthy() { ztype.to_py_str() } else { "unknown".into() };
            skipped.incr(&t.to_lowercase());
        }
        let sub = or_empty_strip(zsub)?;
        if startswith_str(&sub, &["http://", "https://"])? {
            let Stripped::Str(s) = &sub else { unreachable!() };
            let mut e = Map::new();
            e.insert("url".into(), Value::String(s.clone()));
            e.insert("name".into(), Value::Null);
            subs.push(Value::Object(e));
        }
    }
    Ok((links, subs))
}

// ---------------------------------------------------------------------------
// What is already known, and the review
// ---------------------------------------------------------------------------

/// `norm_sub` — a deliberate copy of `importmerge::norm_sub`; see the module
/// doc. Identical in every reachable case, and kept apart because the Pythons
/// are not the same text.
pub fn norm_sub(url: &str) -> String {
    let s = strip(url);
    let Ok(u) = pyurl::urlsplit(s) else { return s.to_string() };
    let q: Vec<(String, String)> = pyurl::parse_qsl(&u.query, true)
        .into_iter()
        .filter(|(k, _)| k.to_lowercase() != "name")
        .collect();
    pyurl::urlunsplit(
        &u.scheme.to_lowercase(),
        &u.netloc.to_lowercase(),
        u.path.trim_end_matches('/'),
        &pyurl::urlencode(&q),
        "",
    )
}

/// `norm_sub(s["url"])` where the url came out of a YAML file and need not be a
/// string. `.strip()` on anything else ends the run.
fn norm_sub_value(url: &Value) -> R<String> {
    match url {
        Value::String(s) => Ok(norm_sub(s)),
        other => Err(Exc::attr(type_name(other), "strip")),
    }
}

/// `load_existing_keys`, given each `--existing` file already parsed (a file
/// that is missing, unreadable or malformed is `None` — the Python's
/// `except Exception: continue`).
pub fn existing_keys(pools: &[Option<Value>]) -> Vec<(String, String)> {
    let mut known: Vec<(String, String)> = Vec::new();
    for pool in pools {
        let Some(Value::Array(arr)) = pool else { continue };
        for o in arr {
            if !o.is_object() {
                continue;
            }
            let k = key_of(o);
            if known.iter().any(|(n, _)| *n == k) {
                continue; // setdefault keeps the FIRST tag seen
            }
            let tag = o.get("tag");
            let tag = if truthy(tag) { py_str(tag.unwrap()) } else { "?".into() };
            known.push((k, tag));
        }
    }
    known
}

/// `load_existing_subs`, given the raw text of each `--existing-subs` file.
pub fn existing_subs(texts: &[String]) -> HashSet<String> {
    let mut known = HashSet::new();
    for text in texts {
        for line in sharelink::splitlines(text) {
            let line = strip(&line);
            if !line.is_empty() && !line.starts_with('#') {
                known.insert(norm_sub(line));
            }
        }
    }
    known
}

/// The review file plus the lines the Python wrote to stderr, in order.
pub struct Review {
    pub value: Value,
    pub notes: Vec<String>,
}

/// `build_review` — parse the links, drop what is already in the pool or
/// already in this same extract, and drop subscriptions already saved.
pub fn build_review(
    links: &[String],
    subs: &[Value],
    skipped: &mut Counter,
    existing: &[(String, String)],
    existing_subs: &HashSet<String>,
) -> R<Review> {
    let mut notes: Vec<String> = Vec::new();
    let parsed: Vec<Value> = if links.is_empty() {
        Vec::new()
    } else {
        match sharelink::parse_many(links) {
            Ok(b) => {
                notes.extend(b.warnings);
                b.outbounds
            }
            // "no usable links" is caught and noted; the per-link warnings were
            // already printed on the way there.
            Err((b, msg)) => {
                notes.extend(b.warnings);
                notes.push(format!("note: {msg}"));
                Vec::new()
            }
        }
    };

    let mut seen_keys: Vec<(String, String)> = Vec::new();
    let mut servers: Vec<Value> = Vec::new();
    for o in &parsed {
        let k = key_of(o);
        let look = |v: &[(String, String)]| {
            v.iter().find(|(n, _)| *n == k).map(|(_, t)| t.clone()).filter(|t| !t.is_empty())
        };
        let prior = look(existing).or_else(|| look(&seen_keys));
        if let Some(prior) = prior {
            notes.push(format!(
                "note: skipping '{}' — already in your pool as '{prior}' ({}:{})",
                o.get("tag").map(py_str).unwrap_or_else(|| "None".into()),
                o.get("server").map(py_str).unwrap_or_else(|| "None".into()),
                o.get("server_port").map(py_str).unwrap_or_else(|| "None".into()),
            ));
            skipped.incr("duplicate");
            continue;
        }
        let tag = o.get("tag");
        seen_keys.push((k, if truthy(tag) { py_str(tag.unwrap()) } else { "?".into() }));
        servers.push(o.clone());
    }

    let mut seen_subs: HashSet<String> = HashSet::new();
    let mut uniq_subs: Vec<Value> = Vec::new();
    for s in subs {
        let raw = s.get("url").cloned().unwrap_or(Value::Null);
        let n = norm_sub_value(&raw)?;
        if existing_subs.contains(&n) {
            notes.push(format!("note: subscription already saved — skipping {}", py_str(&raw)));
            skipped.incr("duplicate-subscription");
            continue;
        }
        if !seen_subs.insert(n) {
            continue;
        }
        uniq_subs.push(s.clone());
    }

    let mut out = Map::new();
    out.insert("servers".into(), Value::Array(servers));
    out.insert("subscriptions".into(), Value::Array(uniq_subs));
    out.insert("proxy_domains".into(), Value::Array(vec![]));
    out.insert("skipped".into(), skipped.to_value());
    Ok(Review { value: Value::Object(out), notes })
}

/// `json.dump(review, sys.stdout, indent=2)` — ensure_ascii=True, unlike the
/// file `import-merge.py` writes from it.
pub fn render(v: &Value) -> String {
    format!("{}\n", pyjson::dumps(v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(v: Value) -> Map<String, Value> {
        match v {
            Value::Object(m) => m,
            _ => unreachable!(),
        }
    }

    fn link(v: Value) -> String {
        clash_proxy_to_link(&obj(v)).unwrap().unwrap()
    }

    // ---- carried over from config/test_foreign.py -------------------------

    #[test]
    fn vless_reality_roundtrips() {
        let l = link(json!({
            "name": "Home", "type": "vless", "server": "1.2.3.4", "port": 8443,
            "uuid": "01779e49-274c-4237-a4a6-f16f91b7850e", "network": "tcp",
            "tls": true, "flow": "xtls-rprx-vision", "servername": "www.microsoft.com",
            "client-fingerprint": "chrome",
            "reality-opts": {"public-key": "PBK123", "short-id": "ab"}
        }));
        assert!(l.starts_with("vless://"), "{l}");
        let o = sharelink::parse_link(&l, "Home").unwrap();
        assert_eq!(o["type"], json!("vless"));
        assert_eq!(o["server"], json!("1.2.3.4"));
        assert_eq!(o["server_port"], json!(8443));
        assert_eq!(o["flow"], json!("xtls-rprx-vision"));
        assert_eq!(o["tls"]["server_name"], json!("www.microsoft.com"));
        assert_eq!(o["tls"]["reality"]["public_key"], json!("PBK123"));
        assert_eq!(o["tls"]["reality"]["short_id"], json!("ab"));
    }

    #[test]
    fn vmess_ws_roundtrips() {
        let l = link(json!({
            "name": "V", "type": "vmess", "server": "h.example", "port": 443,
            "uuid": "u-1", "alterId": 0, "cipher": "auto", "network": "ws",
            "tls": true, "servername": "h.example",
            "ws-opts": {"path": "/p", "headers": {"Host": "h.example"}}
        }));
        let o = sharelink::parse_link(&l, "V").unwrap();
        assert_eq!(o["type"], json!("vmess"));
        assert_eq!(o["transport"]["type"], json!("ws"));
        assert_eq!(o["transport"]["path"], json!("/p"));
        assert_eq!(o["tls"]["server_name"], json!("h.example"));
    }

    #[test]
    fn unsupported_types_are_none() {
        for t in ["trojan", "ss", "tuic", "wireguard"] {
            let p = obj(json!({"type": t, "name": "x", "server": "h", "port": 1}));
            assert_eq!(clash_proxy_to_link(&p).unwrap(), None, "{t}");
        }
    }

    #[test]
    fn skipped_counts_unsupported() {
        let mut c = Counter::new();
        let proxies = json!([
            {"type": "vless", "name": "a", "server": "h", "port": 1, "uuid": "u"},
            {"type": "trojan", "name": "b", "server": "h", "port": 2, "password": "p"},
            {"type": "ss", "name": "c", "server": "h", "port": 3}
        ]);
        let links = links_from_clash_proxies(Some(&proxies), &mut c).unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(c.to_value(), json!({"trojan": 1, "ss": 1}));
    }

    #[test]
    fn build_review_skips_a_server_already_in_the_pool() {
        let l = link(json!({"type": "vless", "name": "Xuhui", "server": "1.2.3.4",
                            "port": 8443, "uuid": "u"}));
        let parsed = sharelink::parse_link(&l, "Xuhui").unwrap();
        let existing = vec![(key_of(&parsed), "Ds415".to_string())];
        let mut c = Counter::new();
        let r = build_review(&[l], &[], &mut c, &existing, &HashSet::new()).unwrap();
        assert_eq!(r.value["servers"], json!([]));
        assert_eq!(c.get("duplicate"), 1);
    }

    #[test]
    fn build_review_keeps_a_new_server() {
        let l = link(json!({"type": "vless", "name": "New", "server": "9.9.9.9",
                            "port": 443, "uuid": "n"}));
        let mut c = Counter::new();
        let r = build_review(&[l], &[], &mut c, &[], &HashSet::new()).unwrap();
        let tags: Vec<&str> =
            r.value["servers"].as_array().unwrap().iter().map(|s| s["tag"].as_str().unwrap()).collect();
        assert_eq!(tags, ["New"]);
    }

    #[test]
    fn norm_sub_ignores_the_name_param_and_a_trailing_slash() {
        assert_eq!(
            norm_sub("https://Sub.Example.com/x/?name=ByWave&token=abc"),
            norm_sub("https://sub.example.com/x?token=abc")
        );
    }

    #[test]
    fn build_review_skips_a_subscription_already_saved() {
        let subs = vec![json!({"url": "https://sub.example.com/x?token=abc&name=ByWave",
                               "name": "ByWave"})];
        let known: HashSet<String> =
            [norm_sub("https://sub.example.com/x?token=abc")].into_iter().collect();
        let mut c = Counter::new();
        let r = build_review(&[], &subs, &mut c, &[], &known).unwrap();
        assert_eq!(r.value["subscriptions"], json!([]));
        assert_eq!(c.get("duplicate-subscription"), 1);
    }

    #[test]
    fn build_review_dedupes_subscriptions_within_one_source() {
        let subs = vec![
            json!({"url": "https://s/x?name=A", "name": "A"}),
            json!({"url": "https://s/x?name=B", "name": "B"}),
        ];
        let mut c = Counter::new();
        let r = build_review(&[], &subs, &mut c, &[], &HashSet::new()).unwrap();
        assert_eq!(r.value["subscriptions"].as_array().unwrap().len(), 1);
    }

    // ---- what the Python tests do not reach -------------------------------

    #[test]
    fn a_non_mapping_reality_opts_is_an_attribute_error() {
        let p = obj(json!({"type": "vless", "server": "h", "port": 1, "reality-opts": "x"}));
        let e = clash_proxy_to_link(&p).unwrap_err();
        assert_eq!(e.name, "AttributeError");
        assert!(e.msg.contains("'str' object has no attribute 'get'"), "{}", e.msg);
    }

    #[test]
    fn the_fragment_uses_quote_and_the_query_uses_quote_plus() {
        // Same space, two encodings, four characters apart in the same link.
        let l = link(json!({"type": "anytls", "name": "a b", "server": "h", "port": 1,
                            "password": "p", "sni": "x y"}));
        assert!(l.ends_with("#a%20b"), "{l}");
        assert!(l.contains("sni=x+y"), "{l}");
    }

    #[test]
    fn a_password_keeps_its_slashes_but_loses_its_pluses() {
        let l = link(json!({"type": "anytls", "server": "h", "port": 1, "password": "a/b c+d"}));
        assert!(l.starts_with("anytls://a/b%20c%2Bd@h:1?"), "{l}");
    }

    #[test]
    fn hysteria2_prefers_sni_and_vless_prefers_servername() {
        let h = link(json!({"type": "hysteria2", "server": "h", "port": 1, "password": "p",
                            "sni": "S", "servername": "N"}));
        assert!(h.contains("sni=S"), "{h}");
        let v = link(json!({"type": "vless", "server": "h", "port": 1, "uuid": "u",
                            "sni": "S", "servername": "N"}));
        assert!(v.contains("sni=N"), "{v}");
    }

    #[test]
    fn a_falsy_query_value_is_dropped_but_a_false_is_not() {
        // `v not in (None, "")` — so `False` and `0` survive as text.
        let l = link(json!({"type": "vless", "server": "h", "port": 1, "uuid": "u",
                            "flow": false, "sni": "", "client-fingerprint": null}));
        assert!(l.contains("flow=False"), "{l}");
        assert!(!l.contains("sni="), "{l}");
        assert!(!l.contains("fp="), "{l}");
    }

    #[test]
    fn a_missing_port_renders_as_empty_not_none() {
        let l = link(json!({"type": "anytls", "server": "h", "password": "p"}));
        assert!(l.starts_with("anytls://p@h:?"), "{l}");
        // …but an explicit null does say None, because the default never fires.
        let l = link(json!({"type": "anytls", "server": "h", "port": null, "password": "p"}));
        assert!(l.starts_with("anytls://p@h:None?"), "{l}");
    }

    #[test]
    fn vmess_keeps_a_numeric_uuid_as_a_number() {
        // `"id": p.get("uuid","")` is the one field the Python does NOT str().
        let l = link(json!({"type": "vmess", "server": "h", "port": 1, "uuid": 7, "name": "n"}));
        let o = sharelink::parse_link(&l, "n").unwrap();
        assert_eq!(o["uuid"], json!("7"));
    }

    #[test]
    fn vmess_payload_uses_the_default_separators() {
        let l = link(json!({"type": "vmess", "server": "h", "port": 1, "uuid": "u"}));
        // `json.dumps` without `separators=` writes `", "` and `": "`. A compact
        // payload parses back to the same outbound and is a different link.
        let want = r#"{"v": "2", "ps": "", "add": "h", "port": "1", "id": "u", "aid": "0", "scy": "auto", "net": "tcp"}"#;
        assert_eq!(l, format!("vmess://{}", b64encode(want.as_bytes())));
    }

    #[test]
    fn python_float_str_switches_to_exponent_where_python_does() {
        assert_eq!(py_float_str(1.0), "1.0");
        assert_eq!(py_float_str(1e15), "1000000000000000.0");
        assert_eq!(py_float_str(1e16), "1e+16");
        assert_eq!(py_float_str(1e-4), "0.0001");
        assert_eq!(py_float_str(1e-5), "1e-05");
        assert_eq!(py_float_str(-2.5e-7), "-2.5e-07");
        assert_eq!(py_float_str(1.5), "1.5");
        assert_eq!(py_float_str(-0.0), "-0.0");
        assert_eq!(py_float_str(f64::INFINITY), "inf");
    }

    #[test]
    fn python_bytes_repr_picks_its_quote_and_escapes_the_rest() {
        assert_eq!(py_bytes_repr(b"ab"), "b'ab'");
        assert_eq!(py_bytes_repr(b"a'b"), "b\"a'b\"");
        assert_eq!(py_bytes_repr(b"a'b\"c"), "b'a\\'b\"c'");
        assert_eq!(py_bytes_repr(b"\x00\n\x7f"), "b'\\x00\\n\\x7f'");
    }

    #[test]
    fn a_blob_url_is_a_type_error_not_a_skip() {
        let rows = vec![(PyVal::Str("x".into()), PyVal::Bytes(b"vless://a".to_vec()), PyVal::None)];
        let e = v2box_rows(&rows, &mut Counter::new()).unwrap_err();
        assert_eq!(e.name, "TypeError");
    }

    #[test]
    fn a_nonzero_integer_url_has_no_strip() {
        let rows = vec![(PyVal::None, PyVal::Int(5), PyVal::None)];
        let e = v2box_rows(&rows, &mut Counter::new()).unwrap_err();
        assert_eq!(e.name, "AttributeError");
        // …while a zero one is falsy and becomes the empty string.
        let rows = vec![(PyVal::Int(0), PyVal::Int(0), PyVal::None)];
        let mut c = Counter::new();
        v2box_rows(&rows, &mut c).unwrap();
        assert_eq!(c.get("unknown"), 1);
    }

    #[test]
    fn v2box_counts_trojan_and_ss_by_scheme_and_others_by_ztype() {
        let rows = vec![
            (PyVal::Str("a".into()), PyVal::Str("trojan://x".into()), PyVal::None),
            (PyVal::Str("a".into()), PyVal::Str("ss://x".into()), PyVal::None),
            (PyVal::Int(7), PyVal::Str("nonsense".into()), PyVal::None),
            (PyVal::Bytes(b"B".to_vec()), PyVal::Str("nope".into()), PyVal::None),
        ];
        let mut c = Counter::new();
        let (links, _) = v2box_rows(&rows, &mut c).unwrap();
        assert!(links.is_empty());
        assert_eq!(c.to_value(), json!({"trojan": 1, "ss": 1, "7": 1, "b'b'": 1}));
    }

    #[test]
    fn a_subscription_column_is_taken_even_when_the_url_column_was_junk() {
        let rows =
            vec![(PyVal::None, PyVal::Str("junk".into()), PyVal::Str(" https://s/x ".into()))];
        let mut c = Counter::new();
        let (_, subs) = v2box_rows(&rows, &mut c).unwrap();
        assert_eq!(subs, vec![json!({"url": "https://s/x", "name": null})]);
    }

    // ---- the profile directory --------------------------------------------

    struct Table {
        files: Vec<String>,
        dirs: Vec<String>,
        docs: Vec<(String, R<Value>)>,
    }

    impl Fs for Table {
        fn exists(&self, p: &str) -> bool {
            self.files.iter().any(|f| f == p) || self.dirs.iter().any(|d| d == p)
        }
        fn is_dir(&self, p: &str) -> bool {
            self.dirs.iter().any(|d| d == p)
        }
        fn rglob_yaml(&self, root: &str) -> Vec<String> {
            let mut v: Vec<String> = self
                .files
                .iter()
                .filter(|f| f.starts_with(&format!("{root}/")) && pypath::matches_yaml(&pypath::name(f)))
                .cloned()
                .collect();
            pypath::sort(&mut v);
            v
        }
        fn yq_json(&self, p: &str) -> R<Value> {
            match self.docs.iter().find(|(n, _)| n == p) {
                Some((_, Ok(v))) => Ok(v.clone()),
                Some((_, Err(e))) => Err(e.clone()),
                None => Err(Exc::new("RuntimeError", format!("yq failed on {p}: no such file"))),
            }
        }
    }

    #[test]
    fn the_index_branch_reads_remotes_as_subs_and_locals_as_proxies() {
        let fs = Table {
            files: vec!["r/profiles.yaml".into(), "r/profiles/a.yaml".into()],
            dirs: vec!["r".into(), "r/profiles".into()],
            docs: vec![
                (
                    "r/profiles.yaml".into(),
                    Ok(json!({"items": [
                        {"type": "remote", "url": "https://s/x", "uid": "u1"},
                        {"type": "local", "file": "a.yaml"}
                    ]})),
                ),
                (
                    "r/profiles/a.yaml".into(),
                    Ok(json!({"proxies": [
                        {"type": "vless", "name": "A", "server": "h", "port": 1, "uuid": "u"}
                    ]})),
                ),
            ],
        };
        let mut c = Counter::new();
        let (links, subs) = import_clash_dir(&fs, "r", &mut c).unwrap();
        assert_eq!(links.len(), 1);
        // `name or uid` — the uid stands in when there is no name.
        assert_eq!(subs, vec![json!({"url": "https://s/x", "name": "u1"})]);
    }

    #[test]
    fn a_yq_failure_is_fatal_with_an_index_and_swallowed_without_one() {
        let boom = || Err(Exc::new("RuntimeError", "`yq` is required"));
        let indexed = Table {
            files: vec!["r/profiles.yaml".into()],
            dirs: vec!["r".into(), "r/profiles".into()],
            docs: vec![("r/profiles.yaml".into(), boom())],
        };
        assert!(import_clash_dir(&indexed, "r", &mut Counter::new()).is_err());

        let scanned = Table {
            files: vec!["r/a.yaml".into()],
            dirs: vec!["r".into()],
            docs: vec![("r/a.yaml".into(), boom())],
        };
        let (links, subs) = import_clash_dir(&scanned, "r", &mut Counter::new()).unwrap();
        assert!(links.is_empty() && subs.is_empty());
    }

    #[test]
    fn a_list_at_the_top_of_the_index_has_no_get() {
        let fs = Table {
            files: vec!["r/profiles.yaml".into()],
            dirs: vec!["r".into(), "r/profiles".into()],
            docs: vec![("r/profiles.yaml".into(), Ok(json!(["a"])))],
        };
        let e = import_clash_dir(&fs, "r", &mut Counter::new()).unwrap_err();
        assert_eq!(e.name, "AttributeError");
    }

    #[test]
    fn a_non_string_file_entry_cannot_be_joined_to_a_path() {
        let fs = Table {
            files: vec!["r/profiles.yaml".into()],
            dirs: vec!["r".into(), "r/profiles".into()],
            docs: vec![(
                "r/profiles.yaml".into(),
                Ok(json!({"items": [{"type": "local", "file": 5}]})),
            )],
        };
        let e = import_clash_dir(&fs, "r", &mut Counter::new()).unwrap_err();
        assert_eq!(e.name, "TypeError");
    }

    #[test]
    fn the_scan_branch_walks_in_path_component_order() {
        let fs = Table {
            files: vec!["r/a-x/c.yaml".into(), "r/a/b.yaml".into()],
            dirs: vec!["r".into()],
            docs: vec![
                (
                    "r/a/b.yaml".into(),
                    Ok(json!({"proxies": [{"type": "vless", "name": "first",
                                           "server": "h", "port": 1, "uuid": "u"}]})),
                ),
                (
                    "r/a-x/c.yaml".into(),
                    Ok(json!({"proxies": [{"type": "vless", "name": "second",
                                           "server": "h", "port": 2, "uuid": "u"}]})),
                ),
            ],
        };
        let (links, _) = import_clash_dir(&fs, "r", &mut Counter::new()).unwrap();
        assert!(links[0].ends_with("#first"), "{:?}", links);
        assert!(links[1].ends_with("#second"), "{:?}", links);
    }

    #[test]
    fn an_absolute_file_entry_escapes_the_profiles_dir() {
        assert_eq!(join("r/profiles", "/etc/x"), "/etc/x");
        assert_eq!(join("r/profiles", "a.yaml"), "r/profiles/a.yaml");
    }
}
