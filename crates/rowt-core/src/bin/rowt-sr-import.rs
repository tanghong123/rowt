//! `rowt-sr-import` — argv-for-argv what `config/sr-import.py` is, so the two
//! can be run against each other by `parity sr-diff`.
//!
//! The semantics live in `rowt_core::srimport`, the container format in
//! `rowt_core::bplist`, and the discovery in `rowt_core::srio`. What is left
//! here is the argparse surface — which is part of the contract, since
//! `bin/rowt` reads the exit status and passes stderr straight through — plus
//! the two ways the Python fails: a traceback, and the one refusal this reader
//! adds.

use rowt_core::foreign::Exc;
use rowt_core::srimport;
use rowt_core::srio::{self, SrErr};

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

fn main() {
    let args = parse_args();
    let store = srio::resolve_store(args.store.as_deref());

    if args.detect {
        if srio::detect(&store, args.conf.as_deref()) {
            println!("shadowrocket");
        }
        return;
    }

    match srio::extract(store.as_deref(), args.conf.as_deref()) {
        Ok(x) => print!("{}", srimport::render(&x)),
        Err(SrErr::Exc(e)) => die(&e),
        Err(SrErr::Xml(path)) => {
            eprintln!("error: {path} is an XML plist; only the binary format is read");
            std::process::exit(1)
        }
    }
}
