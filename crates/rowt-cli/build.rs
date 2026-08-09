//! The version comes from bin/rowt, not from Cargo.toml.
//!
//! While the shell is authoritative, `rowt version` must agree with it exactly —
//! and a number maintained in two places is a number that will disagree. Read it
//! at build time so drift is impossible rather than merely unlikely.

use std::path::Path;

fn main() {
    let shell = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bin/rowt");
    println!("cargo:rerun-if-changed={}", shell.display());
    let version = std::fs::read_to_string(&shell)
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("ROWT_VERSION="))
                .and_then(|l| l.split('"').nth(1).map(|v| v.to_string()))
        })
        .unwrap_or_else(|| "0.0.0".into());
    println!("cargo:rustc-env=ROWT_SHELL_VERSION={version}");
}
