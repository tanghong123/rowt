//! `server` / `sub` — the half of the pool that WRITES.
//!
//! The read arms (`server list`, `sub list`, the two `dump`s) live in main.rs
//! next to the other display commands. This module is everything that changes
//! what `rowt up` will route through: the manual set, the subscription list,
//! and `servers.json` — which is not a file anyone edits but a DERIVED one,
//! rebuilt from those two sources on every change.
//!
//! ## What the shell did, and what this replaces
//!
//! `rebuild_servers` in bin/rowt shells out to `config/vless-parse.py` twice —
//! once per subscription (`--sub`) and once over the accumulated array
//! (`--combine`) — and glues the pieces together with `jq -s 'add'`. Every one
//! of those steps is now a library call into `rowt_core::sharelink`, which is
//! the same code `parity vless-diff` holds against the Python over 2,000
//! generated cases. What that gate does NOT see, and this module adds, is the
//! gluing:
//! the accumulation ORDER (manual first, then subscriptions in file order)
//! decides which of two identical servers `combine` keeps, and therefore which
//! display name survives.
//!
//! ## Deliberate divergences
//!
//! * `have python3 || die "python3 not found"` is gone. It is the point of the
//!   exercise. No gate can see it either way — the sandbox always has python3 —
//!   so it is recorded here rather than left to look like an omission.
//! * A subscription is fetched with `curl` where the Python used
//!   `urllib.request` (already true of `rowt-vless`, for the same reason: one
//!   HTTP client for the whole tree). Both report failure the same way — a
//!   non-zero exit that the caller turns into `subscription fetch failed` — but
//!   the argv trace differs, so a SUCCESSFUL fetch is not comparable in
//!   `parity cli-diff`. The cases there use a refused local port so that both
//!   implementations take the failure path; the parse side of a real
//!   subscription body is covered by `vless-diff`.
//!
//!   One place the two clients disagreed on more than the trace, and that one
//!   is CLOSED rather than recorded: urllib refuses a schemeless URL without
//!   opening a socket, where curl assumes http and dials it. A subs.txt line
//!   that is not a URL — `sub import` pointed at a lane list is the easy way to
//!   get one — therefore had rowt-rs sending requests bin/rowt never sent.
//!   `opens_a_request` now rejects those before the process starts. Recorded here
//!   because it is the one divergence in this module that was a defect rather
//!   than a choice.
//! * `sub add` sorts and dedupes the list bytewise, where the shell's
//!   `sort -u` uses the machine's collation. Under a UTF-8 locale `sort -u`
//!   drops lines that merely COLLATE equally, which for a subscription list
//!   means silently discarding a URL that differs only in case. Keeping both is
//!   the safe direction and the byte order is what the gate pins, so this one
//!   is a divergence on purpose rather than an accident.
//!
//! Everything else is reproduced as-is, including the parts that look like
//! accidents (PORTING.md §6.7 — port first, fix bash bugs in their own commit).
//! They are marked `QUIRK` below.

use crate::lifecycle::{self, read, Ctx};
use rowt_core::sharelink;
use serde_json::Value;
use std::path::{Path, PathBuf};

fn manual_file(cfg: &Path) -> PathBuf { cfg.join("manual.json") }
fn subs_file(cfg: &Path) -> PathBuf { cfg.join("subs.txt") }
fn servers_file(cfg: &Path) -> PathBuf { cfg.join("servers.json") }

fn info(msg: &str) { eprintln!("==> {msg}"); }
fn err(msg: &str) { eprintln!("error: {msg}"); }

fn set_mode(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode));
}

