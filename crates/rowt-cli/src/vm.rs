//! `rowt vm` — the bridged Lima VM escape variant.
//!
//! macOS-only by purpose, and PORTING.md §4.1 says so: its whole job is "run the
//! engine in a Linux guest", which is moot on Linux, where tun mode replaces it.
//! So this module is the one part of the port that is not a step toward
//! portability — it exists so `rowt-rs` needs nothing from bash.
//!
//! Everything here shells out to `limactl`, which means `cli-diff`'s argv trace
//! is the real gate: what this code DOES is choose which limactl invocations to
//! make and in what order.

use crate::lifecycle::{self, Ctx};
use crate::{die, env_or, fetch, PROG};
use rowt_platform::{Mac, Platform};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const VM_NAME: &str = "rowt-vm";

fn out(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd).args(args).stderr(Stdio::null()).output().ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

fn ok(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd).args(args).status().map(|s| s.success()).unwrap_or(false)
}

fn quiet(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd).args(args).stdout(Stdio::null()).stderr(Stdio::null())
        .status().map(|s| s.success()).unwrap_or(false)
}

pub fn vm_running() -> bool {
    out("limactl", &["list", VM_NAME, "--format", "{{.Status}}"]).trim() == "Running"
}

/// Lima must not route its own downloads through rowt's proxy: everything it
/// needs is cached locally and the guest is bridged, so it never uses the host
/// proxy. `env -u http_proxy …` in the shell; here, `env_remove`.
fn limactl_nopx(args: &[&str]) -> bool {
    let mut c = Command::new("limactl");
    for k in ["http_proxy", "https_proxy", "all_proxy", "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"] {
        c.env_remove(k);
    }
    c.args(args).status().map(|s| s.success()).unwrap_or(false)
}

fn ensure_lima(cfg: &Path) {
    if rowt_platform::which("limactl") {
        return;
    }
    eprintln!("==> installing lima + socket_vmnet");
    let _ = ok("brew", &["install", "lima", "socket_vmnet"]);
    if !rowt_platform::which("limactl") {
        die(cfg, "lima install failed");
    }
}

