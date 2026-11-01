//! `rowt-vless` — argv-for-argv what `config/vless-parse.py` is, so the two can
//! be run against each other by `parity vless-diff`.
//!
//! It answers to the Python's program name in its usage text on purpose: the
//! contract under test is "same argv in, same three streams out", and the prog
//! name is part of what argparse writes to stderr. Once bash is gone this binary
//! stays as the gate's other half; `rowt server add` calls the library directly.

use crate::sharelink::{self, Batch};
use serde_json::Value;
use std::io::Read;
use std::process::{Command, ExitCode, Stdio};

const USAGE: &str = "usage: vless-parse.py [-h] [--tag TAG] [--multi] [--sub URL] [--combine]\n                      [--links]\n                      [link]";

const HELP: &str = "\nshare link(s) -> sing-box outbound JSON\n\npositional arguments:\n  link        a share link: vless, vmess, anytls, hysteria2, ss, trojan or\n              tuic\n\noptions:\n  -h, --help  show this help message and exit\n  --tag TAG   outbound tag in single mode\n  --multi     read links from stdin -> array\n  --sub URL   fetch a subscription URL -> array\n  --combine   dedupe an array read from stdin\n  --links     read an outbound array from stdin -> one share link per line";

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

/// What `type(x).__name__` prints for a value that came out of `json.loads`.
fn py_type_name(v: &Value) -> &'static str {
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

/// An exception nothing caught, the way Python leaves it: frames, then
/// `<qualified name>: <message>`. Only the name survives the gate's normalizer
/// — frame lines are interpreter detail — but the message is written properly
/// because a person reads this too.
fn py_traceback(name: &str, msg: &str) -> ! {
    eprintln!("Traceback (most recent call last):");
    eprintln!("{name}: {msg}");
    std::process::exit(1)
}

fn attribute_error(v: &Value) -> ! {
    py_traceback(
        "AttributeError",
        &format!("'{}' object has no attribute 'get'", py_type_name(v)),
    )
}

/// The HTTP status behind a failed `curl -f`, when there was one: "The
/// requested URL returned error: 404" on stderr. The exit code is no guide —
/// 22 on a direct request, but curl 8.x reports the same 404 through a proxy's
/// CONNECT tunnel as 56. `None` for a failure that never got a response
/// (refused, unresolvable, timed out).
pub fn http_status(stderr: &str) -> Option<u16> {
    let (_, tail) = stderr.split_once("returned error: ")?;
    tail.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

/// http.client's reason phrase for the statuses a subscription host sends.
fn reason_phrase(status: u16) -> &'static str {
    match status {
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        410 => "Gone",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Error",
    }
}

/// The IO half of `fetch_subscription`. urlopen's failures are not ValueErrors,
/// so the Python dies with a traceback and exit 1, and so does this — with the
/// same exception TYPE: `HTTPError` when the server answered with a status,
/// `URLError` when nothing answered. This used to exit 1 silently, which lost
/// the one fact that explains a dead subscription (a 404). The shell reads the
/// `HTTPError` line to put the status on its "subscription fetch failed" line.
fn fetch(url: &str) -> Result<String, ()> {
    let ua = std::env::var("ROWT_SUB_UA")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "Shadowrocket/2.2.28 (iPhone; iOS 17.5.1; Scale/3.00)".into());
    let out = Command::new("curl")
        .args(["-fsSL", "--max-time", "20", "-A", &ua, "--", url])
        .stderr(Stdio::piped())
        .output()
        .map_err(|_| ())?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        match http_status(&err) {
            Some(s) => py_traceback("urllib.error.HTTPError", &format!("HTTP Error {s}: {}", reason_phrase(s))),
            None => {
                // "curl: (7) Failed to connect to …" → the part after the code.
                let why = err.lines().last().unwrap_or("").trim();
                let why = why.split_once(") ").map(|(_, w)| w).unwrap_or(why);
                py_traceback("urllib.error.URLError", &format!("<urlopen error {why}>"))
            }
        }
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

