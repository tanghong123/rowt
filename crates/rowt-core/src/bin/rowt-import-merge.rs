//! `rowt-import-merge` — argv-for-argv what `config/import-merge.py` is, so the
//! two can be run against each other by `parity merge-diff`.
//!
//! Like `rowt-vless` it answers to the Python's program name in its usage text:
//! the contract under test includes what argparse writes to stderr.

use rowt_core::importmerge;
use serde_json::Value;
use std::io::Read;
use std::path::Path;

const USAGE: &str = "usage: import-merge.py [-h] --into INTO --add ADD [--source SOURCE]\n                       [--pool POOL] [--pool-subs POOL_SUBS]";

const HELP: &str = "\naccumulate one client's extract into a review file\n\noptions:\n  -h, --help            show this help message and exit\n  --into INTO\n  --add ADD\n  --source SOURCE\n  --pool POOL\n  --pool-subs POOL_SUBS";

fn ap_error(msg: &str) -> ! {
    eprintln!("{USAGE}");
    eprintln!("import-merge.py: error: {msg}");
    std::process::exit(2)
}

/// `_load` — a missing file, an unreadable one and malformed JSON are all the
/// same answer. That is what makes a typo'd `--pool` a silent no-op instead of
/// an error, and it is reproduced rather than tightened.
fn load(path: &str) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn stdin_string() -> String {
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s).unwrap_or(0);
    s
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let (mut into, mut add, mut source) = (None::<String>, None::<String>, String::new());
    let (mut pools, mut pool_subs): (Vec<String>, Vec<String>) = (vec![], vec![]);

    let mut i = 0;
    while i < argv.len() {
        let flag = argv[i].as_str();
        let value = |i: &mut usize| -> String {
            *i += 1;
            match argv.get(*i) {
                Some(v) => v.clone(),
                None => ap_error(&format!("argument {flag}: expected one argument")),
            }
        };
        match flag {
            "-h" | "--help" => {
                println!("{USAGE}");
                println!("{HELP}");
                return;
            }
            "--into" => into = Some(value(&mut i)),
            "--add" => add = Some(value(&mut i)),
            "--source" => source = value(&mut i),
            "--pool" => pools.push(value(&mut i)),
            "--pool-subs" => pool_subs.push(value(&mut i)),
            other => ap_error(&format!("unrecognized arguments: {other}")),
        }
        i += 1;
    }

    // argparse names every missing required argument in one message, in the
    // order they were declared.
    let missing: Vec<&str> = [("--into", into.is_none()), ("--add", add.is_none())]
        .iter()
        .filter(|(_, m)| *m)
        .map(|(n, _)| *n)
        .collect();
    if !missing.is_empty() {
        ap_error(&format!("the following arguments are required: {}", missing.join(", ")));
    }
    let (into, add_path) = (into.unwrap(), add.unwrap());

    let add_val = if add_path == "-" {
        serde_json::from_str::<Value>(&stdin_string()).ok()
    } else {
        load(&add_path)
    };
    let Some(Value::Object(add_obj)) = add_val else {
        eprintln!("error: --add is not a review JSON object");
        std::process::exit(1);
    };

    let acc_in = if into == "-" { None } else { load(&into) };
    let pool_vals: Vec<Option<Value>> = pools.iter().map(|p| load(p)).collect();
    // A --pool-subs that cannot be read contributes nothing (`except OSError`).
    let sub_texts: Vec<String> =
        pool_subs.iter().filter_map(|p| std::fs::read_to_string(p).ok()).collect();

    let m = importmerge::merge(acc_in, &add_obj, &source, &pool_vals, &sub_texts);
    let text = importmerge::render(&m.acc);
    if into == "-" {
        print!("{text}");
    } else if std::fs::write(Path::new(&into), &text).is_err() {
        // write_text raising is an uncaught traceback in the Python; exiting 1
        // is the closest honest thing, and the shell only reads the status.
        eprintln!("error: could not write {into}");
        std::process::exit(1);
    }
    eprintln!("{}", m.summary);
}
