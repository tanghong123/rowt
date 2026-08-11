//! `rowt-netdetect` — the Rust half of the net-detect differential gate.
//!
//! Prints the same JSON `config/net-detect.py` prints, from the same input, so
//! `parity netdetect-diff` can replay both over identical fixtures. Kept as its
//! own binary rather than folded into the CLI for exactly that reason: the gate
//! needs to run the parser with no config directory and no machine around it.

use crate::netdetect::parse;
use std::process::ExitCode;

pub fn main(argv: &[String]) -> ExitCode {
    let mut args = argv.iter().cloned();
    let mut input: Option<String> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input" => input = args.next(),
            other => {
                eprintln!("rowt-netdetect: unknown argument: {other}");
                return ExitCode::FAILURE;
            }
        }
    }
    let text = match input {
        Some(p) => std::fs::read_to_string(p).unwrap_or_default(),
        None => std::process::Command::new("scutil").arg("--dns")
            .output().ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default(),
    };
    let d = parse(&text);
    // `json.dump(..., indent=2)` then a trailing newline — matched so the gate
    // can compare bytes rather than re-parse and lose the ordering guarantee.
    let v = serde_json::json!({
        "internal_domains": d.internal_domains,
        "physical_search": d.physical_search,
        "corp_nameservers": d.corp_nameservers,
    });
    println!("{}", serde_json::to_string_pretty(&v).unwrap());
    ExitCode::SUCCESS
}
