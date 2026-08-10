//! `rowt-foreign-import` — argv-for-argv what `config/foreign-import.py` is, so
//! the two can be run against each other by `parity foreign-diff`.
//!
//! The semantics live in `rowt_core::foreign`; this is the I/O the Python does
//! and the exact argparse surface, which is part of the contract — `bin/rowt`
//! reads the exit status and passes stderr straight through to the terminal.
//!
//! Uncaught exceptions are reproduced as exit 1 with a `Traceback` header and
//! the exception's qualified name, which is what the gate compares. The
//! traceback BODY is normalized away on both sides: frame lines and message
//! wording are interpreter-version detail, while "did this input crash, and
//! with what kind of error" is the thing that keeps garbage out of a pool.

use rowt_core::foreign::{
    self, build_review, existing_keys, existing_subs, import_clash_dir, v2box_rows, Counter, Exc,
    Fs, PyVal, R,
};
use rowt_core::sharelink::{py_json_loads, strip};
use rowt_core::{pypath, pyurl};
use serde_json::Value;

const PROG: &str = "foreign-import.py";

const USAGE: &str = "usage: foreign-import.py [-h] [--from {clash-verge,v2box,flclash}] [--detect]\n                         [--path PATH] [--existing FILE]\n                         [--existing-subs FILE]";

const HELP: &str = "\nimport servers from another proxy client\n\noptions:\n  -h, --help            show this help message and exit\n  --from {clash-verge,v2box,flclash}\n  --detect              print which of clash-verge/v2box/flclash have\n                        importable data\n  --path PATH           override the source location (dir or DB file)\n  --existing FILE       an existing pool (servers.json / manual.json); its\n                        servers are skipped as duplicates\n  --existing-subs FILE  an existing subs list (subs.txt); URLs already saved\n                        are skipped";

const OPTIONS: [&str; 7] =
    ["-h", "--help", "--from", "--detect", "--path", "--existing", "--existing-subs"];

const SOURCES: [&str; 3] = ["clash-verge", "v2box", "flclash"];

fn ap_error(msg: &str) -> ! {
    eprintln!("{USAGE}");
    eprintln!("{PROG}: error: {msg}");
    std::process::exit(2)
}

/// An exception that nothing caught. Python's own last line is
/// `<qualified name>: <message>`; everything above it is frames.
fn die(e: &Exc) -> ! {
    eprintln!("Traceback (most recent call last):");
    eprintln!("{}: {}", e.name, e.msg);
    std::process::exit(1)
}

// ---------------------------------------------------------------------------
// argparse
// ---------------------------------------------------------------------------

/// argparse resolves an abbreviated long option as long as exactly one option
/// starts with it — so `--det` works and `--exist` is an error naming both
/// candidates.
fn resolve(flag: &str) -> String {
    if OPTIONS.contains(&flag) {
        return flag.to_string();
    }
    if !flag.starts_with("--") {
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

/// argparse will not hand an option-looking token to a flag that wants a value:
/// `--from --existing-subs x` is "expected one argument", not a source called
/// `--existing-subs`. A lone `-` and anything matching argparse's
/// `_negative_number_matcher` are values, since this parser declares no option
/// that looks like a negative number.
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
    source: Option<String>,
    detect: bool,
    path: Option<String>,
    existing: Vec<String>,
    existing_subs: Vec<String>,
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
            "--from" => {
                let v = value(&mut i);
                if !SOURCES.contains(&v.as_str()) {
                    ap_error(&format!(
                        "argument --from: invalid choice: {} (choose from {})",
                        pyurl::repr(&v),
                        SOURCES.iter().map(|s| pyurl::repr(s)).collect::<Vec<_>>().join(", ")
                    ));
                }
                a.source = Some(v);
            }
            "--path" => a.path = Some(value(&mut i)),
            "--existing" => a.existing.push(value(&mut i)),
            "--existing-subs" => a.existing_subs.push(value(&mut i)),
            _ => ap_error(&format!("unrecognized arguments: {raw}")),
        }
        i += 1;
    }
    a
}

// ---------------------------------------------------------------------------
// The machine
// ---------------------------------------------------------------------------

fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

fn under(rel: &str) -> String {
    foreign::join(&home(), rel)
}

fn clash_verge_dirs() -> Vec<String> {
    [
        "Library/Application Support/io.github.clash-verge-rev.clash-verge-rev",
        "Library/Application Support/io.github.clash-verge-rev.clash-verge",
        "Library/Application Support/io.github.zzzgydi.clash-verge",
        ".local/share/clash-verge",
        ".config/clash-verge",
    ]
    .iter()
    .map(|p| under(p))
    .collect()
}

fn flclash_dirs() -> Vec<String> {
    [
        "Library/Application Support/FlClash",
        "Library/Application Support/com.follow.clash",
        ".config/FlClash",
        ".config/flclash",
    ]
    .iter()
    .map(|p| under(p))
    .collect()
}

