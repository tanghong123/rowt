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

    // The static text blocks come out of the shell too. They are pure output
    // with no logic in them, so transcribing them into Rust string literals
    // would add nothing but a second copy to keep in sync — and the whole point
    // of this port is that the shell is the specification until it isn't.
    let body = std::fs::read_to_string(&shell).unwrap_or_default();
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    for (name, marker) in [("shell_init", "SH"), ("completion_zsh", "ZSH"), ("completion_bash", "BASH")] {
        let open = format!("<<'{marker}'\n");
        let text = body
            .split_once(&open)
            .and_then(|(_, rest)| rest.split_once(&format!("\n{marker}\n")).map(|(t, _)| t))
            .unwrap_or("");
        std::fs::write(out.join(format!("{name}.txt")), format!("{text}\n")).unwrap();
    }
}
