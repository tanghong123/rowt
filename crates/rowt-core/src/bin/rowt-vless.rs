//! `rowt-vless` — the gate's half of `config/vless-parse.py`, for `parity vless-diff`.
//!
//! A shim on purpose. The surface itself is `rowt_core::pycli::vless_parse`,
//! which is also what `rowt-rs _py` runs, so the gate replays the Python
//! against the code the product uses rather than against a second parser
//! written to match it.

fn main() -> std::process::ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    rowt_core::pycli::vless_parse::main(&argv)
}