fn v2box_dbs() -> Vec<String> {
    vec![under("Library/Group Containers/group.hossin.asaadi.V2Box/DB.sqlite")]
}

fn first_existing(paths: &[String]) -> Option<String> {
    paths.iter().find(|p| std::path::Path::new(p).exists()).cloned()
}

/// `shutil.which` — a PATH entry that is empty means the working directory, and
/// the hit has to be an executable file.
fn which(cmd: &str) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    for dir in std::env::var("PATH").unwrap_or_default().split(':') {
        let p = if dir.is_empty() { cmd.to_string() } else { format!("{dir}/{cmd}") };
        let path = std::path::Path::new(&p);
        if let Ok(md) = path.metadata() {
            if md.is_file() && md.permissions().mode() & 0o111 != 0 {
                return Some(p);
            }
        }
    }
    None
}

fn decode(bytes: Vec<u8>) -> R<String> {
    String::from_utf8(bytes).map_err(|_| {
        Exc::new("UnicodeDecodeError", "'utf-8' codec can't decode byte in yq's output")
    })
}

struct RealFs;

impl Fs for RealFs {
    fn exists(&self, path: &str) -> bool {
        std::path::Path::new(path).exists()
    }
    fn is_dir(&self, path: &str) -> bool {
        std::path::Path::new(path).is_dir()
    }
    fn rglob_yaml(&self, root: &str) -> Vec<String> {
        let mut out = Vec::new();
        walk(root, &mut out);
        pypath::sort(&mut out);
        out
    }
    fn yq_json(&self, path: &str) -> R<Value> {
        // PyYAML is not a dependency, so the Python shells out — and so does
        // this, which also keeps the "yq is missing" failure identical.
        if which("yq").is_none() {
            return Err(Exc::new(
                "RuntimeError",
                "`yq` is required to read Clash YAML — install it: brew install yq",
            ));
        }
        let out = match std::process::Command::new("yq").args(["-o=json", ".", path]).output() {
            Ok(o) => o,
            Err(e) => return Err(Exc::new("OSError", e.to_string())),
        };
        if !out.status.success() {
            let err = decode(out.stderr)?;
            return Err(Exc::new(
                "RuntimeError",
                format!("yq failed on {}: {}", pypath::name(path), strip(&err)),
            ));
        }
        let text = decode(out.stdout)?;
        // `json.loads(r.stdout or "null")` — an empty stdout is a null document.
        let text = if text.is_empty() { "null".to_string() } else { text };
        py_json_loads(&text).map_err(|m| Exc::new("json.decoder.JSONDecodeError", m))
    }
}

/// `Path.rglob("*.y*ml")` — hidden entries included (a `pathlib` glob is not a
/// `glob.glob`), directories whose own name matches included, symlinked
/// directories not descended into.
fn walk(dir: &str, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let path = foreign::join(dir, &name);
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if pypath::matches_yaml(&name) {
            out.push(path.clone());
        }
        if is_dir {
            walk(&path, out);
        }
    }
}

/// `sqlite3.connect(f"file:{db}?mode=ro", uri=True)` and one SELECT.
fn v2box_read(db: &str) -> R<Vec<(PyVal, PyVal, PyVal)>> {
    use rusqlite::{types::ValueRef, OpenFlags};
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI;
    let con = rusqlite::Connection::open_with_flags(format!("file:{db}?mode=ro"), flags)
        .map_err(sqlite_exc)?;
    let mut stmt = con.prepare("SELECT ZTYPE, ZURL, ZSUBSCRIBE FROM ZCDV2RAYITEM").map_err(sqlite_exc)?;
    let mut rows = stmt.query([]).map_err(sqlite_exc)?;
    let mut out = Vec::new();
    // `.fetchall()` — every row is read before anything is done with any of
    // them, so a bad row later in the table fails the whole import.
    while let Some(row) = rows.next().map_err(sqlite_exc)? {
        let cell = |i: usize| -> R<PyVal> {
            match row.get_ref(i).map_err(sqlite_exc)? {
                ValueRef::Null => Ok(PyVal::None),
                ValueRef::Integer(n) => Ok(PyVal::Int(n)),
                ValueRef::Real(f) => Ok(PyVal::Float(f)),
                ValueRef::Text(b) => match std::str::from_utf8(b) {
                    Ok(s) => Ok(PyVal::Str(s.to_string())),
                    Err(_) => Err(Exc::new(
                        "sqlite3.OperationalError",
                        "Could not decode to UTF-8 column",
                    )),
                },
                ValueRef::Blob(b) => Ok(PyVal::Bytes(b.to_vec())),
            }
        };
        out.push((cell(0)?, cell(1)?, cell(2)?));
    }
    Ok(out)
}

