//! `rowt-sr-import` — argv-for-argv what `config/sr-import.py` is, so the two
//! can be run against each other by `parity sr-diff`.
//!
//! The semantics live in `rowt_core::srimport` and the container format in
//! `rowt_core::bplist`; this is the discovery — which store, which `.conf` —
//! and the argparse surface.
//!
//! Discovery is the part that is easy to get subtly wrong, because it is two
//! different globbers. The store is a plain "first of these paths that is a
//! file". The rule file is `glob.glob(..., recursive=True)`, which skips hidden
//! entries (unlike `pathlib`'s, which `foreign-import.py` uses two files over),
//! tries three patterns in order and stops at the first that matches anything,
//! then takes the NEWEST hit by mtime.
//!
//! `--detect` deliberately does not run the expensive one unless it has to:
//! the iCloud pattern walks `com~apple~CloudDocs/**`, which can stall on
//! dataless files, and `bin/rowt` gives detection a six-second budget.

use rowt_core::bplist::{self, PlErr};
use rowt_core::foreign::Exc;
use rowt_core::pypath;
use rowt_core::srimport;

const PROG: &str = "sr-import.py";

const USAGE: &str = "usage: sr-import.py [-h] [--store STORE] [--conf CONF] [--detect]";

const HELP: &str = "\nShadowrocket -> rowt importer\n\noptions:\n  -h, --help     show this help message and exit\n  --store STORE  ServerManager / v2.model plist (auto-detected)\n  --conf CONF    default.conf rule file (auto-detected)\n  --detect       print 'shadowrocket' if importable data is present, else\n                 nothing";

const OPTIONS: [&str; 5] = ["-h", "--help", "--store", "--conf", "--detect"];

fn ap_error(msg: &str) -> ! {
    eprintln!("{USAGE}");
    eprintln!("{PROG}: error: {msg}");
    std::process::exit(2)
}

fn die(e: &Exc) -> ! {
    eprintln!("Traceback (most recent call last):");
    eprintln!("{}: {}", e.name, e.msg);
    std::process::exit(1)
}

// ---------------------------------------------------------------------------

fn resolve(flag: &str) -> String {
    if OPTIONS.contains(&flag) || !flag.starts_with("--") {
        return flag.to_string();
    }
    let hits: Vec<&&str> = OPTIONS.iter().filter(|o| o.starts_with(flag)).collect();
    match hits.len() {
        1 => hits[0].to_string(),
        0 => flag.to_string(),
        _ => ap_error(&format!(
            "ambiguous option: {flag} could match {}",
            hits.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(", ")
        )),
    }
}

fn looks_like_option(s: &str) -> bool {
    if !s.starts_with('-') || s == "-" || s.contains(' ') {
        return false;
    }
    let n = &s[1..];
    let negative = !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())
        || n.split_once('.').is_some_and(|(a, b)| {
            a.chars().all(|c| c.is_ascii_digit())
                && !b.is_empty()
                && b.chars().all(|c| c.is_ascii_digit())
        });
    !negative
}

#[derive(Default)]
struct Args {
    store: Option<String>,
    conf: Option<String>,
    detect: bool,
}

fn parse_args() -> Args {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut a = Args::default();
    let mut i = 0;
    while i < argv.len() {
        let raw = argv[i].clone();
        let (flag, inline) = match raw.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (resolve(f), Some(v.to_string())),
            _ => (resolve(&raw), None),
        };
        let value = |i: &mut usize| -> String {
            if let Some(v) = inline.clone() {
                return v;
            }
            *i += 1;
            match argv.get(*i) {
                Some(v) if !looks_like_option(v) => v.clone(),
                _ => ap_error(&format!("argument {flag}: expected one argument")),
            }
        };
        match flag.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                println!("{HELP}");
                std::process::exit(0);
            }
            "--detect" => a.detect = true,
            "--store" => a.store = Some(value(&mut i)),
            "--conf" => a.conf = Some(value(&mut i)),
            _ => ap_error(&format!("unrecognized arguments: {raw}")),
        }
        i += 1;
    }
    a
}