pub fn main(argv: &[String]) -> ExitCode {
    let (mut tag, mut multi, mut combine, mut sub, mut link) =
        ("escape".to_string(), false, false, None::<String>, None::<String>);
    let mut links = false;

    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                println!("{HELP}");
                return ExitCode::SUCCESS;
            }
            "--multi" => multi = true,
            "--combine" => combine = true,
            "--links" => links = true,
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

    // main()'s chain tests TRUTHINESS, not presence — `elif args.sub:` — so
    // `--sub ""` and a bare empty link both fall through to the "provide a
    // link…" usage error rather than being attempted. Dropping the empties here
    // makes the rest of this function read the way the Python does.
    let sub = sub.filter(|s| !s.is_empty());
    // `elif args.links:` — right after --combine, as main() orders them.
    if links && !combine {
        let text = stdin_string();
        let v = match sharelink::py_json_loads(&text) {
            Ok(v) => v,
            Err(e) => fail(&Batch::default(), &e),
        };
        let Some(arr) = v.as_array() else {
            fail(&Batch::default(), "--links wants a JSON array of outbounds");
        };
        for o in arr {
            match sharelink::to_link(o) {
                Some(l) => println!("{l}"),
                None => {
                    let name = o.as_object().and_then(|m| m.get("tag")).map(sharelink::py_str).unwrap_or_else(|| "?".into());
                    eprintln!("warning: '{name}' has settings no share link carries exactly — not exported");
                }
            }
        }
        std::process::exit(0);
    }
    let link = link.filter(|s| !s.is_empty());

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
        // An element that is not an object, though, IS worth keeping, and the
        // difference is who reads the exit status. `combine` calls `.get` on
        // each element in turn, so `[{...}, "hand-edited into nonsense"]` dies
        // in `key_of` with an AttributeError — not a ValueError, so the script's
        // `except` never sees it — and `rowt server add` reports "combine
        // failed" and writes nothing.
        //
        // This used to be a deliberate divergence, and the generator said so:
        // reporting it cleanly is nicer, and while this binary existed only to
        // be replayed by `vless-diff`, its stdout went nowhere and nothing
        // depended on the refusal. bin/rowt runs it now, and "nicer" became
        // "writes the nonsense string into manual.json instead of refusing" —
        // a corrupt pool accepted rather than rejected. `pool.rs::combine` had
        // the type check all along and explained why; this is that check, on
        // the path that grew a caller.
        //
        // The Python prints each duplicate's note as it meets it, so the notes
        // for the elements BEFORE the bad one are already on stderr when the
        // traceback lands.
        if let Some(i) = arr.iter().position(|e| !e.is_object()) {
            for w in sharelink::combine(&arr[..i]).warnings {
                eprintln!("{w}");
            }
            attribute_error(&arr[i]);
        }
        let b = sharelink::combine(arr);
        let out = Value::Array(b.outbounds.clone());
        emit(b, out);
    }

    let links: Vec<String> = if let Some(url) = sub {
        // Two different refusals, both before any socket opens, and curl makes
        // neither — it would invent `http://` and go and fetch. Which one you
        // get is `pyurl::url_type`'s business; see it for why the rule is not
        // the obvious one.
        match crate::pyurl::url_type(&url) {
            // `Request(url)` raises ValueError, which the script CATCHES.
            None => fail(
                &Batch::default(),
                &format!("unknown url type: {}", crate::pyurl::repr(&url)),
            ),
            // A type with no handler raises URLError from `urlopen`, which is
            // not a ValueError and escapes the `except` — so the user sees a
            // traceback. Reproduced rather than tidied: what a mistyped
            // subscription URL does is the same question either way, and the
            // gate compares the exception TYPE, not the frames.
            Some(t) if !crate::pyurl::URL_HANDLERS.contains(&t.as_str()) => py_traceback(
                "urllib.error.URLError",
                &format!("<urlopen error unknown url type: {t}>"),
            ),
            Some(_) => {}
        }
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

#[cfg(test)]
mod tests {
    use super::http_status;

    #[test]
    fn an_http_status_is_read_off_curls_message_whatever_the_exit_code() {
        // curl ≥ 7.76 prints the bare code; older ones add the reason phrase.
        assert_eq!(http_status("curl: (22) The requested URL returned error: 404\n"), Some(404));
        assert_eq!(http_status("curl: (22) The requested URL returned error: 403 Forbidden\n"), Some(403));
        // curl 8.7 through a proxy's CONNECT tunnel: the same 404, exit 56.
        assert_eq!(http_status("curl: (56) The requested URL returned error: 404\n"), Some(404));
        // No response at all: refused, unresolvable, timed out.
        assert_eq!(http_status("curl: (7) Failed to connect to 127.0.0.1 port 9: Connection refused\n"), None);
        assert_eq!(http_status("curl: (28) Operation timed out after 20001 milliseconds\n"), None);
        assert_eq!(http_status("unexpected\n"), None);
    }
}
