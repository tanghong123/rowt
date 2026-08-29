//! `rowt skill` and the rc-file surgery `uninstall` needs.
//!
//! Both edit things outside the config directory — symlinks under `~/.claude`
//! and `~/.agents`, and the user's shell rc files. `parity cli-diff` compares
//! stdout, the argv trace and the audit log, and NONE of those see a symlink
//! that was or wasn't created, or a line removed from `~/.zshrc`. So the
//! interesting assertions are unit tests over a temp HOME, right here.

use std::path::{Path, PathBuf};

/// `_skill_src` — the installed copy by default. Under a brew Cellar path the
/// versioned directory changes on every upgrade, so the STABLE `opt` prefix is
/// preferred: a link into the Cellar would dangle the next time brew upgrades.
pub fn skill_src(here: &Path) -> PathBuf {
    let src = here.join("skills/rowt");
    if here.to_string_lossy().contains("/Cellar/rowt/") {
        let opt = std::process::Command::new("brew").args(["--prefix", "rowt"])
            .stderr(std::process::Stdio::null()).output().ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        if !opt.is_empty() {
            let p = PathBuf::from(opt).join("libexec/skills/rowt");
            if p.is_dir() {
                return p;
            }
        }
    }
    src
}

/// Claude Code always; `~/.agents/skills` only when it already exists — rowt
/// does not invent a directory for a tool that is not installed.
///
/// With `store`, ONLY the shared store: a skill manager (knack) links each agent
/// directory at the store itself, so writing `~/.claude/skills/rowt` here would
/// fight it for the same path — knack points it at the store, rowt at the source.
pub fn skill_targets_scoped(home: &Path, store: bool) -> Vec<PathBuf> {
    if store {
        return vec![home.join(".agents/skills/rowt")];
    }
    let mut t = vec![home.join(".claude/skills/rowt")];
    if home.join(".agents/skills").is_dir() {
        t.push(home.join(".agents/skills/rowt"));
    }
    t
}

/// A link rowt created always ends in `/skills/rowt`, so `uninstall` can never
/// delete a user's own unrelated directory that happens to be called rowt.
pub fn skill_ours(p: &Path) -> bool {
    std::fs::read_link(p)
        .map(|t| t.to_string_lossy().ends_with("/skills/rowt"))
        .unwrap_or(false)
}

/// The awk that `uninstall` runs over each rc file: drop the marked block, and
/// drop any bare `rowt shell-init` line (the form the docs recommend adding by
/// hand). Returns None when the file has nothing to strip, so an untouched rc
/// is not rewritten — rewriting would change its mtime and, on a failure
/// mid-write, could truncate a file rowt does not own.
pub fn strip_shell_init(body: &str) -> Option<String> {
    if !(body.contains("rowt shell integration") || body.contains("rowt shell-init")) {
        return None;
    }
    let mut out = Vec::new();
    let mut skip = false;
    for line in body.lines() {
        if line.contains("# >>> rowt shell integration >>>") {
            skip = true;
        }
        if skip {
            if line.contains("# <<< rowt shell integration <<<") {
                skip = false;
            }
            continue;
        }
        if line.contains("rowt shell-init") {
            continue;
        }
        out.push(line);
    }
    Some(out.join("\n") + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_marked_block_goes_and_the_rest_stays() {
        let rc = "export PATH=/x\n\
                  # >>> rowt shell integration >>>\n\
                  alias rowt-proxy-on='eval \"$(rowt proxy env)\"'\n\
                  # <<< rowt shell integration <<<\n\
                  export EDITOR=vim\n";
        let got = strip_shell_init(rc).unwrap();
        assert_eq!(got, "export PATH=/x\nexport EDITOR=vim\n");
    }

    #[test]
    fn a_bare_eval_line_goes_too() {
        // The form README recommends adding by hand, with no marker around it.
        let rc = "export PATH=/x\neval \"$(rowt shell-init)\"\nexport EDITOR=vim\n";
        assert_eq!(strip_shell_init(rc).unwrap(), "export PATH=/x\nexport EDITOR=vim\n");
    }

    #[test]
    fn an_rc_with_nothing_of_ours_is_left_alone() {
        // None, not Some(unchanged): an untouched file must not be rewritten.
        assert!(strip_shell_init("export PATH=/x\n").is_none());
    }

    #[test]
    fn store_scope_touches_only_the_shared_store() {
        // The whole point of --store: a skill manager owns the per-agent links,
        // so rowt must not write ~/.claude/skills/rowt and fight it for the path.
        let d = std::env::temp_dir().join(format!("rowt-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join(".claude/skills")).unwrap();
        std::fs::create_dir_all(d.join(".agents/skills")).unwrap();

        let store = skill_targets_scoped(&d, true);
        assert_eq!(store, vec![d.join(".agents/skills/rowt")]);

        // Without the flag, both — unchanged behaviour for an unmanaged machine.
        let plain = skill_targets_scoped(&d, false);
        assert_eq!(plain, vec![d.join(".claude/skills/rowt"), d.join(".agents/skills/rowt")]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn store_scope_does_not_depend_on_the_store_existing() {
        // `install --store` creates ~/.agents/skills; the target list must name
        // it either way, or the flag would silently no-op on a fresh machine.
        let d = std::env::temp_dir().join(format!("rowt-store-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        assert_eq!(skill_targets_scoped(&d, true), vec![d.join(".agents/skills/rowt")]);
        // …while plain install still never invents it: Claude Code only.
        assert_eq!(skill_targets_scoped(&d, false), vec![d.join(".claude/skills/rowt")]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn only_links_into_a_skills_rowt_are_ours() {
        let d = std::env::temp_dir().join(format!("rowt-skill-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        let ours = d.join("a");
        let theirs = d.join("b");
        let _ = std::fs::remove_file(&ours);
        let _ = std::fs::remove_file(&theirs);
        std::os::unix::fs::symlink("/opt/rowt/skills/rowt", &ours).unwrap();
        std::os::unix::fs::symlink("/home/me/my-rowt-notes", &theirs).unwrap();
        assert!(skill_ours(&ours));
        assert!(!skill_ours(&theirs), "uninstall must never delete someone else's dir");
        let _ = std::fs::remove_dir_all(&d);
    }
}