// ---------------------------------------------------------------------------
// Where Shadowrocket keeps things
// ---------------------------------------------------------------------------

fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

fn gc() -> String {
    format!("{}/Library/Group Containers/group.com.liguangming.Shadowrocket", home())
}

fn icloud() -> String {
    format!(
        "{}/Library/Mobile Documents/iCloud~com~liguangming~Shadowrocket/Documents",
        home()
    )
}

fn store_candidates() -> Vec<String> {
    vec![format!("{}/ServerManager", gc()), format!("{}/shadowrocket.v2.model", icloud())]
}

fn is_file(p: &str) -> bool {
    std::path::Path::new(p).is_file()
}

/// `_find` — the first candidate that is a regular file.
fn find(paths: &[String]) -> Option<String> {
    paths.iter().find(|p| is_file(p)).cloned()
}

/// `_find_conf` — three patterns in order; the first with any hits wins, and
/// within it the newest file by mtime.
fn find_conf() -> Option<String> {
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

fn mtime(p: &str) -> std::time::SystemTime {
    std::fs::metadata(p).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH)
}

/// `**` with `recursive=True`: every directory under `base`, `base` included,
/// skipping hidden names — `glob` does not match a leading dot.
fn walk_dirs(dir: &str, out: &mut Vec<String>) {
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
fn collect(dir: &str, tail: &str, out: &mut Vec<String>) {
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

// ---------------------------------------------------------------------------

fn main() {
    let args = parse_args();
    // `args.store or _find(...)` is Python's `or`, so `--store ""` falls
    // through to auto-detection rather than naming a file called "".
    let given = |o: &Option<String>| o.clone().filter(|s| !s.is_empty());
    let store = given(&args.store).or_else(|| find(&store_candidates()));

    if args.detect {
        // The store check is one stat; `_find_conf` is a recursive glob over
        // the whole of iCloud Drive. Only pay for it when there is no store.
        if store.is_some() || given(&args.conf).is_some() || find_conf().is_some() {
            println!("shadowrocket");
        }
        return;
    }

    let conf = given(&args.conf).or_else(find_conf);

    let root = match store.as_deref().filter(|s| is_file(s)) {
        Some(path) => match std::fs::read(path) {
            Ok(bytes) => match bplist::load(&bytes) {
                Ok(v) => Some(v),
                Err(PlErr::Invalid) => die(&Exc::new("plistlib.InvalidFileException", "Invalid file")),
                // Named rather than guessed at: `plistlib` would parse this and
                // this reader will not. Unreachable through rowt, which never
                // passes --store, and Shadowrocket writes binary.
                Err(PlErr::NotBinary) => {
                    eprintln!(
                        "error: {} is an XML plist; only the binary format is read",
                        pypath::path_str(path)
                    );
                    std::process::exit(1)
                }
            },
            Err(e) => die(&Exc::new("OSError", e.to_string())),
        },
        None => None,
    };

    // The store is scanned, and its warning printed, BEFORE the rule file is
    // opened — so a missing store and an unreadable .conf give a warning and
    // then a traceback, in that order.
    let scan = match &root {
        Some(r) => match srimport::scan_store(r) {
            Ok(s) => Some(s),
            Err(e) => die(&e),
        },
        None => {
            eprintln!("warning: no Shadowrocket server store found");
            None
        }
    };

    // `open(conf, encoding="utf-8", errors="replace")` — undecodable bytes
    // become U+FFFD rather than ending the run.
    let domains = match conf.as_deref().filter(|c| is_file(c)) {
        Some(path) => match std::fs::read(path) {
            Ok(bytes) => srimport::parse_rules(&String::from_utf8_lossy(&bytes)),
            Err(e) => die(&Exc::new("OSError", e.to_string())),
        },
        None => {
            eprintln!("warning: no Shadowrocket .conf rule file found");
            Vec::new()
        }
    };

    let x = srimport::assemble(scan, domains);
    print!("{}", srimport::render(&x));
}