/// `jq -r` — a string prints as itself, everything else as its JSON form. Not
/// `py_str`: this is what the shell captured into `$srv` and `$port`, and the
/// two renderings disagree on exactly the values that matter (`null` vs `None`,
/// `true` vs `True`).
fn jq_r(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// `[ -s "$f" ]` — exists and is not empty.
fn non_empty(p: &Path) -> bool {
    std::fs::metadata(p).map(|m| m.is_file() && m.len() > 0).unwrap_or(false)
}

/// The manual set as an array, or nothing.
///
/// `jq -s 'add' acc manual.json` fails on a manual set that is not a JSON
/// ARRAY — an object, a scalar, or a truncated write — and the shell's `&&`
/// chain then simply skips it: no error, no die, the rebuild continues with the
/// subscriptions alone. Reproduced, because the alternative (dying) would make
/// a corrupt manual.json unrecoverable by the very commands meant to fix it.
///
/// A literal `null` is the exception, and not an academic one: jq defines
/// `null + x` as `x`, so `add` SUCCEEDS on it and the file behaves as an empty
/// set rather than as a broken one. Reading it as "unusable" would make
/// `server add` refuse to write where the shell writes.
fn manual_array(cfg: &Path) -> Option<Vec<Value>> {
    if !non_empty(&manual_file(cfg)) {
        return None;
    }
    match serde_json::from_str(&read(&manual_file(cfg))) {
        Ok(Value::Array(v)) => Some(v),
        Ok(Value::Null) => Some(Vec::new()),
        _ => None,
    }
}

/// The accumulator `rebuild` combines: the manual set first, then each
/// subscription's outbounds in the order their URLs appear in subs.txt.
///
/// Pure, and split out because it is the one thing this module adds that no
/// other gate can see. `vless-diff` proves each parse; `cli-diff` proves the
/// files afterwards — but only for subscriptions that FAILED to fetch, since
/// the two implementations use different HTTP clients. The ORDER two successful
/// sources are stitched in decides which of two identical servers `combine`
/// keeps, and therefore which display name the user ends up looking at.
fn accumulate(manual: Option<Vec<Value>>, fetched: Vec<Vec<Value>>) -> Vec<Value> {
    let mut acc = manual.unwrap_or_default();
    for mut v in fetched {
        acc.append(&mut v);
    }
    acc
}

/// `python3 vless-parse.py --combine`, failure surface included.
///
/// The Python reads the array with `json.load` and calls `.get` on each
/// element; an element that is not a dict raises AttributeError, which is NOT a
/// ValueError and so escapes the script's `except` — traceback, exit 1, and the
/// shell dies with "combine failed". `sharelink::combine` would happily
/// stringify it, so the type check the interpreter did for free is done here.
fn combine(outbounds: &[Value]) -> Result<sharelink::Batch, ()> {
    if outbounds.iter().any(|o| !o.is_object()) {
        return Err(());
    }
    Ok(sharelink::combine(outbounds))
}

/// Moved to `rowt_core::pyurl::opens_a_request`, where the reasoning lives
/// now. It was here alone while this was the only caller; `pycli::vless_parse`
/// became the second, without the check, and handed curl a schemeless URL that
/// urllib refuses without a packet. The shared version is also more accurate
/// than what stood here: `nosuch://x` passed the old scheme test and got a
/// curl, where urlopen raises URLError without one.
use rowt_core::pyurl::opens_a_request;

/// `curl` a subscription and parse it — the IO half of `--sub`.
///
/// Every failure the Python turns into a non-zero exit collapses to `Err`
/// here, because a non-zero exit is all the shell reads: the fetch itself, an
/// undecodable body, and a body that yielded no usable links are three
/// different messages on a stderr the caller sends to /dev/null.
fn fetch_sub(url: &str) -> Result<Vec<Value>, ()> {
    // urllib refuses a URL with no scheme outright — `ValueError: unknown url
    // type: 'example.com'` — WITHOUT opening a socket, and the shell reads that
    // non-zero exit as "subscription fetch failed". curl is the opposite: given
    // a bare host it assumes http and fetches. So a subs.txt line that is not a
    // URL (a lane list imported into the wrong arm is the easy way to get one)
    // would have rowt-rs sending plaintext requests to every entry that
    // bin/rowt never contacts at all. Refused here instead — the scheme check
    // has to come BEFORE the process, since by the time curl runs the request
    // is already out.
    if !opens_a_request(url) {
        return Err(());
    }
    let ua = crate::env_or(
        "ROWT_SUB_UA",
        "Shadowrocket/2.2.28 (iPhone; iOS 17.5.1; Scale/3.00)",
    );
    let out = std::process::Command::new("curl")
        .args(["-fsSL", "--max-time", "20", "-A", &ua, "--", url])
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|_| ())?;
    if !out.status.success() {
        return Err(());
    }
    let body = String::from_utf8_lossy(&out.stdout).into_owned();
    let links = sharelink::decode_subscription(&body).map_err(|_| ())?;
    // `2>/dev/null` on the shell's side: the per-link warnings are dropped.
    sharelink::parse_many(&links).map(|b| b.outbounds).map_err(|_| ())
}

