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
    let body = out("limactl", &["shell", VM_NAME, "ip", "-4", "-o", "addr", "show"]);
    for l in body.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() >= 4 && f[1].starts_with("lima") && f[1][4..].starts_with(|c: char| c.is_ascii_digit()) {
            return f[3].split('/').next().unwrap_or("").to_string();
        }
    }
    String::new()
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
        let needle = format!("{}.img\"", fetch::larch());
        let mut outp = String::new();
        for line in body.lines() {
            if line.contains("location: \"https") && line.contains(&needle) {
                let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
                outp.push_str(&format!("{indent}location: \"{}\"\n", img.display()));
            } else {
                outp.push_str(line);
                outp.push('\n');
            }
        }
        body = outp;
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
