//! `config import` and `config export --to shadowrocket` — the two `config`
//! paths rowt-rs handed to the shell until #s74.
//!
//! Both keep the shell's shape rather than improving on it, because `cli-diff`
//! compares them with the shell: stdout, exit status, the config tree, the
//! argv trace and the audit log. Where the shell re-invokes itself (one
//! `<lane> import` per lane of a merge, against a staging config) this
//! re-invokes rowt-rs the same way, so the staging audit, the lane logs and the
//! trace come out of the same code path the direct command uses. Where the
//! shell asks on `/dev/tty`, so does this — a process with no controlling
//! terminal cannot open one, and the answer is "no".

use crate::lifecycle::{self, Ctx};
use crate::{die, env_or, read, set_mode, sh_date, PROG, SILENT};
use rowt_core::render::{geosites_of, parse_list, Filter};
use rowt_core::srexport::{self, Geo, GeoSet};
use rowt_core::{hotspot, sharelink};
use std::io::{BufRead, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const LANE_FILES: [(&str, &str); 4] = [
    ("escape", "escape-domains.txt"),
    ("corp", "corp-domains.txt"),
    ("block", "block-domains.txt"),
    ("hotspot", "hotspot-domains.txt"),
];

/// `mktemp -d <dir>/<prefix>XXXXXX`: a fresh 0700 directory under `dir`.
fn mkdtemp(dir: &Path, prefix: &str) -> Option<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64).unwrap_or(0) ^ ((std::process::id() as u64) << 32);
    for _ in 0..64 {
        let mut name = prefix.to_string();
        for _ in 0..6 {
            // xorshift — the names only have to differ, not be unguessable.
            seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17;
            name.push(ALPHABET[(seed % ALPHABET.len() as u64) as usize] as char);
        }
        let p = dir.join(name);
        if std::fs::DirBuilder::new().mode(0o700).create(&p).is_ok() {
            return Some(p);
        }
    }
    None
}

/// A directory removed when dropped — `rm -rf "$t"` on every way out.
struct TmpDir(PathBuf);
impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `printf '…? [y/N] '; read -r ans </dev/tty || ans=""` — the question goes to
/// stdout, the answer comes from the terminal, and no terminal is "no".
fn ask_tty(prompt: &str) -> bool {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut ans = String::new();
    if let Ok(f) = std::fs::File::open("/dev/tty") {
        let _ = std::io::BufReader::new(f).read_line(&mut ans);
    }
    matches!(ans.trim_end_matches(['\n', '\r']), "y" | "Y" | "yes" | "YES")
}