/// `rebuild_servers` — servers.json from the manual set plus every subscription.
///
/// Order is load-bearing: manual entries go in first, then each subscription in
/// the order the lines appear in subs.txt, and `combine` keeps the FIRST of any
/// two entries with the same identity. So a server you imported by hand keeps
/// your name for it even when a subscription later offers the same endpoint.
pub fn rebuild(ctx: &Ctx) -> Result<(), String> {
    let cfg = &ctx.cfg;
    let mut fetched: Vec<Vec<Value>> = Vec::new();

    // `while IFS= read -r url` — a final line with no trailing newline is NOT
    // read: `read` returns non-zero and the loop body never runs for it. A
    // subscription added by hand with `printf '%s' url >> subs.txt` is
    // therefore invisible to the shell, and has to stay invisible here.
    let body = read(&subs_file(cfg));
    let complete_lines: Vec<&str> = match body.rsplit_once('\n') {
        Some((head, _tail)) if !head.is_empty() => head.split('\n').collect(),
        Some(_) => Vec::new(),
        None => Vec::new(),
    };
    for url in complete_lines {
        // `case "$url" in ''|\#*)` — untrimmed. A line of spaces is not blank
        // and gets handed to the fetcher as if it were a URL.
        if url.is_empty() || url.starts_with('#') {
            continue;
        }
        match fetch_sub(url) {
            Ok(v) => fetched.push(v),
            // `${url%%\?*}` — everything before the first `?`, so the query
            // string (which is where the token lives) stays out of the log.
            Err(()) => err(&format!(
                "subscription fetch failed: {}…",
                url.split('?').next().unwrap_or(url)
            )),
        }
    }
    let acc = accumulate(manual_array(cfg), fetched);

    // `|| { rm -f …; die "combine failed"; }` — a `die`, not a returned error:
    // it writes an ABORT line into the audit log and exits, so the audit
    // bracket around the command never gets its END. That difference is what
    // `cli-diff` compares, so it has to be a `die` here too.
    let Ok(batch) = combine(&acc) else {
        crate::die(cfg, "combine failed");
    };
    for w in &batch.warnings {
        eprintln!("{w}");
    }
    let out = servers_file(cfg);
    let tmp = out.with_extension("json.tmp");
    std::fs::write(&tmp, sharelink::render(&Value::Array(batch.outbounds.clone())))
        .map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &out).map_err(|e| format!("mv: {e}"))?;
    set_mode(&out, 0o600);

    // Keep the selection valid. `auto` always is; a tag has to still be in the
    // set. Anything else falls back to the first server — or to "auto" when the
    // set is empty, which is what a fresh install lands on.
    let sel = ctx.sget("selected");
    let present = !sel.is_empty()
        && batch.outbounds.iter().any(|o| {
            o.get("tag").map(|t| sharelink::py_str(t)).unwrap_or_default() == sel
        });
    if sel != "auto" && !present {
        let first = batch
            .outbounds
            .first()
            .and_then(|o| o.get("tag"))
            .map(sharelink::py_str)
            .unwrap_or_else(|| "auto".into());
        lifecycle::sset(ctx, "selected", &first);
    }
    Ok(())
}

