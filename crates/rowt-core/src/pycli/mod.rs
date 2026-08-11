//! The `config/*.py` command-line surfaces, in Rust.
//!
//! Each submodule is named after the script it replaces and is argv-for-argv
//! what that script is: same flags, same three streams, same exit status,
//! including what argparse writes to stderr. That fidelity is not politeness —
//! `bin/rowt` reads these exit statuses and passes their stderr straight
//! through to the terminal, and `parity {vless,merge,foreign,sr,netdetect}-diff`
//! compares the bytes.
//!
//! These live in the LIBRARY rather than in `src/bin/` so there is exactly one
//! implementation behind two callers: the thin gate binaries (`rowt-vless` and
//! friends), which the differential gates replay against the Python, and
//! `rowt-rs _py <tool>`, which is what `bin/rowt` actually runs once
//! `ROWT_PY=1` is not set. If the two had separate argument parsers, a
//! divergence between them would be invisible to every gate — the gate would be
//! proving something about a parser the product never reaches.
//!
//! **They exit the process.** `ap_error`, `die` and the traceback paths call
//! `std::process::exit`, because reproducing argparse means reproducing when it
//! bails. Both callers are dedicated processes, and `rowt-rs` dispatches `_py`
//! *before* its preamble for that reason (see `main.rs`): an exit from in here
//! must not be able to skip an audit END, so there must not be one open.

pub mod corp_sync_reconcile;
pub mod foreign_import;
pub mod geosite_lookup;
pub mod import_merge;
pub mod net_detect;
pub mod sr_import;
pub mod vless_parse;

/// Dispatch by script name, minus the `.py`. `None` for an unknown tool — the
/// caller decides whether that is a usage error or a fallthrough.
pub fn dispatch(tool: &str, argv: &[String]) -> Option<std::process::ExitCode> {
    Some(match tool {
        "vless-parse" => vless_parse::main(argv),
        "import-merge" => import_merge::main(argv),
        "corp-sync-reconcile" => corp_sync_reconcile::main(argv),
        "net-detect" => net_detect::main(argv),
        "foreign-import" => foreign_import::main(argv),
        "sr-import" => sr_import::main(argv),
        "geosite-lookup" => geosite_lookup::main(argv),
        _ => return None,
    })
}

/// Every tool `dispatch` answers to, for the usage message and for the gate
/// that checks the shell and the binary agree on the list.
pub const TOOLS: [&str; 7] = [
    "vless-parse",
    "import-merge",
    "corp-sync-reconcile",
    "net-detect",
    "foreign-import",
    "sr-import",
    "geosite-lookup",
];
