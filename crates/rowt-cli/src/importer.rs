//! `server import` / `sub import` — bringing servers over from another VPN
//! client.
//!
//! Two steps on purpose, and the shape is the whole design: EXTRACT dumps one
//! client into an editable review file, accumulating rather than replacing, so
//! several clients land in one place; then APPLY merges what survived your
//! edits into the pool. Nothing reaches `servers.json` without passing through
//! a file you had the chance to prune, because the alternative — importing
//! whatever four-year-old client happens to still have on disk — is how a
//! server pool fills with endpoints that no longer answer.
//!
//! The three extractors this drives are `rowt_core::{srio, foreignio}` and
//! `rowt_core::importmerge`, each already held to its Python by its own
//! differential gate. What lives here is what `bin/rowt` had around them: the
//! flag parsing, the detect message, the accumulate summary, and `apply_import`
//! — which is the part that writes, and therefore the part worth gating.

use crate::lifecycle::{read, Ctx};
use crate::pool;
use rowt_core::{foreignio, importmerge, srio};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn info(msg: &str) { eprintln!("==> {msg}"); }

fn review_file(cfg: &Path) -> PathBuf { cfg.join("import-review.json") }

fn set_mode(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode));
}

/// Which supported clients on THIS machine have importable data.
///
/// Time-boxed, because these scan other applications' config directories and
/// one of them walks iCloud Drive, where a dataless file can stall an
/// arbitrarily long time. `onboard` calls this, so it must never be the reason
/// a first run appears to hang. The shell spends a `perl -e alarm` per
/// extractor; here the work goes to a thread and the answer is simply not
/// waited for past the deadline — the thread dies with the process.
fn detect_sources() -> Vec<String> {
    fn boxed<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static, secs: u64) -> Option<T> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(std::time::Duration::from_secs(secs)).ok()
    }
    let mut out = Vec::new();
    // Shadowrocket first, then the other three — the order `bin/rowt` prints
    // them in, which is the order the workflow message suggests running them.
    if boxed(|| srio::detect(&srio::resolve_store(None), None), 6).unwrap_or(false) {
        out.push("shadowrocket".to_string());
    }
    if let Some(found) = boxed(foreignio::detect, 6) {
        out.extend(found);
    }
    out
}

/// The detect → accumulate → edit → apply walkthrough, printed by
/// `server import --detect` and by `onboard`.
pub fn workflow_msg(cfg: &Path) -> String {
    let prog = crate::PROG;
    let file = review_file(cfg).display().to_string();
    let found = detect_sources();
    if found.is_empty() {
        return format!(
            "no supported VPN client with importable data found on this Mac.\n  \
             add servers directly:  {prog} server add '<vless://…>'  |  {prog} sub add '<url>'"
        );
    }
    let mut o = String::from(
        "found these VPN clients — accumulate each into one review file, edit it, then apply:",
    );
    for s in &found {
        o.push_str(&format!("\n  {prog} server import --output {file} --from {s}"));
    }
    o.push_str(&format!(
        "\n  # edit {file} — keep the servers/subscriptions you want (each has a \"_source\"); then:"
    ));
    o.push_str(&format!("\n  {prog} server import --apply --input {file}"));
    o
}