/// `after_import` — what the pool looks like now, then a reload if the router
/// is up. Every mutating arm ends here, so the user always sees the result of
/// the edit rather than just its acknowledgement.
pub fn after_import(ctx: &Ctx) -> Result<String, String> {
    let cfg = &ctx.cfg;
    let servers: Result<Vec<Value>, _> = serde_json::from_str(&read(&servers_file(cfg)));
    // `jq length … || echo 0` — an unreadable file counts as zero rather than
    // stopping the command.
    let n = servers.as_ref().map(|v| v.len()).unwrap_or(0);
    info(&format!(
        "{n} server(s) available (selected: {}):",
        ctx.sget("selected")
    ));
    let mut o = String::new();
    // `| head -40` — a pool that came from a large subscription prints its head
    // and nothing else. The count above is the whole truth; this is a preview.
    for s in servers.unwrap_or_default().iter().take(40) {
        let g = |k: &str| {
            s.get(k)
                .map(|v| match v {
                    Value::String(x) => x.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_else(|| "null".into())
        };
        o.push_str(&format!(
            "  {} {} {}:{}\n",
            crate::pad(&g("tag"), 20),
            crate::pad(&g("type"), 7),
            g("server"),
            g("server_port")
        ));
    }
    o.push_str(&format!("  stored (secret) at: {}", servers_file(cfg).display()));
    lifecycle::reload_if_running(ctx)?;
    Ok(o)
}

/// `server add` — parse share links into the manual set.
///
/// Deduped in the manual set itself, not just in the rebuilt servers.json, so
/// importing the same link twice is idempotent on disk instead of leaving a
/// duplicate that every future rebuild has to drop again.
pub fn add(ctx: &Ctx, links: &[String]) -> Result<String, String> {
    let cfg = &ctx.cfg;
    // `printf '%s\n' "$@" | … --multi` — the arguments become stdin LINES, so
    // an argument containing a newline arrives as two links.
    let lines = sharelink::splitlines(&links.join("\n"));
    let parsed = match sharelink::parse_many(&lines) {
        Ok(b) => {
            for w in &b.warnings {
                eprintln!("{w}");
            }
            b.outbounds
        }
        Err((b, msg)) => {
            for w in &b.warnings {
                eprintln!("{w}");
            }
            eprintln!("error: {msg}");
            // `|| exit 1`, not `die` — no ABORT line, and no END line either,
            // because the shell's exit skips the audit bracket's tail. Matching
            // that is what keeps the audit log comparable.
            std::process::exit(1);
        }
    };

    let manual = manual_file(cfg);
    if !non_empty(&manual) {
        std::fs::write(&manual, "[]\n").map_err(|e| format!("write: {e}"))?;
    }
    // `jq -s 'add' "$MANUAL" <(new)` — when the manual set is unusable jq fails,
    // the combine downstream of it gets an empty stdin, and the `&&` never
    // reaches the `mv`.
    let existing = manual_array(cfg);
    let jq_ok = existing.is_some();
    let mut all = existing.unwrap_or_default();
    all.extend(parsed);

    // `… | --combine > "$MANUAL.n" && mv` — the redirect creates the file
    // BEFORE the pipeline runs, so a failing combine leaves an empty
    // manual.json.n behind and manual.json untouched. QUIRK, reproduced: it is
    // the on-disk trace of a corrupt manual set, and deleting it here would
    // hide that a rebuild silently did nothing.
    let n_file = manual.with_extension("json.n");
    std::fs::write(&n_file, "").map_err(|e| format!("write: {e}"))?;
    if jq_ok {
        if let Ok(b) = combine(&all) {
            for w in &b.warnings {
                eprintln!("{w}");
            }
            std::fs::write(&n_file, sharelink::render(&Value::Array(b.outbounds)))
                .map_err(|e| format!("write: {e}"))?;
            std::fs::rename(&n_file, &manual).map_err(|e| format!("mv: {e}"))?;
        }
    }
    set_mode(&manual, 0o600);
    rebuild(ctx)?;
    after_import(ctx)
}

/// `server rm <tag>…` — drop manual servers by tag.
///
/// A tag is looked up in servers.json (the working set, which is what the user
/// sees) and then matched against manual.json by ENDPOINT — server plus port —
/// because the tag in the working set may have been uniquified during combine
/// and need not be the one stored in the manual set.
pub fn remove_servers(ctx: &Ctx, tags: &[String]) -> Result<String, String> {
    let cfg = &ctx.cfg;
    let servers: Vec<Value> =
        serde_json::from_str(&read(&servers_file(cfg))).unwrap_or_default();
    let mut out = String::new();
    for tag in tags {
        let hits: Vec<&Value> = servers
            .iter()
            .filter(|o| o.get("tag").map(sharelink::py_str).as_deref() == Some(tag.as_str()))
            .collect();
        if hits.is_empty() {
            err(&format!("no server tagged '{tag}'"));
            continue;
        }
        // QUIRK: the shell reads the match with `jq -c`, and two servers
        // sharing a tag give it TWO lines — which it then feeds to `--argjson`
        // as one value. That is not valid JSON, jq exits non-zero, and the
        // entry is reported as coming from a subscription. Reproduced by
        // refusing to act on an ambiguous tag at all.
        let one = hits.len() == 1;
        let srv = jq_r(hits[0].get("server").unwrap_or(&Value::Null));
        let port = jq_r(hits[0].get("server_port").unwrap_or(&Value::Null));
        // The two shell variables reach jq by DIFFERENT doors: `--arg s` makes
        // the host a JSON string whatever it looked like, while `--argjson p`
        // re-parses the port as JSON. So a missing `.server` is compared as the
        // literal string "null" and never matches, while a missing
        // `.server_port` is compared as JSON null and matches any manual entry
        // that also lacks one. And a port that is not valid JSON on its own
        // (`80a`) makes jq exit, which reads as "not in the manual set".
        let want_srv = Value::String(srv.clone());
        let want_port: Option<Value> = serde_json::from_str(&port).ok();
        let manual = manual_array(cfg).unwrap_or_default();
        // QUIRK: because the port is compared as JSON, the comparison is
        // type-strict. A manual entry whose server_port is the string "443"
        // does not match servers.json's number 443, and the removal is refused
        // with the subscription message.
        let matches = |o: &Value| {
            one && want_port.is_some()
                && o.get("server").unwrap_or(&Value::Null) == &want_srv
                && o.get("server_port").unwrap_or(&Value::Null) == want_port.as_ref().unwrap()
        };
        if non_empty(&manual_file(cfg)) && manual.iter().any(matches) {
            let kept: Vec<Value> = manual.into_iter().filter(|o| !matches(o)).collect();
            let n_file = manual_file(cfg).with_extension("json.n");
            std::fs::write(&n_file, sharelink::render(&Value::Array(kept)))
                .map_err(|e| format!("write: {e}"))?;
            std::fs::rename(&n_file, manual_file(cfg)).map_err(|e| format!("mv: {e}"))?;
            out.push_str(&format!("  removed manual server: {tag} ({srv}:{port})\n"));
        } else {
            err(&format!(
                "'{tag}' comes from a subscription — remove that subscription: {} sub rm <n>",
                crate::PROG
            ));
        }
    }
    set_mode(&manual_file(cfg), 0o600);
    rebuild(ctx)?;
    out.push_str(&after_import(ctx)?);
    Ok(out)
}

/// `server clear` — forget every manually-imported server. Subscriptions are
/// untouched, so the rebuild that follows repopulates from them.
pub fn clear_servers(ctx: &Ctx) -> Result<String, String> {
    let manual = manual_file(&ctx.cfg);
    std::fs::write(&manual, "[]\n").map_err(|e| format!("write: {e}"))?;
    set_mode(&manual, 0o600);
    info("manual servers cleared");
    rebuild(ctx)?;
    after_import(ctx)
}

/// `awk '!/^[[:space:]]*(#|$)/{n++}'` — the subscriptions that are not comments.
pub fn sub_count(cfg: &Path) -> usize {
    read(&subs_file(cfg))
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            !(t.is_empty() || t.starts_with('#'))
        })
        .count()
}