/// Lima requires socket_vmnet to be root-owned along its WHOLE path. Homebrew
/// installs it under a user-owned prefix, which Lima rejects, so it is copied to
/// the Lima-recommended /opt/socket_vmnet. Idempotent: once root-owned, no sudo.
fn ensure_socket_vmnet(cfg: &Path) -> String {
    let secure = "/opt/socket_vmnet/bin/socket_vmnet";
    if Path::new(secure).is_file() {
        use std::os::unix::fs::MetadataExt;
        if std::fs::metadata(secure).map(|m| m.uid() == 0).unwrap_or(false) {
            eprintln!("==> socket_vmnet already root-owned at /opt/socket_vmnet — skipping sudo");
            return secure.into();
        }
    }
    let prefix = out("brew", &["--prefix"]).trim().to_string();
    let cellar = format!("{prefix}/Cellar/socket_vmnet");
    // `ls | sort -V | tail -1` — the newest installed version.
    let mut vers: Vec<String> = std::fs::read_dir(&cellar).map(|rd| rd.flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
    vers.sort_by(|a, b| if fetch::ver_ge(a, b) { std::cmp::Ordering::Greater } else { std::cmp::Ordering::Less });
    let Some(ver) = vers.last() else {
        // Not a brew Cellar install — hand Lima the opt path and let it report.
        return format!("{prefix}/opt/socket_vmnet/bin/socket_vmnet");
    };
    eprintln!("==> installing socket_vmnet to /opt/socket_vmnet (root-owned; Lima requires this; needs admin)");
    let good = ok("sudo", &["mkdir", "-p", "/opt/socket_vmnet"])
        && ok("sudo", &["cp", "-R", &format!("{cellar}/{ver}/."), "/opt/socket_vmnet/"])
        && ok("sudo", &["chown", "-R", "root:wheel", "/opt/socket_vmnet"]);
    if !good {
        die(cfg, "could not install socket_vmnet to /opt/socket_vmnet");
    }
    secure.into()
}

fn networks_setup(cfg: &Path) {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let net = home.join(".lima/_config/networks.yaml");
    // /etc is a symlink to /private/etc on macOS and Lima rejects symlinks in
    // path components, so the real path is used — as varRun does.
    let sudoers = "/private/etc/sudoers.d/lima";
    let svm = ensure_socket_vmnet(cfg);
    if let Some(d) = net.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let body = crate::read(&net);
    // Rewritten when missing, missing our network, or any required field is
    // absent or wrong. The path checks are EXACT quoted matches: a bare
    // substring test false-positives, because "/opt/socket_vmnet/…" is a
    // substring of the old brew path and a stale "/etc/sudoers.d/lima" would
    // satisfy a bare "sudoers:" check.
    let want_svm = format!("socketVMNet: \"{svm}\"");
    let want_sudoers = format!("sudoers: \"{sudoers}\"");
    let mut rewrote = false;
    if !net.is_file()
        || !body.contains("rowt-bridged")
        || !body.contains(&want_svm)
        || !body.contains("varRun:")
        || !body.contains(&want_sudoers)
    {
        eprintln!("==> configuring lima bridged network (socket_vmnet at {svm})");
        let iface = Mac.detect_iface().unwrap_or_else(|| "en0".into());
        let yaml = format!(
            "paths:\n  socketVMNet: \"{svm}\"\n  varRun: \"/private/var/run/lima\"\n  sudoers: \"{sudoers}\"\ngroup: \"everyone\"\nnetworks:\n  rowt-bridged:\n    mode: bridged\n    interface: \"{iface}\"\n");
        let _ = std::fs::write(&net, yaml);
        rewrote = true;
    }
    // The sudoers file encodes the socketVMNet path, so it is regenerated
    // whenever the network config changed; otherwise sudo is skipped entirely.
    if rewrote || !Path::new(sudoers).is_file() {
        eprintln!("==> authorizing socket_vmnet (sudo; writes {sudoers})");
        let rules = out("limactl", &["sudoers"]);
        let mut c = Command::new("sudo");
        c.args(["tee", sudoers]).stdin(Stdio::piped()).stdout(Stdio::null());
        if let Ok(mut ch) = c.spawn() {
            use std::io::Write;
            if let Some(si) = ch.stdin.as_mut() {
                let _ = si.write_all(rules.as_bytes());
            }
            let _ = ch.wait();
        }
    } else {
        eprintln!("==> socket_vmnet already authorized ({sudoers} present) — skipping sudo");
    }
}

/// The bridged address. Lima's added interface is lima0/lima1/… — eth0 is its
/// internal NAT (192.168.x), unreachable from the host, and emphatically not it.
pub fn vm_ip_detect() -> String {
    bridged_ip(&out("limactl", &["shell", VM_NAME, "ip", "-4", "-o", "addr", "show"]))
}

/// The guest's BRIDGED address, out of `ip -4 -o addr show`.
///
/// Split from the call because picking the wrong line is silent and expensive:
/// the guest also has a `lo` at 127.0.0.1 and Lima's own user-mode NIC at
/// 192.168.5.15, and rendering either into the host's escape outbound gives a
/// tunnel that resolves, connects to nothing, and reports itself up. Only
/// `lima<digit>` is the socket_vmnet bridge.
fn bridged_ip(body: &str) -> String {
    for l in body.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() >= 4
            && f[1].starts_with("lima")
            && f[1].as_bytes().get(4).is_some_and(u8::is_ascii_digit)
        {
            return f[3].split('/').next().unwrap_or("").to_string();
        }
    }
    String::new()
}

