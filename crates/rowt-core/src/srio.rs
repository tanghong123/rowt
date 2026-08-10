//! The discovery half of `config/sr-import.py` — where Shadowrocket keeps its
//! server store and its rule file, and how to read them off a real machine.
//!
//! Split out of `bin/rowt-sr-import.rs` for the same reason as
//! [`foreignio`](crate::foreignio): the binary exists to be argv-for-argv what
//! the Python is so `parity sr-diff` can compare them, and `rowt server import`
//! needs the same discovery without running either.
//!
//! Discovery is the part that is easy to get subtly wrong, because it is two
//! different globbers. The store is a plain "first of these paths that is a
//! file". The rule file is `glob.glob(..., recursive=True)`, which skips hidden
//! entries (unlike `pathlib`'s, which `foreign-import.py` uses two files over),
//! tries three patterns in order and stops at the first that matches anything,
//! then takes the NEWEST hit by mtime.

use crate::bplist::{self, PlErr};
use crate::foreign::Exc;
use crate::pypath;
use crate::srimport;
use serde_json::Value;

/// How reading a Shadowrocket install can fail.
///
/// Two shapes because the Python has two: an uncaught exception, which the
/// binary reproduces as a `Traceback` and exit 1, and the one case where this
/// reader is deliberately narrower than `plistlib` — an XML plist, refused with
/// a plain message. Keeping them apart is what lets the binary stay
/// argv-for-argv while `rowt server import` collapses both into "could not read
/// Shadowrocket data".
pub enum SrErr {
    Exc(Exc),
    Xml(String),
}

impl From<Exc> for SrErr {
    fn from(e: Exc) -> Self {
        SrErr::Exc(e)
    }
}

/// Python's `or` on a string argument: `--store ""` is falsy and falls through
/// to auto-detection rather than naming a file called "".
pub fn given(o: Option<&str>) -> Option<String> {
    o.map(|s| s.to_string()).filter(|s| !s.is_empty())
}

/// `args.store or _find(_store_candidates())`.
pub fn resolve_store(arg: Option<&str>) -> Option<String> {
    given(arg).or_else(|| find(&store_candidates()))
}

/// `--detect` — is there anything here to import?
///
/// The store check is one stat; `_find_conf` is a recursive glob over the whole
/// of iCloud Drive. Only pay for it when there is no store, which is why the
/// resolved store comes in rather than being looked up again.
pub fn detect(store: &Option<String>, conf_arg: Option<&str>) -> bool {
    store.is_some() || given(conf_arg).is_some() || find_conf().is_some()
}

/// Read the install: the server store, then the rule file, then assemble.
///
/// The order is load-bearing and not decorative — the Python scans the store
/// and prints its warning BEFORE it opens the rule file, so a machine missing
/// both produces the warnings in that order and then whatever the second one
/// raises.
pub fn extract(store: Option<&str>, conf_arg: Option<&str>) -> Result<Value, SrErr> {
    let root = match store.filter(|s| is_file(s)) {
        Some(path) => match std::fs::read(path) {
            Ok(bytes) => match bplist::load(&bytes) {
                Ok(v) => Some(v),
                Err(PlErr::Invalid) => {
                    return Err(Exc::new("plistlib.InvalidFileException", "Invalid file").into())
                }
                // Named rather than guessed at: `plistlib` would parse this and
                // this reader will not. Unreachable through rowt, which never
                // passes --store, and Shadowrocket writes binary.
                Err(PlErr::NotBinary) => return Err(SrErr::Xml(pypath::path_str(path))),
            },
            Err(e) => return Err(Exc::new("OSError", e.to_string()).into()),
        },
        None => None,
    };

    let scan = match &root {
        Some(r) => Some(srimport::scan_store(r).map_err(SrErr::Exc)?),
        None => {
            eprintln!("warning: no Shadowrocket server store found");
            None
        }
    };

    let conf = given(conf_arg).or_else(find_conf);
    // `open(conf, encoding="utf-8", errors="replace")` — undecodable bytes
    // become U+FFFD rather than ending the run.
    let domains = match conf.as_deref().filter(|c| is_file(c)) {
        Some(path) => match std::fs::read(path) {
            Ok(bytes) => srimport::parse_rules(&String::from_utf8_lossy(&bytes)),
            Err(e) => return Err(Exc::new("OSError", e.to_string()).into()),
        },
        None => {
            eprintln!("warning: no Shadowrocket .conf rule file found");
            Vec::new()
        }
    };

    Ok(srimport::assemble(scan, domains))
}

