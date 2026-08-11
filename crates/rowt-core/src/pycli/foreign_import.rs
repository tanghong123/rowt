//! `rowt-foreign-import` — argv-for-argv what `config/foreign-import.py` is, so
//! the two can be run against each other by `parity foreign-diff`.
//!
//! The semantics live in `rowt_core::foreign` and the I/O in
//! `rowt_core::foreignio`; what is left here is the exact argparse surface,
//! which is part of the contract — `bin/rowt` reads the exit status and passes
//! stderr straight through to the terminal.
//!
//! Uncaught exceptions are reproduced as exit 1 with a `Traceback` header and
//! the exception's qualified name, which is what the gate compares. The
//! traceback BODY is normalized away on both sides: frame lines and message
//! wording are interpreter-version detail, while "did this input crash, and
//! with what kind of error" is the thing that keeps garbage out of a pool.

use crate::foreign::{self, build_review, existing_keys, existing_subs, Counter, Exc};
use crate::foreignio::{detect, extract, read_json};
use crate::pyurl;
use serde_json::Value;
use std::process::ExitCode;

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

fn parse_args(argv: &[String]) -> Args {
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

// ---------------------------------------------------------------------------

fn run(argv: &[String]) -> i32 {
    let args = parse_args(argv);

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
    let result = extract(&source, args.path.as_deref(), &mut skipped);

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

pub fn main(argv: &[String]) -> ExitCode {
    ExitCode::from(run(argv) as u8)
}
