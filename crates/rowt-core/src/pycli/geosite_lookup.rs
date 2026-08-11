//! `geosite-lookup.py` — the categories that cover a domain, for `add`'s hint.
//!
//! Argv and printing only. The lookup itself — cache, fetch, decompile, the
//! candidate order — is `geosite::lookup`, shared with `rowt-rs`'s own lane-add
//! hint, because two copies of it is exactly how the hint came to be missing
//! from rowt-rs while this file had it.
//!
//! The rule that governs all of it: **any failure prints nothing and exits 0**.
//! This runs inside `rowt escape add`, and a cold cache, a dead network or a
//! missing sing-box must cost the user a hint, never the add.
//!
//! Unlike its five siblings this one had no gate binary before the cutover,
//! because it was the only `config/*.py` whose Rust counterpart stopped at the
//! pure half. `parity geosite-diff` is that gate (§6.9.2 — when the gate could
//! have looked and didn't, widen it).

use crate::geosite::{self, Env, Lookup};
use std::path::PathBuf;
use std::process::ExitCode;

/// `os.environ.get(k) or fallback` — an empty value is absent, not an override.
fn env_or(key: &str, fallback: impl FnOnce() -> String) -> String {
    match std::env::var(key) {
        Ok(v) if !v.is_empty() => v,
        _ => fallback(),
    }
}

fn home() -> PathBuf {
    // `Path.home()` — HOME, and only HOME on the platforms this runs on.
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

pub fn main(argv: &[String]) -> ExitCode {
    // The Python's loop, quirks included: the LAST positional wins, and a
    // trailing `--have` with nothing after it fails the `i + 1 < len` guard and
    // falls to the else, becoming the domain. Both are reproduced — this is
    // called with a domain the user typed, so "what does it do with the odd
    // one" is a real question and not a hypothetical.
    let (mut domain, mut have): (Option<&str>, Vec<String>) = (None, vec![]);
    let mut i = 0;
    while i < argv.len() {
        if argv[i] == "--have" && i + 1 < argv.len() {
            have = argv[i + 1].split(',').filter(|x| !x.is_empty()).map(String::from).collect();
            i += 2;
        } else {
            domain = Some(&argv[i]);
            i += 1;
        }
    }
    let Some(domain) = domain else { return ExitCode::SUCCESS };

    let env = Env {
        cache: PathBuf::from(env_or("ROWT_CACHE", || {
            home().join(".config/rowt/cache").to_string_lossy().into_owned()
        })),
        sb: env_or("ROWT_SB", || {
            home().join(".config/rowt/bin/sing-box").to_string_lossy().into_owned()
        }),
        base: env_or("ROWT_GEOSITE_BASE", || geosite::BASE.to_string()),
    };
    match geosite::lookup(&env, domain, &have) {
        // Declined — no sing-box, or an input that cannot be a domain. Silence
        // and exit 0, same as every other failure here.
        None => {}
        Some(Lookup::Have(name)) => println!("have:{name}"),
        Some(Lookup::Suggest(names)) => {
            for name in names {
                println!("{name}");
            }
        }
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_positional_wins_and_a_dangling_have_is_one() {
        // Not a design choice — the Python's arg loop does this, and the hint is
        // printed for whatever it lands on.
        let argv: Vec<String> =
            ["a.example.com", "b.example.com"].iter().map(|s| s.to_string()).collect();
        let (mut domain, mut i) = (None, 0);
        while i < argv.len() {
            if argv[i] == "--have" && i + 1 < argv.len() { i += 2 } else { domain = Some(&argv[i]); i += 1 }
        }
        assert_eq!(domain.map(|s| s.as_str()), Some("b.example.com"));
    }

    #[test]
    fn an_empty_env_var_is_absent_not_an_override() {
        std::env::set_var("ROWT_TEST_EMPTY", "");
        assert_eq!(env_or("ROWT_TEST_EMPTY", || "fallback".into()), "fallback");
        std::env::remove_var("ROWT_TEST_EMPTY");
    }

}