// ---------------------------------------------------------------------------
// Where Shadowrocket keeps things
// ---------------------------------------------------------------------------

fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

pub fn gc() -> String {
    format!("{}/Library/Group Containers/group.com.liguangming.Shadowrocket", home())
}

pub fn icloud() -> String {
    format!(
        "{}/Library/Mobile Documents/iCloud~com~liguangming~Shadowrocket/Documents",
        home()
    )
}

pub fn store_candidates() -> Vec<String> {
    vec![format!("{}/ServerManager", gc()), format!("{}/shadowrocket.v2.model", icloud())]
}

pub fn is_file(p: &str) -> bool {
    std::path::Path::new(p).is_file()
}

/// `_find` — the first candidate that is a regular file.
pub fn find(paths: &[String]) -> Option<String> {
    paths.iter().find(|p| is_file(p)).cloned()
}

/// `_find_conf` — three patterns in order; the first with any hits wins, and
/// within it the newest file by mtime.
pub fn find_conf() -> Option<String> {
    let patterns: [(String, &str, bool); 3] = [
        (
            format!("{}/Library/Mobile Documents/com~apple~CloudDocs", home()),
            "default.conf",
            true,
        ),
        (icloud(), "*.conf", true),
        (gc(), "*.conf", false),
    ];
    for (base, tail, recursive) in patterns {
        let mut hits: Vec<String> = Vec::new();
        if recursive {
            let mut dirs = vec![base.clone()];
            walk_dirs(&base, &mut dirs);
            for d in dirs {
                collect(&d, tail, &mut hits);
            }
        } else {
            collect(&base, tail, &mut hits);
        }
        hits.retain(|h| is_file(h));
        if hits.is_empty() {
            continue;
        }
        // `max(hits, key=os.path.getmtime)` keeps the FIRST of equal maxima, in
        // glob's directory order. Equal mtimes therefore make the answer
        // filesystem-dependent on both sides alike; the corpus gives every
        // candidate a distinct one.
        let mut best = hits[0].clone();
        let mut best_m = mtime(&best);
        for h in &hits[1..] {
            let m = mtime(h);
            if m > best_m {
                best_m = m;
                best = h.clone();
            }
        }
        return Some(best);
    }
    None
}

pub fn mtime(p: &str) -> std::time::SystemTime {
    std::fs::metadata(p).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH)
}

/// `**` with `recursive=True`: every directory under `base`, `base` included,
/// skipping hidden names — `glob` does not match a leading dot.
pub fn walk_dirs(dir: &str, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut kids: Vec<String> = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            kids.push(format!("{dir}/{name}"));
        }
    }
    kids.sort();
    for k in kids {
        out.push(k.clone());
        walk_dirs(&k, out);
    }
}

/// The last component of the pattern: either a literal name or `*.conf`.
pub fn collect(dir: &str, tail: &str, out: &mut Vec<String>) {
    if let Some(ext) = tail.strip_prefix('*') {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut names: Vec<String> = Vec::new();
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.starts_with('.') && name.ends_with(ext) {
                names.push(name);
            }
        }
        names.sort();
        out.extend(names.into_iter().map(|n| format!("{dir}/{n}")));
    } else {
        let p = format!("{dir}/{tail}");
        if std::path::Path::new(&p).exists() {
            out.push(p);
        }
    }
}

