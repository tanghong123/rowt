//! `rowt-reconcile` — the corp-lane superset reconcile, drop-in for
//! `config/corp-sync-reconcile.py`. Same flags, same stdout contract.

use rowt_core::reconcile::{load, reconcile, render_outcome};
use std::process::ExitCode;

fn read(p: Option<&String>) -> String {
    p.and_then(|x| std::fs::read_to_string(x).ok()).unwrap_or_default()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |flag: &str| -> Option<String> {
        args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
    };
    let (a, h, b, p) = (get("--active"), get("--handadded"), get("--block"), get("--private"));
    let out = reconcile(
        &load(&read(a.as_ref())),
        &load(&read(h.as_ref())),
        &load(&read(b.as_ref())),
        &load(&read(p.as_ref())),
    );
    println!("{}", render_outcome(&out));
    ExitCode::SUCCESS
}