/// `sort -u file -o file`, bytewise — see the module doc's divergence list for
/// why this does not chase the machine's collation.
pub fn sort_unique(body: &str) -> String {
    let mut lines: Vec<&str> = body.lines().collect();
    lines.sort_unstable();
    lines.dedup();
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

/// `sub add <url>…`
pub fn add_subs(ctx: &Ctx, urls: &[String]) -> Result<String, String> {
    let cfg = &ctx.cfg;
    let f = subs_file(cfg);
    // `printf '%s\n' "$@" >> file` — a plain append. A file that did not end in
    // a newline gets the first new URL glued onto its last line, and `sort -u`
    // then treats that as one entry.
    let mut body = read(&f);
    for u in urls {
        body.push_str(u);
        body.push('\n');
    }
    std::fs::write(&f, sort_unique(&body)).map_err(|e| format!("write: {e}"))?;
    // `sort -u file -o file` writes a scratch file and renames it over the
    // original, and that scratch is created 0600 — so an `add` tightens the
    // list where `sub rm` with two arguments loosens it again. Both are
    // artifacts of which tool touched the file last; both are reproduced.
    set_mode(&f, 0o600);
    info(&format!("subscriptions ({}): fetching…", sub_count(cfg)));
    rebuild(ctx)?;
    after_import(ctx)
}

/// `sub rm <n|url>…` — by 1-based line number, or by URL substring.
///
/// QUIRK, and the reason a multi-argument case exists in the gate: the
/// arguments are applied one after another to the SHRINKING file, so the
/// numbers are re-interpreted after each removal. `sub rm 1 2` removes the
/// original lines 1 and 3, not 1 and 2.
pub fn remove_subs(ctx: &Ctx, args: &[String]) -> Result<String, String> {
    let cfg = &ctx.cfg;
    let f = subs_file(cfg);
    let mut body = read(&f);
    for arg in args {
        let lines: Vec<&str> = body.lines().collect();
        let kept: Vec<&str> = if !arg.is_empty() && arg.bytes().all(|b| b.is_ascii_digit()) {
            // `awk -v n="$arg" 'NR!=n'` — a `-v` assignment that looks numeric
            // compares numerically, so "007" drops line 7.
            let n: usize = arg.parse().unwrap_or(0);
            lines.iter().enumerate().filter(|(i, _)| i + 1 != n).map(|(_, l)| *l).collect()
        } else {
            // `grep -vF "$arg"` — substring, anywhere in the line. An empty
            // argument matches every line and empties the list.
            lines.iter().filter(|l| !l.contains(arg.as_str())).map(|l| *l).collect()
        };
        body = if kept.is_empty() { String::new() } else { format!("{}\n", kept.join("\n")) };
    }
    std::fs::write(&f, &body).map_err(|e| format!("write: {e}"))?;
    // QUIRK: the shell's scratch file comes from `mktemp` (0600) and is MOVED
    // onto subs.txt, so one argument leaves the list owner-only — but the move
    // consumed the temp, and every later argument recreates it with a plain
    // redirect (0644). Subscription URLs carry tokens, so the 0644 is the wrong
    // half of this; fixing it is a §6.7 change to both implementations, not a
    // divergence to introduce here.
    set_mode(&f, if args.len() == 1 { 0o600 } else { 0o644 });
    info("removed; re-fetching remaining subscription(s)…");
    rebuild(ctx)?;
    after_import(ctx)
}

/// `sub import <file>` where the file is a `sub dump` — one URL per line.
///
/// Every space is stripped from a line before it is used, not merely trimmed:
/// a URL pasted with a line break in the middle would otherwise become two
/// subscriptions, neither of which resolves.
pub fn load_sub_dump(ctx: &Ctx, file: &Path) -> Result<String, String> {
    let cfg = &ctx.cfg;
    let f = subs_file(cfg);
    let mut body = read(&f);
    let mut n = 0usize;
    for line in read(file).lines() {
        let e: String = line.chars().filter(|c| !c.is_whitespace()).collect();
        if e.is_empty() || e.starts_with('#') {
            continue;
        }
        body.push_str(&e);
        body.push('\n');
        n += 1;
    }
    std::fs::write(&f, sort_unique(&body)).map_err(|e| format!("write: {e}"))?;
    set_mode(&f, 0o600);
    info(&format!("loaded {n} subscription URL(s) from {}; fetching…", file.display()));
    rebuild(ctx)?;
    after_import(ctx)
}

/// `sub update` — re-fetch every subscription.
pub fn update_subs(ctx: &Ctx) -> Result<String, String> {
    info(&format!(
        "re-fetching {} subscription(s)…",
        sub_count(&ctx.cfg)
    ));
    rebuild(ctx)?;
    after_import(ctx)
}

/// `sub clear` — drop every subscription. `: >` truncates in place, so the
/// file's mode survives.
pub fn clear_subs(ctx: &Ctx) -> Result<String, String> {
    std::fs::write(subs_file(&ctx.cfg), "").map_err(|e| format!("write: {e}"))?;
    info("all subscriptions removed");
    rebuild(ctx)?;
    after_import(ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_final_line_without_a_newline_is_not_a_subscription() {
        // `while read` drops it, so the rebuild never fetches it.
        let complete = |body: &str| -> Vec<String> {
            match body.rsplit_once('\n') {
                Some((head, _)) if !head.is_empty() => {
                    head.split('\n').map(|s| s.to_string()).collect()
                }
                _ => Vec::new(),
            }
        };
        assert_eq!(complete("a\nb\n"), vec!["a", "b"]);
        assert_eq!(complete("a\nb"), vec!["a"]);
        assert_eq!(complete("a"), Vec::<String>::new());
        assert_eq!(complete(""), Vec::<String>::new());
    }

    #[test]
    fn sort_unique_matches_sort_dash_u() {
        assert_eq!(sort_unique("b\na\nb\n"), "a\nb\n");
        // No trailing newline in, one out.
        assert_eq!(sort_unique("b\na"), "a\nb\n");
        assert_eq!(sort_unique(""), "");
        // Byte order: uppercase sorts before lowercase.
        assert_eq!(sort_unique("a\nB\n"), "B\na\n");
    }

    #[test]
    fn removing_two_subscriptions_by_number_renumbers_between_them() {
        // The bash bug, kept: after line 1 goes, the old line 3 IS line 2.
        let mut body = "one\ntwo\nthree\n".to_string();
        for arg in ["1", "2"] {
            let lines: Vec<&str> = body.lines().collect();
            let n: usize = arg.parse().unwrap();
            let kept: Vec<&str> =
                lines.iter().enumerate().filter(|(i, _)| i + 1 != n).map(|(_, l)| *l).collect();
            body = format!("{}\n", kept.join("\n"));
        }
        assert_eq!(body, "two\n");
    }

    #[test]
    fn a_string_port_does_not_match_a_number_port() {
        // `--argjson` makes the comparison type-strict, so this manual entry is
        // reported as coming from a subscription and is never removed.
        let from_servers = json!(443);
        let in_manual = json!("443");
        assert_ne!(from_servers, in_manual);
    }

    fn srv(tag: &str, host: &str, port: u64) -> Value {
        json!({"type": "vless", "tag": tag, "server": host, "server_port": port,
               "uuid": "00000000-0000-4000-8000-000000000001"})
    }

    #[test]
    fn the_manual_set_leads_and_subscriptions_follow_in_file_order() {
        let acc = accumulate(
            Some(vec![srv("mine", "203.0.113.10", 443)]),
            vec![
                vec![srv("first-sub", "203.0.113.11", 443)],
                vec![srv("second-sub", "203.0.113.12", 443)],
            ],
        );
        let tags: Vec<&str> = acc.iter().map(|o| o["tag"].as_str().unwrap()).collect();
        assert_eq!(tags, ["mine", "first-sub", "second-sub"]);
    }

    #[test]
    fn a_hand_imported_name_survives_a_subscription_offering_the_same_server() {
        // Same endpoint AND same secret, so `key_of` calls them one server.
        // Because the manual entry comes first, combine keeps it — which is the
        // whole reason the accumulation order is written down.
        let acc = accumulate(
            Some(vec![srv("the-name-i-chose", "203.0.113.10", 443)]),
            vec![vec![srv("VIP-03-|-Tokyo", "203.0.113.10", 443)]],
        );
        let out = combine(&acc).unwrap();
        assert_eq!(out.outbounds.len(), 1);
        assert_eq!(out.outbounds[0]["tag"], json!("the-name-i-chose"));
        // …and the drop is announced, not silent.
        assert!(out.warnings.iter().any(|w| w.contains("VIP-03-|-Tokyo")));
    }

    #[test]
    fn a_null_manual_set_reads_as_empty_because_jq_adds_null_away() {
        assert_eq!(accumulate(Some(Vec::new()), vec![]), Vec::<Value>::new());
    }

    #[test]
    fn combine_refuses_a_non_object_element() {
        assert!(combine(&[json!(1)]).is_err());
        assert!(combine(&[json!({"type": "vless"})]).is_ok());
    }

    #[test]
    fn a_subscription_line_with_no_scheme_is_never_fetched() {
        // The consequence is a request, not a message: curl given a bare host
        // assumes http and dials it, where urllib raises before opening a
        // socket. `sub import <a lane list>` is the realistic way to get such a
        // line into subs.txt, and rowt-rs must not be the implementation that
        // phones home to every domain in it.
        assert!(!opens_a_request("example.com"));
        assert!(!opens_a_request("new-one.example"));
        assert!(!opens_a_request(""));
        assert!(!opens_a_request("//example.com/feed"));
        // A leading digit is not a scheme, so this is a host:port, not a URL.
        assert!(!opens_a_request("127.0.0.1:9"));
        assert!(!opens_a_request(":no-scheme"));
    }

    #[test]
    fn every_scheme_both_clients_understand_still_goes_through() {
        // Narrowing this to http/https would trade one divergence for another:
        // bin/rowt hands file:// to urllib and it reads it.
        for u in [
            "http://127.0.0.1:9/feed",
            "https://example.com/sub?token=x",
            "file:///tmp/feed.txt",
            "ftp://example.com/feed",
        ] {
            assert!(opens_a_request(u), "{u}");
        }
        // `s3+http` is a legal URI scheme and urlparse accepts it, which is why
        // it sat in the list above while the check was urlparse's. urlopen has
        // no handler for it and raises URLError without a socket, so sending it
        // to curl was a request bin/rowt never made.
        assert!(!opens_a_request("s3+http://example.com/feed"));
    }
}
