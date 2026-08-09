//! `rowt fetch` — pre-download everything `up` needs, while a working VPN is on.
//!
//! The point of the command is that afterwards `up` never touches GitHub: you
//! run it with Shadowrocket on, then switch to the corp VPN and rowt still comes
//! up. So every helper here is "already cached? say so and stop" first.
//!
//! Two deliberate non-features. Nothing is fetched through a mirror proxy: this
//! binary handles your traffic, so it comes from the real source or from a
//! source you named yourself (SINGBOX_URL / SINGBOX_TARBALL). And a failed
//! download always removes the partial file — a truncated .srs that looks
//! cached is worse than one that is missing, because the next render would
//! silently emit a rule-set sing-box cannot read.

use crate::{env_or, PROG};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The linux arch tag both the cloud image and the guest tarball use.
pub fn larch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" | "arm64" => "arm64",
        _ => "amd64",
    }
}

/// macOS arch tag for the host sing-box release.
fn darch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" | "arm64" => "arm64",
        _ => "amd64",
    }
}

/// `curl -fL -# --connect-timeout 12 --retry 1` — the shell's exact flags, so
/// the progress bar and the retry behavior are what people already expect.
fn download(url: &str, dest: &Path) -> bool {
    Command::new("curl")
        .args(["-fL", "-#", "--connect-timeout", "12", "--retry", "1", url, "-o"])
        .arg(dest)
        .status().map(|s| s.success()).unwrap_or(false)
}

fn cache(cfg: &Path) -> PathBuf {
    cfg.join("cache")
}

/// `ver_ge` over sing-box's own `version` output: present, and >= 1.12 for
/// AnyTLS and the current config schema.
pub fn sb_ok(p: &Path) -> bool {
    if !p.is_file() {
        return false;
    }
    let v = Command::new(p).arg("version").stderr(Stdio::null()).output().ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    let v = v.lines().next().unwrap_or("").split_whitespace().nth(2).unwrap_or("");
    ver_ge(v, "1.12.0")
}

/// `sort -V | tail -1` — a numeric-component compare, so 1.13.14 beats 1.12.0
/// and 1.9.0 does not beat 1.12.0 the way a byte compare would.
pub fn ver_ge(a: &str, b: &str) -> bool {
    let parts = |s: &str| -> Vec<u64> {
        s.split(|c: char| !c.is_ascii_digit())
            .filter(|x| !x.is_empty())
            .filter_map(|x| x.parse().ok())
            .collect()
    };
    let (x, y) = (parts(a), parts(b));
    if x.is_empty() {
        return false;
    }
    for i in 0..x.len().max(y.len()) {
        let (l, r) = (x.get(i).copied().unwrap_or(0), y.get(i).copied().unwrap_or(0));
        if l != r {
            return l > r;
        }
    }
    true
}