/// `_export_shadowrocket`: the lanes as a Shadowrocket config, the servers as
/// share links in a file of their own. `out` is a path or `-` for stdout.
pub fn export_shadowrocket(ctx: &Ctx, out: &str, routes_only: bool) -> Result<String, String> {
    let cfg = &ctx.cfg;
    if out == "-" && !routes_only {
        die(cfg, "the servers go to a file of their own, so stdout carries the rules only — add --routes-only");
    }
    let tmpbase = env_or("TMPDIR", "/tmp");
    let t = TmpDir(mkdtemp(Path::new(&tmpbase), "rowt-sr.").unwrap_or_else(|| die(cfg, "mktemp failed")));
    let esc = read(&cfg.join("escape-domains.txt"));
    let corp = read(&cfg.join("corp-domains.txt"));
    let blk = read(&cfg.join("block-domains.txt"));
    let hot = read(&cfg.join("hotspot-domains.txt"));
    let cache = cfg.join("cache");
    let sb = ctx.sb();
    // `_sr_geo`: the cached `.srs` decompiled by sing-box into the temp dir, or
    // one of the two reasons it was not. By absolute path, as the shell's `$SB`.
    let decompile = |cat: &str| -> GeoSet {
        let srs = cache.join(format!("geosite-{cat}.srs"));
        if !srs.is_file() {
            return GeoSet::NotCached;
        }
        let j = t.0.join(format!("geo-{cat}.json"));
        let ok = Command::new(&sb).args(["rule-set", "decompile"]).arg(&srs).arg("-o").arg(&j)
            .stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false);
        if !ok {
            return GeoSet::Undecompilable;
        }
        match serde_json::from_str::<serde_json::Value>(&read(&j)) {
            Ok(v) => GeoSet::Rules(v),
            Err(_) => GeoSet::Undecompilable,
        }
    };
    let mut geos: Vec<Geo> = geosites_of(&esc).into_iter()
        .map(|c| Geo { set: decompile(&c), category: c, policy: "PROXY" }).collect();
    geos.extend(geosites_of(&blk).into_iter().map(|c| Geo { set: decompile(&c), category: c, policy: "REJECT" }));
    let ads = cache.join("geosite-category-ads-all.srs").is_file()
        .then(|| Geo { category: "category-ads-all".into(), policy: "REJECT", set: decompile("category-ads-all") });
    let input = srexport::Input {
        prog: PROG,
        hotspot: &parse_list(&hot, Filter::Domain),
        escape_exact: &parse_list(&esc, Filter::Exact),
        corp_exact: &parse_list(&corp, Filter::Exact),
        block_exact: &parse_list(&blk, Filter::Exact),
        escape: &parse_list(&esc, Filter::Domain),
        corp: &parse_list(&corp, Filter::Domain),
        block: &parse_list(&blk, Filter::Domain),
        geos: &geos,
        corp_cidrs: &parse_list(&corp, Filter::Cidr),
        private_default_corp: env_or("ROWT_PRIVATE_DEFAULT", "corp") == "corp",
        ads: ads.as_ref(),
    };
    let rules = srexport::rules(&input);
    let skip = srexport::skip_proxy(&hotspot::bypass_entries(&hot));
    let conf = srexport::config(env!("ROWT_SHELL_VERSION"), &sh_date("+%Y-%m-%d %H:%M:%S"), PROG, &rules, &skip);
    let counts = srexport::counts(&rules);
    if out == "-" {
        print!("{conf}");
    } else {
        if std::fs::write(out, &conf).is_err() {
            die(cfg, &format!("could not write {out}"));
        }
        println!("wrote {out}");
    }
    // The servers: hand-added ones as share links (each one checked by parsing
    // it back — one that no link carries exactly is named on stderr and left
    // out), then the subscription URLs, which Shadowrocket fetches itself. No
    // comment lines: the whole file is meant to be copied into Shadowrocket.
    let mut sfile = String::new();
    let mut counts_s = None;
    if !routes_only {
        sfile = format!("{}-servers.txt", out.strip_suffix(".conf").unwrap_or(out));
        let manual = cfg.join("manual.json");
        let mut links: Vec<String> = Vec::new();
        if std::fs::metadata(&manual).map(|m| m.len() > 0).unwrap_or(false) {
            let doc = sharelink::py_json_loads(&read(&manual))
                .unwrap_or_else(|_| die(cfg, "could not turn the servers into share links"));
            let Some(arr) = doc.as_array() else { die(cfg, "could not turn the servers into share links") };
            for o in arr {
                match sharelink::to_link(o) {
                    Some(l) => links.push(l),
                    None => {
                        let name = o.get("tag").and_then(|v| v.as_str()).unwrap_or("?");
                        eprintln!("warning: '{name}' has settings no share link carries exactly — not exported");
                    }
                }
            }
        }
        let subs: Vec<String> = read(&cfg.join("subs.txt")).lines()
            .filter(|l| { let t = l.trim_start(); !(t.is_empty() || t.starts_with('#')) })
            .map(str::to_string).collect();
        let mut body = String::new();
        for l in links.iter().chain(subs.iter()) {
            body.push_str(l);
            body.push('\n');
        }
        // `( umask 077; cat … > "$sfile" )`, then `chmod 600`.
        use std::os::unix::fs::OpenOptionsExt;
        let written = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600)
            .open(&sfile).and_then(|mut f| f.write_all(body.as_bytes()));
        if written.is_err() {
            die(cfg, &format!("could not write {sfile}"));
        }
        let _ = set_mode(Path::new(&sfile), 0o600);
        println!("wrote {sfile}");
        counts_s = Some((links.len(), subs.len()));
    }
    let servers = counts_s.map(|(l, s)| (l, s, sfile.as_str()));
    for line in srexport::summary(&counts, rules.regex, servers, out == "-") {
        eprintln!("{line}");
    }
    drop(t);
    Ok(String::new())
}

