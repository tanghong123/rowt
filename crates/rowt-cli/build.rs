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

    // The 500 lines of help text, likewise. These heredocs are UNQUOTED (<<EOF,
    // not <<'EOF'), so the shell interpolates $PROG, $PORT and a dozen others
    // into them; the extracted text keeps the `$NAME` markers and the CLI
    // expands them at runtime against the same values. Anything else would mean
    // maintaining the help in two places, which is how help goes stale.
    // `usage` is three pieces: a heredoc, the command registry rendered by an
    // awk one-liner, then a second heredoc. Extracted as three so the Rust side
    // reproduces the awk's `%-42s` layout rather than freezing its output — the
    // registry is the file people actually edit when they add a command.
    let u = between(&body, "\nusage() {\n", "\n}\n");
    let parts: Vec<&str> = u.split("  cat <<EOF\n").collect();
    std::fs::write(out.join("usage_head.txt"), upto_eof(parts.get(1).copied().unwrap_or(""))).unwrap();
    std::fs::write(out.join("usage_tail.txt"), upto_eof(parts.get(2).copied().unwrap_or(""))).unwrap();
    std::fs::write(out.join("registry.txt"), between(&body, "\n_reg() {\ncat <<'REG'\n", "\nREG\n")).unwrap();

    // help_detail is a `case` whose arms are `<pat>) cat <<EOF … EOF\n;;`. Each
    // becomes one record: patterns, US, body, RS. The pattern is kept verbatim
    // (`escape|corp|block`) and split at match time, so adding an alias to the
    // shell adds it here with no Rust change at all.
    let detail = between(&body, "\nhelp_detail() {\n", "\n}\n");
    let mut recs = String::new();
    let mut rest = detail.as_str();
    while let Some(i) = rest.find(") cat <<EOF\n") {
        let pats = rest[..i].rsplit('\n').next().unwrap_or("").trim().to_string();
        let after = &rest[i + ") cat <<EOF\n".len()..];
        let (text, tail) = after.split_once("\nEOF\n").unwrap_or((after, ""));
        // Five of the 32 arms interpolate more than a variable — `$1` for the
        // lane name, and inline `$(… && echo … || echo …)` conditionals. Rather
        // than grow a shell evaluator to render five help pages, they are marked
        // and handed to the shell at runtime; the text still has exactly one
        // home, which is the whole point.
        let dynamic = text.contains("$(") || text.contains("$1");
        recs.push_str(&format!("{pats}\u{1f}{}\u{1f}{text}\n\u{1e}",
                               if dynamic { "shell" } else { "text" }));
        rest = tail;
    }
    std::fs::write(out.join("help_detail.txt"), recs).unwrap();

    // `seed_lists` copies three templates out of the repo's config/ directory.
    // The shell finds them relative to itself; embedding them instead keeps
    // rowt-rs working from target/release, a brew prefix or anywhere else, and
    // they are static text with no logic, so there is nothing to reimplement.
    let cfgdir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config");
    let mut seeds = String::from("pub const SEEDS: [(&str, &str); 3] = [\n");
    for n in ["escape-domains.txt", "corp-domains.txt", "block-domains.txt"] {
        let p = cfgdir.join(n);
        println!("cargo:rerun-if-changed={}", p.display());
        let text = std::fs::read_to_string(&p).unwrap_or_default();
        seeds.push_str(&format!("    ({n:?}, r#\"{text}\"#),\n"));
    }
    seeds.push_str("];\n");
    std::fs::write(out.join("seeds.rs"), seeds).unwrap();
}

/// A heredoc body: everything up to the closing `EOF` line. The terminator may
/// be the last line with no newline after it, when the caller already trimmed
/// at the enclosing `}`.
fn upto_eof(s: &str) -> String {
    let end = s.split('\n').scan(0usize, |at, l| {
        let start = *at;
        *at += l.len() + 1;
        Some((start, l))
    }).find(|(_, l)| *l == "EOF").map(|(i, _)| i);
    match end {
        Some(i) => s[..i].to_string(),
        None => String::new(),
    }
}

fn between(body: &str, open: &str, close: &str) -> String {
    body.split_once(open)
        .and_then(|(_, rest)| rest.split_once(close).map(|(t, _)| t.to_string()))
        .unwrap_or_default()
}
