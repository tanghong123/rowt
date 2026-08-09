//! `rowt-vless` — argv-for-argv what `config/vless-parse.py` is, so the two can
//! be run against each other by `parity vless-diff`.
//!
//! It answers to the Python's program name in its usage text on purpose: the
//! contract under test is "same argv in, same three streams out", and the prog
//! name is part of what argparse writes to stderr. Once bash is gone this binary
//! stays as the gate's other half; `rowt server add` calls the library directly.

use rowt_core::sharelink::{self, Batch};
use serde_json::Value;
use std::io::Read;
use std::process::{Command, Stdio};

const USAGE: &str = "usage: vless-parse.py [-h] [--tag TAG] [--multi] [--sub URL] [--combine]\n                      [link]";

const HELP: &str = "\nshare link(s) -> sing-box outbound JSON\n\npositional arguments:\n  link        a vless:// / anytls:// share link\n\noptions:\n  -h, --help  show this help message and exit\n  --tag TAG   outbound tag in single mode\n  --multi     read links from stdin -> array\n  --sub URL   fetch a subscription URL -> array\n  --combine   dedupe an array read from stdin";

/// `ap.error(msg)` — usage on stderr, then the complaint, then exit 2.
fn ap_error(msg: &str) -> ! {
    eprintln!("{USAGE}");
    eprintln!("vless-parse.py: error: {msg}");
    std::process::exit(2)
}

/// `print(f"error: {e}", file=sys.stderr); return 1` — every ValueError the
/// script catches lands here.
fn fail(b: &Batch, msg: &str) -> ! {
    for w in &b.warnings {
        eprintln!("{w}");
    }
    eprintln!("error: {msg}");
    std::process::exit(1)
}

fn stdin_string() -> String {
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s).unwrap_or(0);
    s
}

/// The IO half of `fetch_subscription`. urlopen's failures are not ValueErrors,
/// so the Python dies with a traceback and exit 1; curl failing gives the same
/// status, which is all the shell caller reads (`2>/dev/null`, status-checked).
fn fetch(url: &str) -> Result<String, ()> {
    let ua = std::env::var("ROWT_SUB_UA")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "Shadowrocket/2.2.28 (iPhone; iOS 17.5.1; Scale/3.00)".into());
    let out = Command::new("curl")
        .args(["-fsSL", "--max-time", "20", "-A", &ua, "--", url])
        .stderr(Stdio::null())
        .output()
        .map_err(|_| ())?;
    if !out.status.success() {
        return Err(());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn emit(b: Batch, result: Value) -> ! {
    for w in &b.warnings {
        eprintln!("{w}");
    }
    print!("{}", sharelink::render(&result));
    std::process::exit(0)
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let (mut tag, mut multi, mut combine, mut sub, mut link) =
        ("escape".to_string(), false, false, None::<String>, None::<String>);

    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                println!("{HELP}");
                return;
            }
            "--multi" => multi = true,
            "--combine" => combine = true,
            "--tag" => {
                i += 1;
                match argv.get(i) {
                    Some(v) => tag = v.clone(),
                    None => ap_error("argument --tag: expected one argument"),
                }
            }
            "--sub" => {
                i += 1;
                match argv.get(i) {
                    Some(v) => sub = Some(v.clone()),
                    None => ap_error("argument --sub: expected one argument"),
                }
            }
            a if a.starts_with('-') && a != "-" => {
                ap_error(&format!("unrecognized arguments: {a}"))
            }
            a => {
                if link.is_some() {
                    ap_error(&format!("unrecognized arguments: {a}"));
                }
                link = Some(a.to_string());
            }
        }
        i += 1;
    }

    // The same precedence main() has: --combine, then --sub, then --multi, then
    // a bare link. Passing several is not an error, the first one wins.
    if combine {
        let text = stdin_string();
        let v = match sharelink::py_json_loads(&text) {
            Ok(v) => v,
            Err(e) => fail(&Batch::default(), &e),
        };
        let Some(arr) = v.as_array() else {
            // Python iterates whatever it got and dies on the first element
            // that has no .get — a traceback, not this message. Reproduced as a
            // clean failure because a traceback is not a contract worth keeping.
            fail(&Batch::default(), "combine expects a JSON array of outbounds");
        };
        let b = sharelink::combine(arr);
        let out = Value::Array(b.outbounds.clone());
        emit(b, out);
    }

    let links: Vec<String> = if let Some(url) = sub {
        let Ok(body) = fetch(&url) else { std::process::exit(1) };
        match sharelink::decode_subscription(&body) {
            Ok(l) => l,
            Err(e) => fail(&Batch::default(), &e),
        }
    } else if multi {
        // `sys.stdin.read().splitlines()` — not `.lines()`, which would miss
        // every separator Python recognises beyond `\n`.
        sharelink::splitlines(&stdin_string())
    } else if let Some(l) = link {
        match sharelink::parse_link(sharelink::strip(&l), &tag) {
            Ok(v) => emit(Batch::default(), v),
            Err(e) => fail(&Batch::default(), &e),
        }
    } else {
        ap_error("provide a link, --multi, --sub, or --combine")
    };

    match sharelink::parse_many(&links) {
        Ok(b) => {
            let out = Value::Array(b.outbounds.clone());
            emit(b, out);
        }
        Err((b, e)) => fail(&b, &e),
    }
}
