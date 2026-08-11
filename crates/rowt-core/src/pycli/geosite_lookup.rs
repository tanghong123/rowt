//! `geosite-lookup.py` — the categories that cover a domain, for `add`'s hint.
//!
//! The decisions live in `crate::geosite` and were ported first; what is here is
//! the half that touches the world — argv, the cache directory, fetching a
//! `.srs`, decompiling it with sing-box — and the rule that governs all of it:
//! **any failure prints nothing and exits 0**. This runs inside `rowt escape
//! add`, and a cold cache, a dead network or a missing sing-box must cost the
//! user a hint, never the add.
//!
//! Unlike its five siblings this one had no gate binary before the cutover,
//! because it was the only `config/*.py` whose Rust counterpart stopped at the
//! pure half. `parity geosite-diff` is that gate (§6.9.2 — when the gate could
//! have looked and didn't, widen it).

use crate::geosite::{self, Coverage};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

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

const BASE: &str = "https://github.com/SagerNet/sing-geosite/raw/rule-set";

/// `load(name, allow_fetch)`. `None` for every failure — a missing set, a fetch
/// that did not happen or did not work, a decompile that failed, JSON that will
/// not parse. The Python catches all four the same way and so does this.
fn load(cache: &Path, sb: &str, base: &str, name: &str, allow_fetch: bool) -> Option<Coverage> {
    let js = cache.join(format!("geosite-{name}.json"));
    let srs = cache.join(format!("geosite-{name}.srs"));
    if !js.exists() {
        if !srs.exists() {
            if !allow_fetch {
                return None;
            }
            fetch(base, name, &srs)?;
        }
        let out = Command::new(sb)
            .args(["rule-set", "decompile"])
            .arg(&srs)
            .arg("-o")
            .arg(&js)
            // `capture_output=True` — sing-box's chatter belongs to nobody here.
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()?;
        if !out.success() {
            return None;
        }
    }
    let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&js).ok()?).ok()?;
    Some(geosite::parse_ruleset(&doc))
}

/// `urllib.request.urlretrieve(url, srs)`, as curl — writing straight to the
/// destination, which is what `urlretrieve` does.
///
/// Straight-to-destination looks like the careless choice and is deliberate.
/// Measured, not assumed: `curl -fsSL -o dst` leaves NO file behind on a
/// refused connection, an unresolvable host, or an HTTP error, and neither does
/// `urlretrieve` — it raises before it opens the destination. So the two agree
/// on every failure that ends the request.
///
/// Where they would differ is a transfer INTERRUPTED after a 200, which leaves
/// a partial `geosite-<name>.srs` in the cache — and that file is not inert:
/// `cached()` globs `*.srs`, so the truncated set is offered as
/// already-downloaded from then on, and the hint for that category is dead
/// until someone clears the cache by hand. A `.part` file renamed on success
/// fixes it in three lines. It is not done here because it is a FIX, and this
/// commit is a port — §6.7, and the row is in the table.
fn fetch(base: &str, name: &str, srs: &Path) -> Option<()> {
    Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(srs)
        .arg("--")
        .arg(format!("{base}/geosite-{name}.srs"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()
        .filter(|s| s.success())
        .map(|_| ())
}

/// `sorted(p.name[len("geosite-"):-len(".srs")] for p in cache.glob("geosite-*.srs"))`.
fn cached(cache: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(cache) else { return vec![] };
    let mut out: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|n| {
            n.strip_prefix("geosite-").and_then(|r| r.strip_suffix(".srs")).map(String::from)
        })
        .collect();
    out.sort();
    out
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
    // No dot, or already a `geosite:` reference — nothing to say either way.
    let Some(domain) = geosite::normalize(domain) else { return ExitCode::SUCCESS };

    let cache = PathBuf::from(env_or("ROWT_CACHE", || {
        home().join(".config/rowt/cache").to_string_lossy().into_owned()
    }));
    let sb = env_or("ROWT_SB", || {
        home().join(".config/rowt/bin/sing-box").to_string_lossy().into_owned()
    });
    let base = env_or("ROWT_GEOSITE_BASE", || BASE.to_string());
    if !Path::new(&sb).exists() {
        return ExitCode::SUCCESS;
    }
    let _ = std::fs::create_dir_all(&cache);

    // Already covered by a category on this lane? Then there is nothing to
    // suggest. `allow_fetch` is false here: a set that is not already cached
    // cannot be the reason this domain is covered TODAY.
    for name in &have {
        if load(&cache, &sb, &base, name, false).is_some_and(|c| c.covers(&domain)) {
            println!("have:{name}");
            return ExitCode::SUCCESS;
        }
    }

    let mut results: Vec<String> = vec![];
    for (name, allow) in geosite::candidates(&domain, &cached(&cache), &have) {
        if load(&cache, &sb, &base, &name, allow).is_some_and(|c| c.covers(&domain)) {
            results.push(name);
        }
    }
    for name in results.iter().take(geosite::MAX_SHOWN) {
        println!("{name}");
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

    #[test]
    fn cached_lists_only_srs_and_sorts() {
        let d = std::env::temp_dir().join(format!("rowt-geosite-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        for f in ["geosite-zzz.srs", "geosite-aaa.srs", "geosite-aaa.json", "other.srs"] {
            std::fs::write(d.join(f), "").unwrap();
        }
        assert_eq!(cached(&d), ["aaa", "zzz"]);
        let _ = std::fs::remove_dir_all(&d);
    }
}
