//! Lane membership in the audit log — the shell's `_lane_audit_begin` /
//! `_lane_audit_end`, around the same bracket and the same watchdog step.
//!
//! The why, the format and the diff live in `rowt_core::lanestate`; this is the
//! I/O around it. `cache/lane-state.tsv` is the LAST LOGGED state (0600 — it
//! holds the corp lane). At `begin`, a difference from it happened outside rowt
//! and is logged as such, then the live state becomes the baseline; at `end`,
//! the command's own change is the difference from that persisted state. Diffing
//! against the persisted file rather than a private copy is what keeps a nested
//! rowt — `config import` exec-ing the shell from inside this bracket — from
//! logging one move twice. No state yet is a baseline: recorded, nothing logged.

use rowt_core::lanestate;
use std::path::{Path, PathBuf};

fn state(cfg: &Path) -> PathBuf {
    cfg.join("cache").join("lane-state.tsv")
}

fn live(cfg: &Path) -> String {
    lanestate::snapshot(|f| crate::read(&cfg.join(f)))
}

/// `_lane_state_keep` — write the new baseline, 0600, via a rename.
fn keep(cfg: &Path, snap: &str) {
    let dir = cfg.join("cache");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let tmp = dir.join(format!(".lane-state.{}", std::process::id()));
    if std::fs::write(&tmp, snap).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    if std::fs::rename(&tmp, state(cfg)).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

fn log(cfg: &Path, before: &str, after: &str, prefix: &str) {
    for l in lanestate::diff_lines(before, after, prefix) {
        crate::shell::audit(cfg, &l);
    }
}

/// Before a mutating command or a watchdog tick: what moved outside rowt.
pub fn begin(cfg: &Path) {
    if !cfg.is_dir() {
        return;
    }
    let now = live(cfg);
    if let Ok(prev) = std::fs::read_to_string(state(cfg)) {
        log(cfg, &prev, &now, "outside rowt: ");
    }
    keep(cfg, &now);
}

/// After it: what the command (or the tick) moved. `prefix` names an actor
/// that is not a bracketed command — `watchdog corp sync: `.
pub fn end(cfg: &Path, prefix: &str) {
    if !cfg.is_dir() {
        return;
    }
    let Ok(prev) = std::fs::read_to_string(state(cfg)) else { return };
    let now = live(cfg);
    log(cfg, &prev, &now, prefix);
    keep(cfg, &now);
}