/// Ensure a usable sing-box at `$CFG/bin/sing-box`, in the shell's order:
/// an existing one, a pre-downloaded tarball, a system install, then GitHub.
pub fn ensure_singbox(cfg: &Path) -> Result<(), String> {
    let sb = cfg.join("bin/sing-box");
    if sb_ok(&sb) {
        return Ok(());
    }
    let ver = env_or("SINGBOX_VERSION", "1.13.14");
    let dir = format!("sing-box-{ver}-darwin-{}", darch());
    let tgz = format!("{dir}.tar.gz");
    let tmp = std::env::temp_dir().join(format!("rowt-sb-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let _ = std::fs::create_dir_all(cfg.join("bin"));

    let local = std::env::var("SINGBOX_TARBALL").unwrap_or_default();
    if !local.is_empty() && Path::new(&local).is_file() {
        eprintln!("==> using SINGBOX_TARBALL={local}");
        std::fs::copy(&local, tmp.join(&tgz)).map_err(|e| e.to_string())?;
    } else if let Some(sys) = which("sing-box").filter(|p| sb_ok(p)) {
        let _ = std::fs::remove_file(&sb);
        std::os::unix::fs::symlink(&sys, &sb).map_err(|e| e.to_string())?;
        let v = Command::new(&sb).arg("version").output().ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        let v = v.lines().next().unwrap_or("").split_whitespace().nth(2).unwrap_or("");
        eprintln!("==> using system sing-box {v} ({})", sys.display());
        let _ = std::fs::remove_dir_all(&tmp);
        return Ok(());
    } else {
        eprintln!("==> fetching sing-box {ver} (needs internet — e.g. Shadowrocket on)");
        let mut got = false;
        let custom = std::env::var("SINGBOX_URL").unwrap_or_default();
        let gh = format!("https://github.com/SagerNet/sing-box/releases/download/v{ver}/{tgz}");
        for b in [custom.as_str(), gh.as_str()] {
            if b.is_empty() {
                continue;
            }
            eprintln!("==>   ↓ {b}");
            if download(b, &tmp.join(&tgz)) {
                got = true;
                break;
            }
            eprintln!();
        }
        if !got {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(format!("could not fetch sing-box — turn a VPN on, then '{PROG} fetch host'"));
        }
    }
    let ok = Command::new("tar").arg("xzf").arg(tmp.join(&tgz)).arg("-C").arg(&tmp)
        .status().map(|s| s.success()).unwrap_or(false);
    if !ok {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err("could not unpack the sing-box tarball".into());
    }
    std::fs::copy(tmp.join(&dir).join("sing-box"), &sb)
        .map_err(|e| format!("install sing-box: {e}"))?;
    let _ = crate::set_mode(&sb, 0o755);
    let _ = std::fs::remove_dir_all(&tmp);
    if !sb_ok(&sb) {
        return Err(format!("installed sing-box at {} is not usable", sb.display()));
    }
    Ok(())
}

fn which(cmd: &str) -> Option<PathBuf> {
    std::env::var("PATH").ok()?.split(':')
        .map(|d| Path::new(d).join(cmd))
        .find(|c| c.is_file())
}

/// The ad/tracker rule-set. Best-effort unless `required`: rendering works
/// without it (the hand list still blocks), it just misses the big set.
pub fn ads_ruleset(cfg: &Path, required: bool) -> Result<(), String> {
    let dst = cache(cfg).join("geosite-category-ads-all.srs");
    if dst.is_file() {
        eprintln!("==> ad-block rule-set already cached: {}", dst.display());
        return Ok(());
    }
    let _ = std::fs::create_dir_all(cache(cfg));
    let url = env_or("ROWT_ADS_SRS_URL",
        "https://github.com/SagerNet/sing-geosite/raw/rule-set/geosite-category-ads-all.srs");
    eprintln!("==> fetching geosite ad/tracker rule-set for the block lane");
    eprintln!("==>   ↓ {url}");
    if download(&url, &dst) {
        eprintln!("==> ad-block rule-set cached: {}", dst.display());
        return Ok(());
    }
    let _ = std::fs::remove_file(&dst);
    if required {
        return Err(format!("could not fetch the ad-block rule-set — turn a VPN on, then '{PROG} fetch host'"));
    }
    eprintln!("error: could not fetch the ad-block rule-set (skipping — the hand list still blocks; retry with '{PROG} fetch host' when online)");
    Ok(())
}

/// One `geosite:<name>` rule-set. Best-effort by design — a typo'd category
/// must not halt the whole fetch, and render skips anything uncached.
pub fn geosite(cfg: &Path, name: &str) {
    let dst = cache(cfg).join(format!("geosite-{name}.srs"));
    if dst.is_file() {
        eprintln!("==> geosite:{name} already cached");
        return;
    }
    let _ = std::fs::create_dir_all(cache(cfg));
    let base = env_or("ROWT_GEOSITE_BASE",
        "https://github.com/SagerNet/sing-geosite/raw/rule-set");
    let url = format!("{base}/geosite-{name}.srs");
    eprintln!("==> fetching geosite:{name} rule-set");
    eprintln!("==>   ↓ {url}");
    if download(&url, &dst) {
        eprintln!("==> geosite:{name} cached: {}", dst.display());
    } else {
        let _ = std::fs::remove_file(&dst);
        eprintln!("error: could not fetch geosite:{name} — check the category name is a real sing-geosite tag, then '{PROG} fetch host' when a VPN is on");
    }
}

/// Every `geosite:` category named across the lane files.
pub fn all_geosites(cfg: &Path) {
    use rowt_core::render::geosites_of;
    let esc = crate::read(&cfg.join("escape-domains.txt"));
    let blk = crate::read(&cfg.join("block-domains.txt"));
    let mut all: Vec<String> = geosites_of(&esc);
    all.extend(geosites_of(&blk));
    all.sort();
    all.dedup();
    for n in all {
        geosite(cfg, &n);
    }
}

/// The VM's two large artifacts, cached on the HOST so Lima never has to reach
/// the internet over its bare LAN.
pub fn vm_artifacts(cfg: &Path) -> Result<(), String> {
    let ver = env_or("SINGBOX_VERSION", "1.13.14");
    let img = cache(cfg).join(format!("ubuntu-24.04-server-cloudimg-{}.img", larch()));
    if img.is_file() {
        eprintln!("==> ubuntu image already cached: {}", img.display());
    } else {
        let _ = std::fs::create_dir_all(cache(cfg));
        let url = std::env::var("UBUNTU_IMG_URL").ok().filter(|u| !u.is_empty())
            .unwrap_or_else(|| format!("https://cloud-images.ubuntu.com/releases/24.04/release/ubuntu-24.04-server-cloudimg-{}.img", larch()));
        eprintln!("==> fetching ubuntu 24.04 image (~600MB, once) for the VM");
        eprintln!("==>   ↓ {url}");
        if !download(&url, &img) {
            let _ = std::fs::remove_file(&img);
            return Err(format!("could not fetch ubuntu image — turn Shadowrocket on, then '{PROG} fetch vm'"));
        }
        eprintln!("==> ubuntu image cached: {}", img.display());
    }

    let tgz = format!("sing-box-{ver}-linux-{}.tar.gz", larch());
    let dst = cache(cfg).join(&tgz);
    if dst.is_file() {
        eprintln!("==> guest sing-box already cached: {}", dst.display());
        return Ok(());
    }
    eprintln!("==> fetching guest sing-box {ver} (linux/{}) for the VM", larch());
    let local = std::env::var("GUEST_SINGBOX_TARBALL").unwrap_or_default();
    let custom = std::env::var("GUEST_SINGBOX_URL").unwrap_or_default();
    let gh = format!("https://github.com/SagerNet/sing-box/releases/download/v{ver}/{tgz}");
    for b in [local.as_str(), custom.as_str(), gh.as_str()] {
        if b.is_empty() {
            continue;
        }
        if Path::new(b).is_file() {
            if std::fs::copy(b, &dst).is_ok() {
                eprintln!("==> guest sing-box cached: {}", dst.display());
                return Ok(());
            }
            continue;
        }
        eprintln!("==>   ↓ {b}");
        if download(b, &dst) {
            eprintln!("==> guest sing-box cached: {}", dst.display());
            return Ok(());
        }
        eprintln!();
    }
    let _ = std::fs::remove_file(&dst);
    Err(format!("could not fetch guest sing-box — turn Shadowrocket on, then '{PROG} fetch vm'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare_is_numeric_not_lexical() {
        // The bug a byte compare would have: "1.9.0" > "1.12.0" as text.
        assert!(ver_ge("1.13.14", "1.12.0"));
        assert!(!ver_ge("1.9.0", "1.12.0"));
        assert!(ver_ge("1.12.0", "1.12.0"));
        assert!(ver_ge("2.0.0", "1.99.99"));
        assert!(!ver_ge("", "1.12.0"), "an unparseable version must never pass the gate");
    }
}