/// The escape lane after an import: the existing file, a marker, then the
/// client's proxy domains — deduped.
///
/// ```text
/// { cat "$DOMAINS"; echo "# --- imported ---"; jq -r '.proxy_domains[]? // empty' "$j"; } \
///   | awk '!/^[[:space:]]*#/ && NF {if(seen[$0]++)next} {print}'
/// ```
///
/// The awk guard is the whole behaviour and it is narrower than "dedupe the
/// file": only lines that are neither comment nor blank are candidates, they
/// are keyed on the WHOLE line so indentation makes two spellings distinct, and
/// the FIRST occurrence is the one kept. Comments and blanks are therefore
/// copied through however often they repeat — which is why applying twice
/// leaves two `# --- imported ---` markers rather than one. That is bash's
/// behaviour, not an oversight here; see §6.7.
fn merge_domains(existing: &str, j: &Value) -> String {
    let mut merged = String::from(existing);
    // `cat` of a file with no trailing newline would run its last line into the
    // marker; the shell's `echo` starts a line of its own regardless.
    if !merged.is_empty() && !merged.ends_with('\n') {
        merged.push('\n');
    }
    merged.push_str("# --- imported ---\n");
    if let Some(list) = j.get("proxy_domains").and_then(Value::as_array) {
        for d in list {
            if let Some(s) = d.as_str() {
                merged.push_str(s);
                merged.push('\n');
            }
        }
    }
    let mut seen: Vec<&str> = Vec::new();
    let mut kept = String::new();
    for line in merged.lines() {
        let t = line.trim_start();
        // awk's `NF` counts whitespace-separated fields, so a line of only
        // spaces has none and is passed through like a blank.
        let comment_or_blank = t.starts_with('#') || line.split_whitespace().next().is_none();
        if !comment_or_blank {
            if seen.contains(&line) {
                continue;
            }
            seen.push(line);
        }
        kept.push_str(line);
        kept.push('\n');
    }
    kept
}

/// `apply_import` — merge an edited review file into the pool.
///
/// Three destinations, and they are deliberately different kinds of merge: the
/// servers are deduped by endpoint against the manual set with the EXISTING
/// entries first (so re-applying a review file is idempotent and your own names
/// win), the subscription URLs are appended and sorted unique, and the client's
/// proxy domains are appended to the escape lane under a marker comment with
/// duplicates dropped.
pub fn apply(ctx: &Ctx, file: &Path) -> Result<String, String> {
    let cfg = &ctx.cfg;
    let j: Value = serde_json::from_str(&read(file)).unwrap_or(Value::Null);

    // `jq -s '.[0] + ((.[1].servers // []) | map(del(._source)))'` — the review
    // file's `_source` tag is an editing aid and never reaches manual.json or
    // sing-box.
    let manual_path = cfg.join("manual.json");
    if !std::fs::metadata(&manual_path).map(|m| m.len() > 0).unwrap_or(false) {
        std::fs::write(&manual_path, "[]\n").map_err(|e| format!("write: {e}"))?;
    }
    let mut all: Vec<Value> = serde_json::from_str(&read(&manual_path)).unwrap_or_default();
    if let Some(servers) = j.get("servers").and_then(Value::as_array) {
        for s in servers {
            let mut s = s.clone();
            if let Some(o) = s.as_object_mut() {
                o.remove("_source");
            }
            all.push(s);
        }
    }
    let combined = rowt_core::sharelink::combine(&all);
    for w in &combined.warnings {
        eprintln!("{w}");
    }
    let n_file = manual_path.with_extension("json.n");
    std::fs::write(&n_file, rowt_core::sharelink::render(&Value::Array(combined.outbounds)))
        .map_err(|e| format!("write: {e}"))?;
    std::fs::rename(&n_file, &manual_path).map_err(|e| format!("mv: {e}"))?;
    set_mode(&manual_path, 0o600);

    // `jq -r '.subscriptions[]?.url // empty' >> subs.txt`, then `sort -u`.
    let subs_path = cfg.join("subs.txt");
    let mut subs = read(&subs_path);
    if let Some(list) = j.get("subscriptions").and_then(Value::as_array) {
        for s in list {
            if let Some(u) = s.get("url").and_then(Value::as_str).filter(|u| !u.is_empty()) {
                subs.push_str(u);
                subs.push('\n');
            }
        }
    }
    std::fs::write(&subs_path, pool::sort_unique(&subs)).map_err(|e| format!("write: {e}"))?;
    // Same `sort -u file -o file` rename that `sub add` goes through.
    set_mode(&subs_path, 0o600);

    let domains_path = cfg.join("escape-domains.txt");
    // A plain `> "$DOMAINS"` redirect, NOT mktemp+mv: the lane file keeps
    // whatever mode it already had, unlike the lane-edit path.
    std::fs::write(&domains_path, merge_domains(&read(&domains_path), &j))
        .map_err(|e| format!("write: {e}"))?;

    info("importing… rebuilding server set (this fetches the subscriptions)");
    pool::rebuild(ctx)?;
    let out = pool::after_import(ctx)?;
    let n = rowt_core::lanes::dump(&read(&domains_path)).len();
    info(&format!("escape-domains now has {n} domains. Next: {} up", crate::PROG));
    Ok(out)
}