/// `sed -E 's/ \(from .*\)$//'` on a lane import's tally line.
fn strip_from(l: &str) -> &str {
    if l.ends_with(')') {
        if let Some(i) = l.find(" (from ") {
            return &l[..i];
        }
    }
    l
}

/// `config import <file.tgz> [-y] [--replace]`. The default is a MERGE: each
/// lane list of the bundle is folded into this config's through the same
/// `<lane> import` the CLI runs (normalize, dedupe, the single-lane pull),
/// against a staging copy, and the live lanes are swapped only at the very
/// end. `--replace` restores the whole bundle over the config, the
/// move-to-another-Mac path. The server pool is never merged.
pub fn import(ctx: &Ctx, args: &[String]) -> Result<String, String> {
    let cfg = &ctx.cfg;
    let (mut yes, mut replace, mut input) = (false, false, String::new());
    for a in args {
        match a.as_str() {
            "-y" | "--yes" => yes = true,
            "--replace" => replace = true,
            f if f.starts_with('-') => die(cfg, &format!("unknown flag: {f}")),
            f => input = f.to_string(),
        }
    }
    if input.is_empty() {
        die(cfg, &format!("usage: {PROG} config import <file.tgz> [-y] [--replace]"));
    }
    if !Path::new(&input).is_file() {
        die(cfg, &format!("no such file: {input}"));
    }
    let listing = match Command::new("tar").args(["tzf", &input]).stderr(Stdio::null()).output() {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => die(cfg, &format!("{input} is not a readable .tgz")),
    };
    // A bundle is identified by the server pool (servers.json) OR ANY lane's
    // rules — the two things it carries; subs.txt or a secondary pool JSON on
    // its own is not one (a bare URL list is not a bundle).
    let is_bundle = listing.lines().any(|l| matches!(l.rsplit('/').next().unwrap_or(""),
        "servers.json" | "escape-domains.txt" | "corp-domains.txt" | "block-domains.txt" | "hotspot-domains.txt"));
    if !is_bundle {
        die(cfg, &format!("{input} doesn't look like a rowt config bundle"));
    }
    let _ = std::fs::create_dir_all(cfg);
    if replace {
        if !yes && !ask_tty(&format!("import {input} into {} — REPLACES matching config files. Continue? [y/N] ", cfg.display())) {
            println!("aborted.");
            return Err(SILENT.into());
        }
        let ok = Command::new("tar").args(["xzf", &input, "-C"]).arg(cfg)
            .status().map(|s| s.success()).unwrap_or(false);
        if !ok {
            die(cfg, "extract failed");
        }
    } else {
        // Staged UNDER the config dir so each final swap is a same-filesystem
        // rename (atomic); a bare temp dir honours TMPDIR and could land on
        // another volume. Removed on every way out.
        let work = TmpDir(mkdtemp(cfg, ".merge.")
            .unwrap_or_else(|| die(cfg, &format!("cannot create a staging dir under {}", cfg.display()))));
        let xdg = work.0.join("x");
        let stage = xdg.join("rowt");
        let inx = work.0.join("in");
        let _ = std::fs::create_dir_all(&stage);
        let _ = std::fs::create_dir_all(&inx);
        let ok = Command::new("tar").args(["xzf", &input, "-C"]).arg(&inx)
            .status().map(|s| s.success()).unwrap_or(false);
        if !ok {
            drop(work);
            die(cfg, "extract failed");
        }
        for (_, name) in LANE_FILES {
            let lf = cfg.join(name);
            if lf.is_file() {
                let _ = std::fs::copy(&lf, stage.join(name));
            }
        }
        // Fold each lane in through THIS binary, against the staging config —
        // the shell re-invokes its own resolved script the same way. A child
        // that fails aborts the whole merge with its own words; nothing is
        // swapped. ROWT_NO_SYSPROXY keeps the hotspot fold from touching the
        // system proxy: the swap, then the user's restart, is what applies it.
        let me = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("rowt-rs"));
        let mut report = String::new();
        for (lane, name) in LANE_FILES {
            let src = inx.join(name);
            if !src.is_file() {
                continue;
            }
            let o = Command::new(&me).arg(lane).arg("import").arg(&src).arg("--no-reload")
                .env("XDG_CONFIG_HOME", &xdg).env("ROWT_NO_SYSPROXY", "1").output();
            let (ok, text) = match o {
                Ok(o) => {
                    let mut t = String::from_utf8_lossy(&o.stdout).into_owned();
                    t.push_str(&String::from_utf8_lossy(&o.stderr));
                    (o.status.success(), t)
                }
                Err(e) => (false, format!("{e}")),
            };
            let text = text.trim_end_matches('\n').to_string();
            if !ok {
                drop(work);
                die(cfg, &format!("merge failed folding the {lane} lane — nothing changed:\n{text}"));
            }
            report.push_str(&text);
            report.push('\n');
        }
        // What folded in — the per-lane tallies, the staging path stripped.
        for l in report.lines().filter(|l| l.contains("+=")) {
            println!("{}", strip_from(l));
        }
        // A full bundle carries a server pool; merge deliberately leaves it, and
        // says so rather than silently dropping it.
        if ["servers.json", "manual.json", "import-review.json", "subs.txt", "outbound.json",
            "escape-dns.txt", "manual-sources.tsv"].iter().any(|f| inx.join(f).is_file())
        {
            println!("note: the bundle also carries a server pool or its settings — NOT merged (use --replace to take the whole bundle, or 'server import' to fold servers in).");
        }
        // A conflict is an entry the bundle puts in a lane where it already is
        // in a DIFFERENT one; folding it in moves it. All of them at the end,
        // then apply-or-abort the merge as a whole.
        let conflicts: Vec<&str> = report.lines().filter(|l| l.contains("moved out of")).collect();
        if !conflicts.is_empty() {
            println!("merge conflicts — these entries are in a different lane here; the bundle would move them:");
            for c in &conflicts {
                println!("{c}");
            }
            if !yes && !ask_tty("apply the merge? [y/N] ") {
                println!("aborted.");
                drop(work);
                return Err(SILENT.into());
            }
        }
        for (_, name) in LANE_FILES {
            let s = stage.join(name);
            if s.is_file() {
                let _ = std::fs::rename(&s, cfg.join(name));
            }
        }
        drop(work);
    }
    for f in ["servers.json", "manual.json", "import-review.json", "subs.txt", "manual-sources.tsv"] {
        let p = cfg.join(f);
        if p.is_file() {
            let _ = set_mode(&p, 0o600);
        }
    }
    println!("imported{} into {}.", if replace { "" } else { " (lanes merged)" }, cfg.display());
    // `( cmd_render ) >/dev/null 2>&1`: on a fresh box render dies (no servers,
    // no engine yet). Contained in a subshell there, so the command goes on to
    // the fresh-box advice — but the die in that subshell still wrote its
    // ABORT line into the audit log, and so does this.
    match lifecycle::cmd_render(ctx) {
        Ok(_) => println!("  ✓ regenerated host.json"),
        Err(e) => {
            crate::audit_abort(cfg, &e);
            println!("  (couldn't render yet — that's fine on a fresh box: '{PROG} fetch' installs sing-box)");
        }
    }
    println!("next: '{PROG} up'  (fresh machine) — or '{PROG} restart' if rowt is already running.");
    Ok(String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_staging_path_is_stripped_from_a_tally_line_only() {
        assert_eq!(strip_from("  escape += 2 new, 0 already present (from /x/.merge.abc/in/escape-domains.txt)"), "  escape += 2 new, 0 already present");
        assert_eq!(strip_from("  moved out of block lane: ads.example"), "  moved out of block lane: ads.example");
        assert_eq!(strip_from("x (from y) z"), "x (from y) z");
    }

    #[test]
    fn mkdtemp_makes_a_private_directory_under_the_dir_with_the_prefix() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir();
        let d = mkdtemp(&base, "rowt-test.").unwrap();
        assert!(d.is_dir());
        assert!(d.file_name().unwrap().to_string_lossy().starts_with("rowt-test."));
        assert_eq!(std::fs::metadata(&d).unwrap().permissions().mode() & 0o777, 0o700);
        let _ = std::fs::remove_dir(&d);
    }
}
