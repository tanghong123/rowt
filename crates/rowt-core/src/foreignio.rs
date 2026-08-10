//! The I/O half of `config/foreign-import.py` — where it looks, what it shells
//! out to, and how it reads a V2Box store.
//!
//! Split out of `bin/rowt-foreign-import.rs` so that both callers can have it:
//! that binary, which exists to be argv-for-argv what the Python is so
//! `parity foreign-diff` can compare them, and `rowt server import`, which
//! should no longer be running a Python at all. Leaving it in the binary would
//! have left the product either exec'ing a gate artifact or carrying a second
//! copy of the search paths — and the search paths are the part most likely to
//! drift, since they are a list of other applications' install locations.
//!
//! [`foreign`](crate::foreign) stays pure: given a directory listing and some
//! parsed YAML it decides what the servers are. This module is what turns a
//! real machine into those inputs.

use crate::foreign::{self, Counter, Exc, Fs, PyVal, R};
use crate::sharelink::{py_json_loads, strip};
use crate::pypath;
use serde_json::Value;

pub fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

fn under(rel: &str) -> String {
    foreign::join(&home(), rel)
}

pub fn clash_verge_dirs() -> Vec<String> {
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

pub fn flclash_dirs() -> Vec<String> {
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

pub fn v2box_dbs() -> Vec<String> {
    vec![under("Library/Group Containers/group.hossin.asaadi.V2Box/DB.sqlite")]
}

pub fn first_existing(paths: &[String]) -> Option<String> {
    paths.iter().find(|p| std::path::Path::new(p).exists()).cloned()
}

/// `shutil.which` — a PATH entry that is empty means the working directory, and
/// the hit has to be an executable file.
pub fn which(cmd: &str) -> Option<String> {
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

pub struct RealFs;

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
pub fn v2box_read(db: &str) -> R<Vec<(PyVal, PyVal, PyVal)>> {
    use rusqlite::{types::ValueRef, OpenFlags};
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI;
    let con = rusqlite::Connection::open_with_flags(format!("file:{db}?mode=ro"), flags)
        .map_err(sqlite_exc)?;
    let mut stmt =
        con.prepare("SELECT ZTYPE, ZURL, ZSUBSCRIBE FROM ZCDV2RAYITEM").map_err(sqlite_exc)?;
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
                    Err(_) => {
                        Err(Exc::new("sqlite3.OperationalError", "Could not decode to UTF-8 column"))
                    }
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
pub fn detect() -> Vec<String> {
    let fs = RealFs;
    let mut found = Vec::new();
    if let Some(cv) = first_existing(&clash_verge_dirs()) {
        if fs.exists(&foreign::join(&cv, "profiles.yaml")) || fs.is_dir(&foreign::join(&cv, "profiles"))
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

/// The extract step: find the client's data and read it into share links plus
/// subscription URLs.
///
/// `Ok(None)` is the Python's "no data found" return — the message is already
/// on stderr and `main` leaves with 1 before any review exists. The two `==>`-
/// less lines it prints along the way are the Python's, and `bin/rowt` passes
/// them straight to the terminal, so they stay here rather than becoming
/// return values nobody would print.
pub fn extract(
    source: &str,
    path: Option<&str>,
    skipped: &mut Counter,
) -> R<Option<(Vec<String>, Vec<Value>)>> {
    let fs = RealFs;
    // `[Path(args.path)] if args.path else …` — Python's truthiness, so
    // `--path ""` uses the defaults instead of naming ".".
    let given = path.filter(|p| !p.is_empty());
    if source == "clash-verge" || source == "flclash" {
        let cands = match given {
            Some(p) => vec![pypath::path_str(p)],
            None if source == "clash-verge" => clash_verge_dirs(),
            None => flclash_dirs(),
        };
        let Some(root) = first_existing(&cands) else {
            eprintln!("error: no {source} data found (looked in: {})", cands.join(", "));
            return Ok(None);
        };
        eprintln!("reading {source} from {root}");
        foreign::import_clash_dir(&fs, &root, skipped).map(Some)
    } else {
        let cands = match given {
            Some(p) => vec![pypath::path_str(p)],
            None => v2box_dbs(),
        };
        let Some(db) = first_existing(&cands) else {
            eprintln!("error: no V2Box DB found (looked in: {})", cands.join(", "));
            return Ok(None);
        };
        eprintln!("reading v2box from {db}");
        let rows = v2box_read(&db)?;
        foreign::v2box_rows(&rows, skipped).map(Some)
    }
}

/// `--existing FILE` — an existing pool, unreadable or malformed reading as
/// absent (`foreign::existing_keys` takes the Nones).
pub fn read_json(path: &str) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}