/// Repoint OUR arch's `location:` at the local file, and only that one.
///
/// `s#location: "https[^"]*<arch>.img"#location: "<img>"#` — a SUBSTRING
/// replacement, which is load-bearing twice over. The template lists an image
/// per architecture, so the arch has to be in the pattern or the arm64 entry
/// gets pointed at an amd64 file. And the line is a YAML list item, `  - `
/// included: rebuilding it from its leading whitespace drops the `- ` and
/// produces a file Lima cannot parse. Only the matched span is replaced;
/// everything to its left is untouched.
fn repoint_image(body: &str, needle: &str, path: &str) -> String {
    const HEAD: &str = "location: \"";
    let mut out = String::new();
    for line in body.lines() {
        let rewritten = line.find("location: \"https").and_then(|i| {
            let after = &line[i + HEAD.len()..];
            let q = after.find('"')?;
            // `[^"]*<arch>.img"` — the span must END with our arch's file.
            after[..q + 1].ends_with(needle)
                .then(|| format!("{}{HEAD}{path}\"{}", &line[..i], &after[q + 1..]))
        });
        out.push_str(rewritten.as_deref().unwrap_or(line));
        out.push('\n');
    }
    out
}

/// The repo's Lima template with our arch's image location repointed at the
/// pre-fetched local file, so `limactl start` never downloads ~600MB while the
/// working VPN is off.
fn render_vm_yaml(ctx: &Ctx, here: &Path) -> Result<(), String> {
    let img = ctx.cfg.join("cache").join(format!("ubuntu-24.04-server-cloudimg-{}.img", fetch::larch()));
    if !img.is_file() {
        fetch::vm_artifacts(&ctx.cfg)?;
    }
    let tmpl = here.join("lima/rowt-vm.yaml");
    let mut body = crate::read(&tmpl);
    if body.is_empty() {
        return Err(format!("no Lima template at {}", tmpl.display()));
    }
    if img.is_file() {
        body = repoint_image(&body, &format!("{}.img\"", fetch::larch()), &img.display().to_string());
        eprintln!("==> using cached ubuntu image: {}", img.display());
    }
    std::fs::write(ctx.cfg.join("rowt-vm.yaml"), body).map_err(|e| e.to_string())
}

/// Install sing-box into the running guest from the HOST cache — the VM never
/// reaches GitHub itself over its bare LAN.
fn ensure_guest_singbox(ctx: &Ctx) -> Result<(), String> {
    let ver = env_or("SINGBOX_VERSION", "1.13.14");
    if quiet("limactl", &["shell", VM_NAME, "sh", "-c",
                          &format!("sing-box version 2>/dev/null | grep -q '{ver}'")]) {
        eprintln!("==> guest sing-box {ver} already installed");
        return Ok(());
    }
    let tgz = ctx.cfg.join("cache").join(format!("sing-box-{ver}-linux-{}.tar.gz", fetch::larch()));
    if !tgz.is_file() {
        fetch::vm_artifacts(&ctx.cfg)?;
    }
    eprintln!("==> installing sing-box {ver} into the VM (from the host cache)");
    let _ = ok("limactl", &["copy", &tgz.to_string_lossy(), &format!("{VM_NAME}:/tmp/sb.tgz")]);
    if !ok("limactl", &["shell", VM_NAME, "sudo", "sh", "-c",
        "cd /tmp && tar -xzf sb.tgz && install -m755 sing-box-*/sing-box /usr/local/bin/sing-box && systemctl daemon-reload"]) {
        return Err("failed to install sing-box in the VM".into());
    }
    Ok(())
}