/// `_pysqlite_seterror`'s table — which SQLite result code becomes which
/// `sqlite3.*` exception. Only the class name survives normalization, and it is
/// the part that says whether a file was unreadable or simply not a database.
fn sqlite_exc(e: rusqlite::Error) -> Exc {
    let (code, msg) = match &e {
        // The PRIMARY result code is the low byte of the extended one; the
        // `ErrorCode` enum next to it is rusqlite's own numbering, not
        // SQLite's, and would map to the wrong exception class.
        rusqlite::Error::SqliteFailure(f, m) => {
            (f.extended_code & 0xff, m.clone().unwrap_or_else(|| e.to_string()))
        }
        other => (1, other.to_string()),
    };
    let name = match code {
        2 | 12 => "sqlite3.InternalError",
        7 => "MemoryError",
        1 | 3 | 4 | 5 | 6 | 8 | 9 | 10 | 13 | 14 | 15 | 16 | 17 => "sqlite3.OperationalError",
        18 => "sqlite3.DataError",
        19 | 20 => "sqlite3.IntegrityError",
        21 => "sqlite3.InterfaceError",
        _ => "sqlite3.DatabaseError",
    };
    Exc::new(name, msg)
}

/// Which clients have importable data here — the fast checks only, so
/// `bin/rowt`'s six-second time box is never the thing that decides.
fn detect() -> Vec<String> {
    let fs = RealFs;
    let mut found = Vec::new();
    if let Some(cv) = first_existing(&clash_verge_dirs()) {
        if fs.exists(&foreign::join(&cv, "profiles.yaml"))
            || fs.is_dir(&foreign::join(&cv, "profiles"))
        {
            found.push("clash-verge".to_string());
        }
    }
    if first_existing(&v2box_dbs()).is_some() {
        found.push("v2box".to_string());
    }
    if let Some(fc) = first_existing(&flclash_dirs()) {
        if !fs.rglob_yaml(&fc).is_empty() {
            found.push("flclash".to_string());
        }
    }
    found
}

// ---------------------------------------------------------------------------

fn read_json(path: &str) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn run() -> i32 {
    let args = parse_args();

    if args.detect {
        for name in detect() {
            println!("{name}");
        }
        return 0;
    }
    let Some(source) = args.source.clone() else {
        ap_error("--from is required (one of clash-verge/v2box/flclash), or use --detect")
    };

    let mut skipped = Counter::new();
    let fs = RealFs;
    // `None` is the "no data found" return: the message is already printed and
    // the Python leaves `main` with 1 before any review exists.
    let result: R<Option<(Vec<String>, Vec<Value>)>> = (|| {
        if source == "clash-verge" || source == "flclash" {
            // `[Path(args.path)] if args.path else …` — Python's truthiness,
            // so `--path ""` uses the defaults instead of naming ".".
            let cands = match args.path.as_deref().filter(|p| !p.is_empty()) {
                Some(p) => vec![pypath::path_str(p)],
                None if source == "clash-verge" => clash_verge_dirs(),
                None => flclash_dirs(),
            };
            let Some(root) = first_existing(&cands) else {
                eprintln!("error: no {source} data found (looked in: {})", cands.join(", "));
                return Ok(None);
            };
            eprintln!("reading {source} from {root}");
            import_clash_dir(&fs, &root, &mut skipped).map(Some)
        } else {
            // `[Path(args.path)] if args.path else …` — Python's truthiness,
            // so `--path ""` uses the defaults instead of naming ".".
            let cands = match args.path.as_deref().filter(|p| !p.is_empty()) {
                Some(p) => vec![pypath::path_str(p)],
                None => v2box_dbs(),
            };
            let Some(db) = first_existing(&cands) else {
                eprintln!("error: no V2Box DB found (looked in: {})", cands.join(", "));
                return Ok(None);
            };
            eprintln!("reading v2box from {db}");
            let rows = v2box_read(&db)?;
            v2box_rows(&rows, &mut skipped).map(Some)
        }
    })();

    let (links, subs) = match result {
        Ok(Some(v)) => v,
        Ok(None) => return 1,
        // `except RuntimeError` — the one exception main turns into a message.
        Err(e) if e.is_runtime() => {
            eprintln!("error: {}", e.msg);
            return 1;
        }
        Err(e) => die(&e),
    };

    let pools: Vec<Option<Value>> = args.existing.iter().map(|p| read_json(p)).collect();
    let texts: Vec<String> =
        args.existing_subs.iter().filter_map(|p| std::fs::read_to_string(p).ok()).collect();
    let review = match build_review(
        &links,
        &subs,
        &mut skipped,
        &existing_keys(&pools),
        &existing_subs(&texts),
    ) {
        Ok(r) => r,
        Err(e) => die(&e),
    };
    for note in &review.notes {
        eprintln!("{note}");
    }
    let empty = review.value["servers"].as_array().is_some_and(|a| a.is_empty())
        && review.value["subscriptions"].as_array().is_some_and(|a| a.is_empty());
    if empty {
        eprintln!("note: nothing new to import (already in your pool / subscriptions)");
    }
    print!("{}", foreign::render(&review.value));
    0
}

fn main() {
    std::process::exit(run())
}