/// `server import <file>` where the file is a `server dump` — an array of
/// outbounds, merged into the manual set and deduped, so restoring a backup
/// twice is the same as restoring it once.
pub fn load_dump(ctx: &Ctx, file: &Path) -> Result<String, String> {
    let cfg = &ctx.cfg;
    let v: Value = serde_json::from_str(&read(file)).unwrap_or(Value::Null);
    let Some(arr) = v.as_array() else {
        crate::die(cfg, &format!("not a 'server dump' (expected a JSON array): {}", file.display()));
    };
    let manual_path = cfg.join("manual.json");
    if !std::fs::metadata(&manual_path).map(|m| m.len() > 0).unwrap_or(false) {
        std::fs::write(&manual_path, "[]\n").map_err(|e| format!("write: {e}"))?;
    }
    let mut all: Vec<Value> = serde_json::from_str(&read(&manual_path)).unwrap_or_default();
    all.extend(arr.iter().cloned());
    let combined = rowt_core::sharelink::combine(&all);
    for w in &combined.warnings {
        eprintln!("{w}");
    }
    let n_file = manual_path.with_extension("json.n");
    std::fs::write(&n_file, rowt_core::sharelink::render(&Value::Array(combined.outbounds)))
        .map_err(|e| format!("write: {e}"))?;
    std::fs::rename(&n_file, &manual_path).map_err(|e| format!("mv: {e}"))?;
    set_mode(&manual_path, 0o600);
    info(&format!("loaded {} server(s) from {}", arr.len(), file.display()));
    pool::rebuild(ctx)?;
    pool::after_import(ctx)
}