pub fn cmd(ctx: &Ctx, here: &Path, action: &str) -> Result<String, String> {
    let cfg = &ctx.cfg;
    match action {
        "up" => {
            // Checked BEFORE the expensive path — brew install, sudo, a 1-2
            // minute VM build — because otherwise all of that happens and only
            // then does render die for want of a server.
            let n = serde_json::from_str::<Vec<serde_json::Value>>(&crate::read(&cfg.join("servers.json")))
                .map(|v| v.len()).unwrap_or(0);
            if n == 0 {
                die(cfg, &format!("no servers — add some first: {PROG} server add '<vless://...>' or {PROG} sub add <url>"));
            }
            ensure_lima(cfg);
            networks_setup(cfg);
            let listed = out("limactl", &["list", "-q"]);
            if !listed.lines().any(|l| l == VM_NAME) {
                eprintln!("==> creating VM '{VM_NAME}' (~1-2 min)");
                render_vm_yaml(ctx, here)?;
                limactl_nopx(&["start", "--tty=false", "--name", VM_NAME,
                               &cfg.join("rowt-vm.yaml").to_string_lossy()]);
            } else if !vm_running() {
                eprintln!("==> starting existing VM '{VM_NAME}'");
                limactl_nopx(&["start", VM_NAME]);
            } else {
                println!("  VM already running");
            }
            let ip = vm_ip_detect();
            if ip.is_empty() {
                die(cfg, "could not read VM bridged IP");
            }
            lifecycle::sset(ctx, "vm_ip", &ip);
            lifecycle::sset(ctx, "mode", "vm");
            eprintln!("==> VM bridged IP: {ip}  (escape proxy on {ip}:{}, clash API on {ip}:{})",
                      ctx.port + 1, ctx.clash_port + 1);
            ensure_guest_singbox(ctx)?;
            // ALWAYS rendered fresh: this vm_ip may have changed on reboot, and
            // the server list or selection may have changed since.
            lifecycle::cmd_render(ctx)?;
            let _ = ok("limactl", &["copy", &ctx.vm_cfg().to_string_lossy(),
                                    &format!("{VM_NAME}:/tmp/vm.json")]);
            let _ = ok("limactl", &["shell", VM_NAME, "sudo", "install", "-m600",
                                    "/tmp/vm.json", "/etc/rowt/vm.json"]);
            let _ = ok("limactl", &["shell", VM_NAME, "sudo", "systemctl", "enable", "--now", "rowt-singbox"]);
            let _ = ok("limactl", &["shell", VM_NAME, "sudo", "systemctl", "restart", "rowt-singbox"]);
            Ok("  ✓ VM tunnel active".into())
        }
        "down" => {
            // Only a RUNNING vm is stopped: `limactl stop` on an already-stopped
            // or absent one prints a scary FATA even though nothing is wrong.
            let msg = if vm_running() {
                if !quiet("limactl", &["stop", VM_NAME]) {
                    let _ = quiet("limactl", &["stop", "-f", VM_NAME]);
                }
                "  VM stopped"
            } else {
                "  VM already stopped"
            };
            // Printed before the limbo check, not returned past it: the check
            // clears a stranded system proxy and says so, and the shell's order
            // is VM-line-then-proxy-line. A returned string would arrive after.
            println!("{msg}");
            lifecycle::ensure_no_limbo(ctx);
            Ok(String::new())
        }
        "restart" => {
            if vm_running()
                && quiet("limactl", &["shell", VM_NAME, "sudo", "systemctl", "restart", "rowt-singbox"])
            {
                Ok("  VM tunnel restarted".into())
            } else {
                cmd(ctx, here, "up")
            }
        }
        "status" => {
            let body = out("limactl", &["list"]);
            let rows: Vec<&str> = body.lines()
                .filter(|l| l.contains("NAME") || l.contains(VM_NAME)).collect();
            Ok(if rows.is_empty() { "  no VM yet".into() } else { rows.join("\n") })
        }
        "log" => {
            if !vm_running() {
                die(cfg, "VM not running");
            }
            use crate::ExecReplace;
            Err(Command::new("limactl")
                .args(["shell", VM_NAME, "sudo", "journalctl", "-u", "rowt-singbox", "-n", "40", "-f"])
                .exec_replace())
        }
        "delete" => {
            let _ = quiet("limactl", &["delete", "-f", VM_NAME]);
            Ok("  VM deleted".into())
        }
        _ => die(cfg, &format!("usage: {PROG} vm up|down|restart|status|log|delete")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Which of the guest's addresses is the one the host tunnels to. Picking
    /// wrong is silent: the render succeeds, sing-box starts, and the escape
    /// lane points at something that answers nothing.
    #[test]
    fn only_the_socket_vmnet_bridge_counts_as_the_vm_ip() {
        // Verbatim `ip -4 -o addr show` from a bridged Lima guest: loopback,
        // Lima's own user-mode NIC, then the socket_vmnet bridge.
        let body = "\
1: lo    inet 127.0.0.1/8 scope host lo\\       valid_lft forever preferred_lft forever
2: lima0    inet 192.168.5.15/24 metric 100 brd 192.168.5.255 scope global dynamic lima0\\       valid_lft 85903sec
3: lima1    inet 192.0.2.50/24 metric 200 brd 192.0.2.255 scope global dynamic lima1\\       valid_lft 3591sec
";
        // lima0 comes first in the file and wins — which is what the shell
        // does too. The bridge is whichever lima<N> the guest lists first.
        assert_eq!(bridged_ip(body), "192.168.5.15");
        // With only the bridge present, that is the answer.
        let one = "3: lima1    inet 192.0.2.50/24 brd 192.0.2.255 scope global dynamic lima1\n";
        assert_eq!(bridged_ip(one), "192.0.2.50");
        // The mask is stripped; the host renders `server`, not a CIDR.
        assert!(!bridged_ip(one).contains('/'));
    }

    /// `lima` alone is not `lima<N>` — an interface literally named "lima", or
    /// "limbo", must not be mistaken for the bridge. The digit check reads one
    /// byte past "lima", and it must not panic when there is nothing there.
    #[test]
    fn an_interface_merely_starting_with_lima_is_not_the_bridge() {
        assert_eq!(bridged_ip("2: lima    inet 10.0.0.1/8 scope global lima\n"), "");
        assert_eq!(bridged_ip("2: limahost    inet 10.0.0.1/8 scope global limahost\n"), "");
        assert_eq!(bridged_ip(""), "");
        // A short line cannot be indexed as if it had four fields.
        assert_eq!(bridged_ip("2: lima1\n"), "");
    }

    /// The Lima template carries one image per architecture. A rewrite that
    /// matched on `location: "https` alone would repoint the WRONG arch — or
    /// both arches at one file — and the VM would fail to boot after a
    /// download the cache was meant to avoid.
    #[test]
    fn only_our_architectures_image_gets_repointed() {
        let tmpl = "\
images:
  - location: \"https://cloud-images.ubuntu.com/releases/24.04/release/ubuntu-24.04-server-cloudimg-arm64.img\"
    arch: \"aarch64\"
  - location: \"https://cloud-images.ubuntu.com/releases/24.04/release/ubuntu-24.04-server-cloudimg-amd64.img\"
    arch: \"x86_64\"
";
        let out = repoint_image(tmpl, "arm64.img\"", "/cache/ubuntu-24.04-server-cloudimg-arm64.img");
        assert!(out.contains("  - location: \"/cache/ubuntu-24.04-server-cloudimg-arm64.img\""),
                "arm64 repointed, with its indent:\n{out}");
        assert!(out.contains("cloudimg-amd64.img\""), "amd64 left alone:\n{out}");
        assert_eq!(out.matches("location:").count(), 2, "no line gained or lost");
        // The list-item indent survives, or the YAML stops parsing.
        assert!(out.lines().any(|l| l.starts_with("  - location: \"/cache/")));
    }

    /// A template with nothing to repoint comes back unchanged apart from a
    /// guaranteed trailing newline — it is written straight to disk.
    #[test]
    fn a_template_without_our_image_is_left_alone() {
        let t = "images:\n  - location: \"https://example.invalid/other.img\"\n";
        assert_eq!(repoint_image(t, "arm64.img\"", "/cache/x.img"), t);
    }
}