/// The extract step: read one client and accumulate it into the review file.
fn accumulate(ctx: &Ctx, source: &str, path: Option<&str>, file: &Path) -> Result<String, String> {
    let cfg = &ctx.cfg;
    let (label, add) = match source {
        "shadowrocket" | "sr" => {
            // QUIRK: the shell appends `--path <p>` to whichever dumper it
            // picked, and `sr-import.py` has no such option — it takes
            // `--store` and `--conf`. So `--from shadowrocket --path X` is
            // always an argparse error and always ends here. Reproduced rather
            // than quietly made to work, because making it work would change
            // which file gets read.
            if path.is_some() {
                eprintln!("usage: sr-import.py [-h] [--store STORE] [--conf CONF] [--detect]");
                eprintln!("sr-import.py: error: unrecognized arguments: --path");
                crate::die(cfg, "could not read Shadowrocket data");
            }
            let store = srio::resolve_store(None);
            match srio::extract(store.as_deref(), None) {
                Ok(v) => ("Shadowrocket", v),
                Err(srio::SrErr::Exc(e)) => {
                    eprintln!("Traceback (most recent call last):");
                    eprintln!("{}: {}", e.name, e.msg);
                    crate::die(cfg, "could not read Shadowrocket data");
                }
                Err(srio::SrErr::Xml(p)) => {
                    eprintln!("error: {p} is an XML plist; only the binary format is read");
                    crate::die(cfg, "could not read Shadowrocket data");
                }
            }
        }
        "clash-verge" | "clashverge" | "verge" | "v2box" | "flclash" => {
            let (from, label) = match source {
                "v2box" => ("v2box", "V2Box"),
                "flclash" => ("flclash", "FlClash"),
                _ => ("clash-verge", "Clash Verge"),
            };
            let mut skipped = rowt_core::foreign::Counter::new();
            let extracted = match foreignio::extract(from, path, &mut skipped) {
                Ok(Some(v)) => v,
                // The Python printed its own message and left with 1.
                Ok(None) => crate::die(cfg, &format!("could not read {label} data")),
                Err(e) if e.is_runtime() => {
                    eprintln!("error: {}", e.msg);
                    crate::die(cfg, &format!("could not read {label} data"));
                }
                Err(e) => {
                    eprintln!("Traceback (most recent call last):");
                    eprintln!("{}: {}", e.name, e.msg);
                    crate::die(cfg, &format!("could not read {label} data"));
                }
            };
            // The extract is RAW — no pool dedup here, because import-merge
            // does all of it and doing it twice would drop entries the review
            // file is supposed to show you.
            let review = match rowt_core::foreign::build_review(
                &extracted.0,
                &extracted.1,
                &mut skipped,
                &rowt_core::foreign::existing_keys(&[]),
                &rowt_core::foreign::existing_subs(&[]),
            ) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("Traceback (most recent call last):");
                    eprintln!("{}: {}", e.name, e.msg);
                    crate::die(cfg, &format!("could not read {label} data"));
                }
            };
            for note in &review.notes {
                eprintln!("{note}");
            }
            (label, review.value)
        }
        _ => crate::die(
            cfg,
            &format!("unknown source: {source}  (want: shadowrocket | clash-verge | v2box | flclash)"),
        ),
    };
    let _ = label;

    let Some(add_obj) = add.as_object() else {
        crate::die(cfg, &format!("merge into {} failed", file.display()));
    };
    let acc_in: Option<Value> = serde_json::from_str(&read(file)).ok();
    let pools: Vec<Option<Value>> = [cfg.join("servers.json"), cfg.join("manual.json")]
        .iter()
        .map(|p| serde_json::from_str(&read(p)).ok())
        .collect();
    let sub_texts = vec![read(&cfg.join("subs.txt"))];
    let m = importmerge::merge(acc_in, add_obj, source, &pools, &sub_texts);
    std::fs::write(file, importmerge::render(&m.acc)).map_err(|e| format!("write: {e}"))?;
    eprintln!("{}", m.summary);
    set_mode(file, 0o600);

    let count = |k: &str| m.acc.get(k).and_then(Value::as_array).map(|a| a.len()).unwrap_or(0);
    let mut o = format!(
        "Accumulated into {} — now {} server(s), {} subscription(s):",
        file.display(),
        count("servers"),
        count("subscriptions")
    );
    for s in m.acc.get("servers").and_then(Value::as_array).into_iter().flatten().take(40) {
        let g = |k: &str| {
            s.get(k)
                .map(|v| match v {
                    Value::String(x) => x.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_else(|| "null".into())
        };
        let src = s.get("_source").and_then(Value::as_str).unwrap_or("?");
        o.push_str(&format!(
            "\n  {} {} {} [{src}]",
            crate::pad(&g("tag"), 20),
            crate::pad(&g("type"), 7),
            crate::pad(&format!("{}:{}", g("server"), g("server_port")), 22)
        ));
    }
    o.push_str("\n\nImport from more clients into the same file, or edit it (keep what you want —");
    o.push_str("\neach entry has a \"_source\"; subscription nodes are fetched fresh so you can drop");
    o.push_str("\nservers that come from one), then apply:");
    o.push_str(&format!("\n  {} server import --apply --input {}", crate::PROG, file.display()));
    Ok(o)
}

/// What the flag loop decided: which client to read, which file to read or
/// write, and whether this is the extract half or the apply half.
#[derive(Debug, Default, PartialEq)]
struct Flags {
    source: String,
    path: String,
    apply_it: bool,
    file: String,
}

#[derive(Debug, PartialEq)]
enum FlagErr {
    /// A flag that takes a value was last on the line.
    MissingValue,
    Unknown(String),
}

/// The flag loop, which is a decision and not a formatting detail: it picks the
/// file that gets WRITTEN. Two properties come from the shell rather than from
/// intent, and both are pinned by test — every flag is last-wins (the loop just
/// assigns), and `--output`/`--input` are the SAME variable, so mixing them is
/// legal and the later one takes the file.
fn parse_flags(args: &[String]) -> Result<Flags, FlagErr> {
    let mut f = Flags::default();
    let mut i = 0;
    while i < args.len() {
        let need = |i: &mut usize| -> Result<String, FlagErr> {
            *i += 1;
            args.get(*i).cloned().ok_or(FlagErr::MissingValue)
        };
        match args[i].as_str() {
            "--apply" => f.apply_it = true,
            "--from" => f.source = need(&mut i)?,
            "--output" | "--input" | "-o" | "-i" => f.file = need(&mut i)?,
            "--path" => f.path = need(&mut i)?,
            x if x.starts_with("--from=") => f.source = x["--from=".len()..].to_string(),
            x if x.starts_with("--output=") || x.starts_with("--input=") => {
                f.file = x.split_once('=').map(|(_, v)| v.to_string()).unwrap_or_default()
            }
            x if x.starts_with("--path=") => f.path = x["--path=".len()..].to_string(),
            x => return Err(FlagErr::Unknown(x.to_string())),
        }
        i += 1;
    }
    Ok(f)
}

/// `server import` / `sub import` — the whole arm.
pub fn cmd(ctx: &Ctx, args: &[String]) -> Result<String, String> {
    let cfg = &ctx.cfg;
    // `--detect` only counts as the FIRST argument, the way the shell tests it.
    if args.first().map(|s| s.as_str()) == Some("--detect") {
        return Ok(workflow_msg(cfg));
    }

    let Flags { source, path, apply_it, file } = match parse_flags(args) {
        Ok(f) => f,
        // `${1:?msg}` — a flag whose value is missing ends the shell with 1 and
        // a parameter-expansion complaint, not a usage message.
        Err(FlagErr::MissingValue) => std::process::exit(1),
        Err(FlagErr::Unknown(x)) => crate::die(
            cfg,
            &format!(
                "unknown option: {x}  (use --from <src> [--output <file>], or --apply [--input <file>])"
            ),
        ),
    };
    let target = if file.is_empty() { review_file(cfg) } else { PathBuf::from(&file) };

    if apply_it {
        if !std::fs::metadata(&target).map(|m| m.len() > 0).unwrap_or(false) {
            crate::die(cfg, &format!(
                "no review file at {} — run '{} server import --output {} --from <source>' first",
                target.display(), crate::PROG, target.display()));
        }
        if serde_json::from_str::<Value>(&read(&target)).is_err() {
            crate::die(cfg, &format!("review file is not valid JSON: {}", target.display()));
        }
        return apply(ctx, &target);
    }
    if source.is_empty() {
        crate::die(cfg, &format!(
            "usage: {p} server import --from <src> [--output <file>]   |   --apply [--input <file>]   |   --detect",
            p = crate::PROG));
    }
    accumulate(ctx, &source, Some(path.as_str()).filter(|p| !p.is_empty()), &target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn f(args: &[&str]) -> Result<Flags, FlagErr> {
        parse_flags(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn output_and_input_are_one_variable_and_the_last_one_wins() {
        // The consequence worth having a test for: this decides which file gets
        // WRITTEN. `--output a --input b` is not a contradiction the shell
        // catches, it is two assignments to the same slot.
        assert_eq!(f(&["--output", "a", "--input", "b"]).unwrap().file, "b");
        assert_eq!(f(&["--input", "b", "--output", "a"]).unwrap().file, "a");
        assert_eq!(f(&["-o", "a", "-i", "b"]).unwrap().file, "b");
        // …and so is repeating one flag.
        assert_eq!(f(&["--from", "v2box", "--from", "flclash"]).unwrap().source, "flclash");
    }

    #[test]
    fn the_equals_spelling_reaches_the_same_slot_as_the_spaced_one() {
        for (spaced, joined) in [
            (vec!["--from", "v2box"], "--from=v2box"),
            (vec!["--output", "/tmp/r.json"], "--output=/tmp/r.json"),
            (vec!["--input", "/tmp/r.json"], "--input=/tmp/r.json"),
            (vec!["--path", "/tmp/p"], "--path=/tmp/p"),
        ] {
            assert_eq!(f(&spaced).unwrap(), f(&[joined]).unwrap(), "{joined}");
        }
        // An empty value is a value, not a missing one — `--from=` leaves the
        // source empty, which `cmd` then reports as the usage error rather than
        // as an unknown option.
        assert_eq!(f(&["--from="]).unwrap(), Flags::default());
    }

    #[test]
    fn a_flag_last_on_the_line_is_a_missing_value_not_an_unknown_option() {
        // The shell's `${1:?}` exits 1 silently here; an unknown option prints
        // a message and dies. Different exits, so the gate can tell them apart.
        for flag in ["--from", "--output", "--input", "-o", "-i", "--path"] {
            assert_eq!(f(&[flag]), Err(FlagErr::MissingValue), "{flag}");
        }
        // `--apply` takes no value, so it is fine alone.
        assert!(f(&["--apply"]).unwrap().apply_it);
    }

    #[test]
    fn only_the_documented_flags_are_accepted() {
        assert_eq!(f(&["--nosuchflag"]), Err(FlagErr::Unknown("--nosuchflag".into())));
        // A bare positional is NOT a file argument here: `cmd` is only reached
        // after main.rs has already ruled out the restore-a-dump path, so
        // anything left over is a typo.
        assert_eq!(f(&["backup.json"]), Err(FlagErr::Unknown("backup.json".into())));
        // The value of a flag is never re-examined as a flag itself.
        assert_eq!(f(&["--from", "--nosuchflag"]).unwrap().source, "--nosuchflag");
        // `-o=x` is not a spelling the loop knows — the `=` forms are long-only.
        assert_eq!(f(&["-o=x"]), Err(FlagErr::Unknown("-o=x".into())));
    }

    #[test]
    fn the_marker_is_a_comment_so_re_applying_grows_the_file() {
        // Comments are not deduped, so each apply leaves another marker behind.
        // Bug-for-bug with the awk; recorded so a "tidy-up" that dedupes
        // comments has to argue with a test instead of a diff.
        let j = json!({"proxy_domains": ["a.example"]});
        let once = merge_domains("", &j);
        let twice = merge_domains(&once, &j);
        assert_eq!(once, "# --- imported ---\na.example\n");
        assert_eq!(twice, "# --- imported ---\na.example\n# --- imported ---\n");
        // The DOMAINS themselves stay unique across applies, which is the part
        // that actually matters for the lane.
        assert_eq!(twice.lines().filter(|l| *l == "a.example").count(), 1);
    }

    #[test]
    fn the_first_spelling_of_a_domain_is_the_one_kept() {
        // `seen[$0]++` keys on the whole line, so indentation makes two lines
        // distinct — and the survivor is the FIRST, which means an existing
        // lane entry always outlives an imported duplicate of itself.
        let j = json!({"proxy_domains": ["keep.example", "  keep.example", "keep.example"]});
        assert_eq!(
            merge_domains("keep.example\n", &j),
            "keep.example\n# --- imported ---\n  keep.example\n"
        );
    }

    #[test]
    fn comments_and_blanks_survive_however_often_they_repeat() {
        let j = json!({"proxy_domains": []});
        // Two identical comments, an indented one, a blank, and a whitespace-only
        // line — awk's NF is zero for the last, so it is passed through too.
        let body = "# head\n# head\n   # indented\n\n   \nx.example\nx.example\n";
        assert_eq!(
            merge_domains(body, &j),
            "# head\n# head\n   # indented\n\n   \nx.example\n# --- imported ---\n"
        );
    }

    #[test]
    fn a_lane_file_with_no_trailing_newline_does_not_swallow_the_marker() {
        // `cat` would run the last line straight into the `echo`; the shell's
        // marker always starts its own line, so this must too.
        let j = json!({"proxy_domains": []});
        assert_eq!(merge_domains("a.example", &j), "a.example\n# --- imported ---\n");
        // An empty lane file gets no leading blank line.
        assert_eq!(merge_domains("", &j), "# --- imported ---\n");
    }

    #[test]
    fn proxy_domains_that_are_not_strings_are_skipped_not_stringified() {
        // `jq -r '.proxy_domains[]? // empty'` would render a number, but the
        // review file is hand-edited and a non-string there is a mistake; the
        // lane must not gain a line reading `null` or `123`.
        let j = json!({"proxy_domains": ["ok.example", 123, null, {"a": 1}]});
        assert_eq!(merge_domains("", &j), "# --- imported ---\nok.example\n");
        // A missing key, or a wrong-typed one, leaves the lane alone but for
        // the marker — `[]?` swallows both.
        assert_eq!(merge_domains("", &json!({})), "# --- imported ---\n");
        assert_eq!(merge_domains("", &json!({"proxy_domains": "a.example"})), "# --- imported ---\n");
    }
}
