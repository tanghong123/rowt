//! Rendering, supervising sing-box, and the up/down/reload path.
//!
//! This is the half of rowt that is not a pure function: it writes files, spawns
//! a long-lived process, and waits for it to answer. Every system-proxy write
//! still goes through `rowt_platform`, which honours ROWT_NO_SYSPROXY — so this
//! whole path can be exercised on a machine that is using rowt for real.

use rowt_core::render::{geosites_of, group, parse_list, render_host, render_vm, Filter, Geo, HostInput, Lists};
use rowt_platform::{Mac, Platform};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct Ctx {
    pub cfg: PathBuf,
    pub port: u16,
    pub clash_port: u16,
}

impl Ctx {
    pub fn new(cfg: PathBuf) -> Self {
        let port = env_num("ROWT_PORT", 7890);
        Ctx { cfg, port, clash_port: env_num("ROWT_CLASH_PORT", 9090) }
    }
    pub fn sb(&self) -> PathBuf { self.cfg.join("bin/sing-box") }
    pub fn host_cfg(&self) -> PathBuf { self.cfg.join("host.json") }
    pub fn vm_cfg(&self) -> PathBuf { self.cfg.join("vm.json") }
    pub fn pidfile(&self) -> PathBuf { self.cfg.join("host.pid") }
    pub fn logdir(&self) -> PathBuf { self.cfg.join("log") }
    pub fn host_log(&self) -> PathBuf { self.logdir().join("host.log") }
    pub fn state(&self) -> String { read(&self.cfg.join("state")) }
    pub fn sget(&self, k: &str) -> String { sget(&self.state(), k) }
    pub fn mode(&self) -> String {
        let m = self.sget("mode");
        if m.is_empty() { "host".into() } else { m }
    }
    /// Empty when mode=vm and no vm_ip is known — the shell's `controller`
    /// returns non-zero there, and every caller reads that as "no API".
    pub fn controller(&self) -> String { controller(self).unwrap_or_default() }
}

/// `controller` — where the clash API lives, which in vm mode is the GUEST's.
///
/// The API answers on the machine sing-box runs on: 127.0.0.1 for host and
/// local mode, but the VM's bridged IP on CLASH_PORT+1 when the tunnel lives in
/// the guest. Reading 127.0.0.1 there talks to nothing, which is how `use`,
/// `status` and the collector would all quietly stop working in vm mode.
pub fn controller(ctx: &Ctx) -> Option<String> {
    if ctx.mode() == "vm" {
        let ip = ctx.sget("vm_ip");
        if ip.is_empty() { return None; }
        return Some(format!("{ip}:{}", ctx.clash_port + 1));
    }
    Some(format!("127.0.0.1:{}", ctx.clash_port))
}

/// `clash_secret` — read it, or mint one and remember it.
///
/// Generate-on-first-use, exactly as the shell does. Reading the key without
/// the mint looks equivalent because every config that has ever been rendered
/// already carries one — but on a FRESH config it renders a clash API with an
/// empty secret, which is an unauthenticated control plane that can switch
/// outbounds and list every live connection. No gate can see this: the parity
/// fixtures pin `clash_secret` precisely because minting is nondeterministic
/// (tests/parity/README.md), so both sides read the pinned value and agree.
pub fn clash_secret(ctx: &Ctx) -> String {
    let s = ctx.sget("clash_secret");
    if !s.is_empty() {
        return s;
    }
    // `head -c 24 /dev/urandom | base64 | tr -dc 'A-Za-z0-9' | cut -c1-24`
    let mut raw = [0u8; 24];
    if fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut raw))
        .is_err()
    {
        return String::new();
    }
    let s: String = b64(&raw).chars().filter(|c| c.is_ascii_alphanumeric()).take(24).collect();
    sset(ctx, "clash_secret", &s);
    s
}

fn b64(data: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(A[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// `_boot_id` — through the platform seam, not around it.
///
/// Paired with `intent`, this is what lets the watchdog tell a mid-session
/// crash (same boot, recover it) from a reboot (different boot, leave it). It
/// is also the most platform-specific thing on this path — `kern.boottime` on
/// a Mac, `/proc/stat`'s `btime` on Linux — so it belongs behind `Platform`
/// with the rest of Phase 5's seam, and a second reader here would be one more
/// place for the two to disagree about what boot it is.
pub fn boot_id() -> String {
    Mac.boot_id().unwrap_or_default()
}

pub fn env_num(k: &str, d: u16) -> u16 {
    std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
}
pub fn env_or(k: &str, d: &str) -> String {
    std::env::var(k).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| d.to_string())
}
pub fn read(p: &Path) -> String { fs::read_to_string(p).unwrap_or_default() }
pub fn sget(state: &str, key: &str) -> String {
    state.lines().filter_map(|l| l.strip_prefix(&format!("{key}="))).next_back().unwrap_or("").into()
}

/// 0600 on a file the shell writes through `mktemp` + `mv`.
///
/// `mktemp` creates its file 0600 and `mv` carries that mode onto the
/// destination, so every file the shell renders this way is owner-only whether
/// or not it also chmods. Rust's `fs::write` creates 0666 & ~umask — 0644 on a
/// normal Mac. For `state` that is a cosmetic difference; for `host.json` and
/// `vm.json`, which carry the escape server's uuid/password, it is the file
/// permission that keeps the credentials off every other account on the
/// machine. Same idiom, same mode.
pub fn private(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o600));
}

/// `reload_if_running` — re-render and restart, but only if the router is up.
///
/// Every edit that changes what gets routed ends here: a lane change, a corp
/// sync, an import. With the router down there is nothing to reload and the
/// next `up` renders anyway, so this is silent; with it up, the `==>` lines are
/// deliberately NOT swallowed, because a watchdog-triggered reload is only
/// legible in watch.log if the restart said whether it worked.
pub fn reload_if_running(ctx: &Ctx) -> Result<(), String> {
    if host_running(ctx).is_none() {
        return Ok(());
    }
    eprintln!("==> reloading router");
    // `cmd_render >/dev/null` — the rendered paths are noise here; its `==>`
    // line still shows, and a render that fails ends the command, because the
    // shell's cmd_render dies rather than returning.
    cmd_render(ctx)?;
    router_stop(ctx);
    router_up(ctx)?;
    Ok(())
}

/// `sset` — replace the last value or append, preserving the rest.
pub fn sset(ctx: &Ctx, key: &str, val: &str) {
    let p = ctx.cfg.join("state");
    let body = read(&p);
    let mut out: Vec<String> = body
        .lines()
        .filter(|l| !l.starts_with(&format!("{key}=")))
        .map(|s| s.to_string())
        .collect();
    out.push(format!("{key}={val}"));
    if fs::write(&p, out.join("\n") + "\n").is_ok() {
        private(&p);
    }
}

pub fn host_running(ctx: &Ctx) -> Option<i32> {
    let pid: i32 = read(&ctx.pidfile()).trim().parse().ok()?;
    if pid > 0 && unsafe { libc::kill(pid, 0) } == 0 { Some(pid) } else { None }
}

// ---------------------------------------------------------------- render

fn escape_outbounds(ctx: &Ctx) -> Result<(Value, String), String> {
    let servers: Vec<Value> =
        serde_json::from_str(&read(&ctx.cfg.join("servers.json"))).unwrap_or_default();
    let selected = { let s = ctx.sget("selected"); if s.is_empty() { "auto".into() } else { s } };
    let interval = env_or("ROWT_AUTO_INTERVAL", "20m");
    match ctx.mode().as_str() {
        // No tunnel at all: the escape lane's rules point at `direct` instead.
        "local" => Ok((json!([]), format!("127.0.0.1:{}", ctx.clash_port))),
        "vm" => {
            let ip = ctx.sget("vm_ip");
            if ip.is_empty() {
                return Err("vm ip unknown (run: rowt vm up)".into());
            }
            Ok((json!([{"type":"socks","tag":"escape","server":ip,
                        "server_port":ctx.port + 1,"version":"5"}]), String::new()))
        }
        // Detected HERE rather than passed in, because `build_escape_outbounds
        // host` runs its own `detect_iface` and `assemble_host` runs another —
        // two probes of the network per render. Wasteful, and reproduced
        // anyway: `cli-diff` compares the argv trace, and collapsing the two
        // would be a behavior change smuggled in as a tidy-up. Listed in
        // PORTING.md §6.7 to fix on the shell side, where it belongs.
        _ => Ok((group(&servers, &Mac.detect_iface().unwrap_or_default(), &selected, &interval),
                 format!("127.0.0.1:{}", ctx.clash_port))),
    }
}

pub fn build_host(ctx: &Ctx) -> Result<Value, String> {
    // Order matters to the argv trace: `assemble_host "$(build_escape_outbounds
    // …)"` runs the substitution first, so the outbound builder's probe lands
    // before assemble's own.
    let (escapes, clash) = escape_outbounds(ctx)?;
    let iface = Mac.detect_iface().unwrap_or_default();
    let cache = ctx.cfg.join("cache");
    let esc_src = read(&ctx.cfg.join("escape-domains.txt"));
    let corp_src = read(&ctx.cfg.join("corp-domains.txt"));
    let blk_src = read(&ctx.cfg.join("block-domains.txt"));
    let cached = |n: &str| cache.join(format!("geosite-{n}.srs")).is_file();
    let all: BTreeSet<String> =
        geosites_of(&esc_src).into_iter().chain(geosites_of(&blk_src)).collect();
    let ads = cache.join("geosite-category-ads-all.srs");
    let local = ctx.mode() == "local";
    Ok(render_host(&HostInput {
        escapes,
        listen: "127.0.0.1".into(),
        port: ctx.port as u64,
        iface: iface.to_string(),
        clash,
        secret: clash_secret(ctx),
        log_level: env_or("ROWT_LOG_LEVEL", "warn"),
        final_route: env_or("ROWT_FINAL", "direct"),
        dns_direct: if local { env_or("ROWT_DNS_LOCAL", "1.1.1.1") } else { env_or("ROWT_DNS_DIRECT", "223.5.5.5") },
        private_default: env_or("ROWT_PRIVATE_DEFAULT", "corp"),
        private_cidrs: ["10.0.0.0/8","172.16.0.0/12","192.168.0.0/16","100.64.0.0/10","169.254.0.0/16"]
            .iter().map(|s| s.to_string()).collect(),
        lists: Lists {
            escape_domains: parse_list(&esc_src, Filter::Domain),
            corp_domains: parse_list(&corp_src, Filter::Domain),
            corp_cidrs: parse_list(&corp_src, Filter::Cidr),
            block_domains: parse_list(&blk_src, Filter::Domain),
        },
        escape_outbound: if local { "direct".into() } else { "escape".into() },
        geo: Geo {
            escape: geosites_of(&esc_src).into_iter().filter(|n| cached(n)).collect(),
            block: geosites_of(&blk_src).into_iter().filter(|n| cached(n)).collect(),
            sets: all.iter().filter(|n| cached(n))
                .map(|n| (format!("geosite-{n}"),
                          cache.join(format!("geosite-{n}.srs")).to_string_lossy().into_owned()))
                .collect(),
            ads_path: if ads.is_file() { ads.to_string_lossy().into_owned() } else { String::new() },
        },
    }))
}

pub fn cmd_render(ctx: &Ctx) -> Result<String, String> {
    let mode = ctx.mode();
    let servers_n = serde_json::from_str::<Vec<Value>>(&read(&ctx.cfg.join("servers.json")))
        .map(|v| v.len()).unwrap_or(0);
    // `[ "$mode" = local ] || [ -s "$SERVERS" ]` — the shell asks whether the
    // FILE has bytes in it, not whether it holds any servers. `[]` passes, and
    // the render goes on to emit an `auto` selector with nothing under it.
    // Counting the array instead is the better check and still the wrong one to
    // make here (PORTING.md §6.7): `render` succeeding is what `up`, `reload`
    // and `config import` branch on, so tightening it changes three other
    // commands' behavior in a commit that is not about them.
    let has_servers = std::fs::metadata(ctx.cfg.join("servers.json"))
        .map(|m| m.is_file() && m.len() > 0).unwrap_or(false);
    if mode != "local" && !has_servers {
        return Err("no servers — run: rowt server add '<vless://...>' or rowt sub add <url>".into());
    }
    // Before the render, not after: the render VALIDATES what it produced by
    // running `sing-box check` on it, so a missing binary would surface as
    // "generated host.json failed validation" — a config problem, on a machine
    // whose only problem is that it has not downloaded the router yet.
    crate::fetch::ensure_singbox(&ctx.cfg)?;
    eprintln!("==> rendering configs (mode={mode}, port={}, servers={servers_n})", ctx.port);
    let host = build_host(ctx)?;

    let servers: Vec<Value> =
        serde_json::from_str(&read(&ctx.cfg.join("servers.json"))).unwrap_or_default();
    let selected = { let s = ctx.sget("selected"); if s.is_empty() { "auto".into() } else { s } };
    let guest = group(&servers, "", &selected, &env_or("ROWT_AUTO_INTERVAL", "20m"));
    let vm = render_vm(&guest, "0.0.0.0", (ctx.port + 1) as u64,
                       &format!("0.0.0.0:{}", ctx.clash_port + 1),
                       &clash_secret(ctx), &env_or("ROWT_LOG_LEVEL", "warn"));

    // Written to a temp file and moved into place: a plain truncate-then-write
    // lets a concurrent reader — sing-box starting while this re-renders — see
    // an empty file and die on "decode config EOF".
    for (path, val) in [(ctx.host_cfg(), &host), (ctx.vm_cfg(), &vm)] {
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(val).unwrap() + "\n")
            .map_err(|e| format!("write {}: {e}", tmp.display()))?;
        // Before the check, not after the rename: these hold the escape
        // server's credentials, and the window where they are world-readable
        // is the window `sing-box check` spends on them.
        private(&tmp);
        let ok = Command::new(ctx.sb()).arg("check").arg("-c").arg(&tmp)
            .stdout(Stdio::null()).stderr(Stdio::inherit()).status()
            .map(|s| s.success()).unwrap_or(false);
        if !ok {
            let _ = fs::remove_file(&tmp);
            return Err(format!("generated {} failed validation", path.display()));
        }
        fs::rename(&tmp, &path).map_err(|e| format!("mv: {e}"))?;
    }
    Ok(format!("  host: {}\n  vm:   {}", ctx.host_cfg().display(), ctx.vm_cfg().display()))
}

// ---------------------------------------------------------------- supervision

/// Split sing-box's output the way the shell's python splitter does: per-lane
/// connection failures into lane-*.log, everything else into host.log, with the
/// block flood kept out of host.log entirely.
pub fn run_splitter(host_log: &Path, logdir: &Path) {
    let mut h = fs::OpenOptions::new().create(true).append(true).open(host_log).ok();
    let mut lanes: std::collections::HashMap<String, fs::File> = Default::default();
    let stdin = std::io::stdin();
    for line in BufReader::new(stdin.lock()).lines().map_while(Result::ok) {
        let clean = strip_ansi(&line);
        if let Some((dom, tag, reason)) = parse_conn(&clean) {
            let lane = match tag.as_str() {
                "block" | "direct" | "corp" => tag.clone(),
                _ => "escape".to_string(),
            };
            let ts = find_ts(&clean).unwrap_or_default();
            let f = lanes.entry(lane.clone()).or_insert_with(|| {
                fs::OpenOptions::new().create(true).append(true)
                    .open(logdir.join(format!("lane-{lane}.log"))).unwrap()
            });
            let _ = writeln!(f, "{ts}\t{dom}\t{reason}");
            if lane != "block" {
                if let Some(h) = h.as_mut() { let _ = writeln!(h, "{line}"); }
            }
        } else if let Some(h) = h.as_mut() {
            let _ = writeln!(h, "{line}");
        }
    }
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for c2 in chars.by_ref() {
                if c2 == 'm' { break; }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// `open connection to (.+?):\d+ using outbound/\w+\[([^\]]+)\]: (.*)`
fn parse_conn(s: &str) -> Option<(String, String, String)> {
    let i = s.find("open connection to ")? + "open connection to ".len();
    let rest = &s[i..];
    let marker = rest.find(" using outbound/")?;
    let hostport = &rest[..marker];
    let dom = hostport.rsplit_once(':')?.0.to_string();
    let after = &rest[marker + " using outbound/".len()..];
    let lb = after.find('[')?;
    let rb = after.find(']')?;
    let tag = after[lb + 1..rb].to_string();
    let reason = after[rb + 1..].strip_prefix(": ")?.trim_end().to_string();
    Some((dom, tag, reason))
}

fn find_ts(s: &str) -> Option<String> {
    let b = s.as_bytes();
    for i in 0..b.len().saturating_sub(18) {
        let w = &s[i..i + 19];
        let ok = w.as_bytes().iter().enumerate().all(|(j, c)| match j {
            4 | 7 => *c == b'-',
            10 => *c == b' ',
            13 | 16 => *c == b':',
            _ => c.is_ascii_digit(),
        });
        if ok { return Some(w.to_string()); }
    }
    None
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Returns the live `Child`, not just its pid, and the caller must keep it.
///
/// A spawned process this process never reaps becomes a ZOMBIE when it exits —
/// and `kill(pid, 0)` SUCCEEDS on a zombie. So a sing-box that dies on startup
/// (bad config, port already bound) would read as "running" for as long as
/// rowt-rs lives. bash does not have this problem: it reaps its own background
/// jobs, so its `kill -0` reports the truth. Found by the parity sandbox, whose
/// fake sing-box exits immediately — the shell said "could not start router"
/// and rowt-rs said "✓ running".
pub fn start_router(ctx: &Ctx, split: bool) -> Result<std::process::Child, String> {
    fs::create_dir_all(ctx.logdir()).ok();
    // sing-box logs to STDERR. The shell merges the streams (`2>&1`) before the
    // splitter sees them; piping them separately and reading only one deadlocks
    // the process the moment the unread pipe fills — which looks like a tunnel
    // that stops working. `exec` keeps the pid as sing-box's, not the shell's.
    let merged = format!("exec {} run -c {} 2>&1",
                         shell_quote(&ctx.sb().to_string_lossy()),
                         shell_quote(&ctx.host_cfg().to_string_lossy()));
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(&merged);
    let child = if split {
        // The splitter is its own process, so both outlive this CLI — the shell
        // gets the same shape from process substitution.
        cmd.stdout(Stdio::piped()).stderr(Stdio::inherit());
        let mut child = cmd.spawn().map_err(|e| format!("spawn sing-box: {e}"))?;
        let out = child.stdout.take().ok_or("no stdout")?;
        let me = std::env::current_exe().map_err(|e| e.to_string())?;
        Command::new(me)
            .arg("_splitter").arg(ctx.host_log()).arg(ctx.logdir())
            .stdin(Stdio::from(out))
            .stdout(Stdio::null()).stderr(Stdio::null())
            .spawn().map_err(|e| format!("spawn splitter: {e}"))?;
        child
    } else {
        let log = fs::OpenOptions::new().create(true).append(true).open(ctx.host_log())
            .map_err(|e| format!("open host.log: {e}"))?;
        let log2 = log.try_clone().map_err(|e| e.to_string())?;
        cmd.stdout(Stdio::from(log)).stderr(Stdio::from(log2));
        cmd.spawn().map_err(|e| format!("spawn sing-box: {e}"))?
    };
    fs::write(ctx.pidfile(), format!("{}\n", child.id())).ok();
    Ok(child)
}

/// `clash_curl` — the one shape every clash API call takes. Reproduced argv for
/// argv (including the `-X GET` the shell always passes and curl would otherwise
/// imply) because `cli-diff` compares the trace: a call that behaves the same but
/// is spelled differently is still a difference, and the next one might not be.
pub fn clash_curl(ctx: &Ctx, method: &str, path: &str, body: Option<&str>) -> Option<String> {
    let secret = clash_secret(ctx);
    let auth = format!("Authorization: Bearer {secret}");
    let url = format!("http://{}{path}", ctx.controller());
    let mut c = Command::new("curl");
    c.args(["--noproxy", "*", "-sS", "-m", "6", "-X", method, "-H", &auth]);
    if let Some(b) = body {
        c.args(["-H", "Content-Type: application/json", "-d", b]);
    }
    let o = c.arg(&url).stderr(Stdio::null()).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

fn clash_ok(ctx: &Ctx) -> bool {
    let secret = clash_secret(ctx);
    Command::new("curl")
        .args(["--noproxy", "*", "-sS", "-m", "2", "-H", &format!("Authorization: Bearer {secret}"),
               &format!("http://{}/version", ctx.controller())])
        .stdout(Stdio::null()).stderr(Stdio::null())
        .status().map(|s| s.success()).unwrap_or(false)
}

pub fn router_ready(ctx: &Ctx, tries: u32, child: &mut std::process::Child) -> bool {
    let mut seen = false;
    for _ in 0..tries {
        std::thread::sleep(std::time::Duration::from_secs(1));
        // Reap first. Without this the exited child stays a zombie, `kill(pid,
        // 0)` keeps succeeding, and `host_running` below would never notice.
        if matches!(child.try_wait(), Ok(Some(_)) | Err(_)) { return false; }
        if host_running(ctx).is_none() { return false; }
        if clash_ok(ctx) { seen = true; }
    }
    seen
}

/// The metrics collector is located the way the monitor is: next to the shell,
/// or in a local cargo build. Its lifetime is tied to the router's — a collector
/// left polling a clash API that is gone spins on connection refusals.
pub fn collector_bin(ctx: &Ctx) -> Option<PathBuf> {
    let here = repo_root();
    let mut cands: Vec<PathBuf> = vec![
        here.join("bin/rowt-collector"),
        here.join("rowt-monitor/target/release/rowt-collector"),
        here.join("rowt-monitor/target/debug/rowt-collector"),
    ];
    cands.push(ctx.cfg.join("bin/rowt-collector"));
    cands.into_iter().find(|c| c.is_file())
}

/// `$HERE` — the directory bin/rowt lives beside, i.e. the repo or brew prefix.
fn repo_root() -> PathBuf {
    std::env::current_exe().ok()
        .and_then(|e| e.parent().map(|d| d.join("../..")))
        .and_then(|d| d.canonicalize().ok())
        .unwrap_or_default()
}

fn collector_running(ctx: &Ctx) -> bool {
    match read(&ctx.cfg.join("collector.pid")).trim().parse::<i32>() {
        Ok(p) => p > 0 && unsafe { libc::kill(p, 0) } == 0,
        Err(_) => false,
    }
}

/// `_start_collector` — the metrics sidecar, tied to the router's lifetime.
///
/// Best-effort at every step: metrics turned off, no collector built, no clash
/// API to poll — each is a reason to do nothing, never a reason to fail the
/// `up` that called it. Without this the router comes up and `rowt mon` stays
/// empty forever, which reads as a broken monitor rather than an absent feed.
pub fn start_collector(ctx: &Ctx) {
    if env_or("ROWT_METRICS", "on") == "off" || collector_running(ctx) {
        return;
    }
    let (Some(bin), Some(ep)) = (collector_bin(ctx), controller(ctx)) else { return };
    let sec = clash_secret(ctx);
    fs::create_dir_all(ctx.logdir()).ok();
    let Ok(log) = fs::OpenOptions::new().create(true).append(true)
        .open(ctx.logdir().join("collector.log")) else { return };
    let Ok(log2) = log.try_clone() else { return };
    let mut cmd = Command::new(&bin);
    cmd.env("ROWT_COLLECT_EP", &ep).env("ROWT_COLLECT_SECRET", &sec).env("ROWT_CFG", &ctx.cfg)
        .stdin(Stdio::null()).stdout(Stdio::from(log)).stderr(Stdio::from(log2));
    // `nohup` — the collector outlives the shell that started it, so a hangup
    // on the terminal `rowt up` was typed into must not reach it.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
            Ok(())
        });
    }
    if let Ok(c) = cmd.spawn() {
        let _ = fs::write(ctx.cfg.join("collector.pid"), format!("{}\n", c.id()));
    }
}

pub fn stop_collector(ctx: &Ctx) {
    let pf = ctx.cfg.join("collector.pid");
    if let Ok(p) = read(&pf).trim().parse::<i32>() {
        if p > 0 { unsafe { libc::kill(p, libc::SIGTERM) }; }
    }
    let _ = fs::remove_file(&pf);
    if let Some(b) = collector_bin(ctx) {
        let _ = Command::new("pkill").arg("-f").arg(b)
            .stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
}

pub fn router_stop(ctx: &Ctx) {
    stop_collector(ctx);
    if let Some(pid) = host_running(ctx) {
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
    let _ = fs::remove_file(ctx.pidfile());
    // strays: the shell pkills the exact command line, so this does too
    let _ = Command::new("pkill")
        .arg("-f").arg(format!("{} run -c {}", ctx.sb().display(), ctx.host_cfg().display()))
        .stdout(Stdio::null()).stderr(Stdio::null()).status();
}

/// The router must never be left down with the system proxy still pointing at
/// it — that strands the network.
/// Prints, because the shell's `( cmd_proxy off )` prints: this fires in the
/// MIDDLE of `router down`'s output, and a caller that swallowed the line would
/// leave "  router stopped" as the only trace of a system proxy that was just
/// switched off underneath it.
pub fn ensure_no_limbo(ctx: &Ctx) {
    if host_running(ctx).is_some() { return; }
    say(&cmd_proxy_off(ctx));
}

/// `cmd_proxy off` — the stamp, then the write.
///
/// The shell has no un-stamped proxy-off on this path: `cmd_revert`,
/// `cmd_restart`, `cmd_uninstall` and `_ensure_no_limbo` all go through
/// `cmd_proxy`, which records the intent before it touches anything. Stamping
/// only at the CLI arm and calling the raw `proxy_off` everywhere else leaves
/// `proxy_intent=on` behind a `rowt down` — and that flag is exactly what the
/// watchdog reads to decide whether an off proxy was deliberate, so the next
/// tick would helpfully turn it back on.
///
/// The one deliberate exception stays raw: the captive-portal drop, which the
/// shell spells `_captive_proxy_off` and documents as "intent stays on".
pub fn cmd_proxy_off(ctx: &Ctx) -> String {
    sset(ctx, "proxy_intent", "off");
    proxy_off(ctx)
}

/// `cmd_proxy on` — the stamp, then the write.
///
/// Stamped BEFORE the router-running guard, like the shell: `rowt proxy on`
/// with the router down still records that you asked for it, and the no-op
/// path that prints "router not running" leaves `proxy_intent=on` behind.
pub fn cmd_proxy_on(ctx: &Ctx, force: bool) -> String {
    sset(ctx, "proxy_intent", "on");
    proxy_on(ctx, force)
}

fn guarded() -> bool {
    std::env::var("ROWT_NO_SYSPROXY").as_deref() == Ok("1")
}

pub fn proxy_off(_ctx: &Ctx) -> String {
    let p = Mac;
    let Some(svc) = p.active_service() else {
        return "  no active network service — proxy already effectively off".into();
    };
    if p.proxy_any_on(&svc) {
        let _ = p.proxy_states_off(&svc, false);
        // Say what happened, not what would have happened: reporting "proxy off"
        // while the guard silently blocked it is how a test convinces you it
        // broke something, or worse, that it didn't.
        if guarded() {
            return format!("  system proxy left alone for '{svc}' — ROWT_NO_SYSPROXY=1");
        }
        format!("  proxy off for '{svc}'")
    } else {
        format!("  proxy already off for '{svc}' — no change (no sudo)")
    }
}

pub fn proxy_on(ctx: &Ctx, force: bool) -> String {
    let p = Mac;
    // Resolved BEFORE the router-running guard, because the shell resolves it
    // before its own — `svc` is computed once at the top of cmd_proxy for every
    // action. Short-circuiting would skip four probes the shell makes, and
    // cli-diff compares the argv trace.
    let svc = p.active_service();
    if !force && host_running(ctx).is_none() {
        return format!(
            "  router not running — nothing is listening on 127.0.0.1:{} yet.\n  start it first:  rowt up      (that renders, starts the router, and sets the proxy)\n  override anyway: rowt proxy on --force",
            ctx.port
        );
    }
    let Some(svc) = svc else {
        return "no active network service — connect a network first".into();
    };
    let pointing = p.proxy_pointing_ok(&svc, ctx.port);
    let bypass = rowt_platform::bypass_ok(&svc);
    if pointing && bypass {
        return format!("  ✓ '{svc}' already proxied to 127.0.0.1:{} with local bypass — no change (no sudo)", ctx.port);
    }
    let mut out = String::new();
    if !pointing {
        if guarded() {
            eprintln!("==> system proxy left alone — ROWT_NO_SYSPROXY=1");
        } else {
            eprintln!("==> pointing '{svc}' system proxy at 127.0.0.1:{} (SOCKS + HTTP; needs admin)", ctx.port);
        }
        let _ = p.proxy_set(&svc, ctx.port);
    } else {
        out.push_str(&format!("  proxy already pointing at 127.0.0.1:{} — left as is\n", ctx.port));
    }
    if !bypass {
        let want: Vec<String> = rowt_platform::bypass_want().iter().map(|s| s.to_string()).collect();
        let _ = p.proxy_set_bypass(&svc, &want);
        if guarded() {
            out.push_str("  (system proxy left alone — ROWT_NO_SYSPROXY=1)");
        } else {
            out.push_str("  ✓ proxy bypass set (*.local / private ranges / captive-probe hosts)");
        }
    } else {
        out.push_str("  ✓ local/mDNS bypass already set — left as is");
    }
    out
}

/// The escape selector's current pick, via the clash API. None when the router
/// is down or the API does not answer — the shell omits the line entirely then.
pub fn clash_selected(ctx: &Ctx) -> Option<String> {
    let body = clash_curl(ctx, "GET", "/proxies/escape", None)?;
    let v: Value = serde_json::from_str(&body).ok()?;
    v.get("now")?.as_str().filter(|s| !s.is_empty()).map(|s| s.to_string())
}

/// An HTTP status through a proxy, "000" when it never answered.
pub fn curl_code(proxy: &str, url: &str) -> String {
    let out = Command::new("curl")
        .args(["-sS", "-m", "8", "-x", proxy, "-o", "/dev/null", "-w", "%{http_code}", url])
        .stderr(Stdio::null()).output();
    match out {
        Ok(o) if !o.stdout.is_empty() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => "000".into(),
    }
}

/// `tail -8 "$HOST_LOG" >&2` — the last thing sing-box said before it gave up.
///
/// Spelled as the external command the shell runs, not read in-process: the
/// point of showing it is the same either way, but `cli-diff` compares the argv
/// trace, and a `tail` that never happens is a difference.
fn tail_host_log(ctx: &Ctx) {
    let _ = Command::new("tail").arg("-8").arg(ctx.host_log())
        .stdout(Stdio::inherit()).stderr(Stdio::inherit()).status();
}

pub fn router_up(ctx: &Ctx) -> Result<String, String> {
    if let Some(pid) = host_running(ctx) {
        return Ok(format!("  router already running (pid {pid})"));
    }
    if !ctx.host_cfg().is_file() {
        // Printed, not swallowed: `cmd_router up` renders inline when there is
        // no config yet and the shell shows where it put it. Only the caller
        // that spells it `cmd_render >/dev/null` gets to be quiet.
        say(&cmd_render(ctx)?);
    }
    // A missing or too-old sing-box is the single most common reason a fresh
    // machine cannot come up, and it is fixable here: fetch it before the start
    // rather than reporting a spawn failure the user cannot act on.
    crate::fetch::ensure_singbox(&ctx.cfg)?;
    eprintln!("==> starting rule-router on 127.0.0.1:{}", ctx.port);
    let mut child = start_router(ctx, true)?;
    let pid = child.id();
    if router_ready(ctx, 5, &mut child) {
        start_collector(ctx);
        return Ok(format!("  ✓ running (pid {pid})"));
    }
    eprintln!("error: router did not come up healthy within 5s (exited or API wedged) — see {}",
              ctx.host_log().display());
    tail_host_log(ctx);
    eprintln!("==> retrying once (plain log, after a short settle)…");
    router_stop(ctx);
    let _ = child.try_wait();
    std::thread::sleep(std::time::Duration::from_secs(2));
    let mut child = start_router(ctx, false)?;
    let pid = child.id();
    if router_ready(ctx, 5, &mut child) {
        start_collector(ctx);
        return Ok(format!("  ✓ running (pid {pid})  [plain log]"));
    }
    let _ = child.try_wait();
    // Reported here rather than through the error string, because the shell
    // prints the message, THEN the log tail, THEN clears the proxy — and an
    // error that surfaces only at the top level would land after all of it.
    eprintln!("error: router failed to come up healthy — see {}", ctx.host_log().display());
    tail_host_log(ctx);
    ensure_no_limbo(ctx);
    Err(crate::SILENT.into())
}

pub fn router_down(ctx: &Ctx) -> String {
    router_stop(ctx);
    ensure_no_limbo(ctx);
    "  router stopped".into()
}

// ---------------------------------------------------------------- the arms
//
// `up`, `down`, `reload` and `restart` are COMPOSITIONS: each one drives the
// same handful of steps — render, router, proxy, VM — in a particular order,
// and the order is the behaviour. They print as they go rather than returning
// one assembled string, because that is what the shell does and what the
// interleaving has to match: `router down` clears a stranded proxy in the
// middle of its own output, so a caller that buffered its pieces would report
// them in an order that never happened.
//
// `[ -s "$SERVERS" ]` is the shell's guard and stays byte-shaped (PORTING.md
// §6.7): `[]` is two bytes, so a config whose server list is an empty array
// passes it and then renders a selector with nothing under it.
/// `echo` only when there is something to echo. An arm that already printed as
/// it went returns an empty string, and a blank line where the shell emits
/// nothing is a difference like any other.
fn say(s: &str) {
    if !s.is_empty() {
        println!("{s}");
    }
}

fn has_servers(ctx: &Ctx) -> bool {
    fs::metadata(ctx.cfg.join("servers.json")).map(|m| m.is_file() && m.len() > 0).unwrap_or(false)
}

fn pkill(pattern: &str) {
    let _ = Command::new("pkill").arg("-f").arg(pattern)
        .stdout(Stdio::null()).stderr(Stdio::null()).status();
}

/// `cmd_revert` — what `rowt down` runs.
///
/// Every step is best-effort and the order is defensive: intent goes down FIRST
/// so a watchdog tick landing mid-teardown treats this as deliberate instead of
/// resurrecting what is being torn down, and each step is isolated so a failure
/// with no active network service (a plane, a captive portal) cannot abort the
/// teardown before sing-box is actually killed.
pub fn cmd_revert(ctx: &Ctx, here: &Path) -> Result<String, String> {
    sset(ctx, "intent", "down");
    say(&cmd_proxy_off(ctx));
    say(&router_down(ctx));
    match crate::vm::cmd(ctx, here, "down") {
        Ok(s) => say(&s),
        Err(e) => eprintln!("error: {e}"),
    }
    // Last resort, and deliberately broader than `router_stop`'s: that one
    // pkills the exact `<sb> run -c <host.json>` line, which misses a stray
    // started against some other config — the high-CPU orphan this is for.
    pkill(&format!("{} run", ctx.sb().display()));
    let _ = fs::remove_file(ctx.pidfile());
    eprintln!("==> stopped — system proxy off, sing-box down (servers kept at {}).",
              ctx.cfg.join("servers.json").display());
    Ok(String::new())
}

/// `cmd_reload` — re-detect the network, re-render, bounce, re-apply the proxy.
pub fn cmd_reload(ctx: &Ctx, here: &Path) -> Result<String, String> {
    if !has_servers(ctx) {
        return Err(format!("nothing to reload — run '{} up' first", crate::PROG));
    }
    eprintln!("==> reloading for the current network…");
    if ctx.sget("mode") == "vm" && crate::vm::vm_running() {
        // re-detects the VM's bridged IP, re-renders, re-pushes the guest
        say(&crate::vm::cmd(ctx, here, "up")?);
    } else {
        say(&cmd_render(ctx)?);   // re-detects the physical NIC
    }
    // Require the router to be HEALTHY (clash API answering, not merely a live
    // process) before touching the proxy: a wedged start must fail honestly
    // rather than report "reloaded" over a dead tunnel.
    let mode = ctx.mode();
    let up_ok = if host_running(ctx).is_some() {
        router_stop(ctx);
        router_up(ctx)
    } else {
        // No stop when nothing is running — the shell calls plain `router up`
        // here, and a `pkill` for a router that was never started is a step
        // that did not happen.
        router_up(ctx)
    };
    let healthy = match up_ok {
        Ok(s) => { say(&s); true }
        Err(e) => { if e != crate::SILENT { eprintln!("error: {e}"); } false }
    };
    // host: the router's own API-readiness gate decides. vm: health is the VM's
    // business, so fall back to the process check.
    if mode == "host" {
        if !healthy {
            eprintln!("error: reload: router did not come up healthy — proxy left OFF (no limbo). See '{} router log'.",
                      crate::PROG);
            ensure_no_limbo(ctx);
            return Err(crate::SILENT.into());
        }
    } else if host_running(ctx).is_none() {
        eprintln!("error: reload: router did not come up — proxy left OFF (no limbo).");
        ensure_no_limbo(ctx);
        return Err(crate::SILENT.into());
    }
    say(&cmd_proxy_on(ctx, false));
    sset(ctx, "intent", "up");
    sset(ctx, "boot", &boot_id());
    eprintln!("==> reloaded (mode={mode}).");
    Ok(String::new())
}

/// `cmd_setup` — what `rowt up` runs: choose a mode, render, start, proxy.
pub fn cmd_setup(ctx: &Ctx, here: &Path, args: &[String]) -> Result<String, String> {
    let (mut forced, mut force) = (String::new(), false);
    for a in args {
        match a.as_str() {
            "--force" | "-f" => force = true,
            "host" | "vm" | "local" => forced = a.clone(),
            "" => {}
            _ => return Err(format!("usage: {} up [host|vm|local] [--force]", crate::PROG)),
        }
    }
    // No target named: ask the network whether the escape lane is needed at
    // all. Only here — an explicit `up host` must never be second-guessed.
    if forced.is_empty() {
        let canaries: Vec<String> = env_or("ROWT_GFW_CANARIES",
            "https://www.google.com/generate_204 https://www.youtube.com/generate_204")
            .split_whitespace().map(str::to_string).collect();
        if Mac.direct_reaches_escape(&canaries, env_or("ROWT_GFW_TIMEOUT", "3").parse().unwrap_or(3)) {
            forced = "local".into();
            eprintln!("==> escape-lane hosts answer directly on this network — choosing local mode (no tunnel)");
        }
    }
    if forced != "local" && !has_servers(ctx) {
        return Err(format!("first import servers: {p} server add '<vless://...>' or {p} sub add <url>",
                           p = crate::PROG));
    }
    // Recorded BEFORE the work, so a setup that FAILS still reads as "wanted
    // up" and the watchdog keeps trying — which is the point of the flag.
    sset(ctx, "intent", "up");
    sset(ctx, "boot", &boot_id());
    // Already fully set up in the requested mode? Then nothing to do — except
    // in vm mode, where the bridged DHCP lease can move under us (reboot, new
    // lease, network switch), so re-detect and re-wire before saying so.
    if !force && !forced.is_empty() && ctx.sget("mode") == forced && host_running(ctx).is_some()
        && (forced != "vm" || crate::vm::vm_running())
    {
        if forced == "vm" {
            let cur = crate::vm::vm_ip_detect();
            if !cur.is_empty() && cur != ctx.sget("vm_ip") {
                eprintln!("==> VM bridged IP changed ({} -> {cur}) — re-wiring", ctx.sget("vm_ip"));
                say(&crate::vm::cmd(ctx, here, "up")?);
                say(&cmd_render(ctx)?);
                router_stop(ctx);
                say(&router_up(ctx)?);
                say(&cmd_proxy_on(ctx, false));
                eprintln!("==> re-wired to {cur}.");
                return Ok(String::new());
            }
        }
        eprintln!("==> already set up in '{forced}' mode (router running) — nothing to do.");
        return Ok(format!("  re-detect/re-wire: {p} reload   (or '{p} vm up' to refresh the VM)",
                          p = crate::PROG));
    }
    // sing-box up front (may need internet); fails fast with guidance if offline
    crate::fetch::ensure_singbox(&ctx.cfg)?;
    // Best-effort: grab the ad-block rule-set while a VPN is (probably) still
    // up. Non-fatal — the block hand-list works without it, and `fetch host`
    // retries later.
    if !ctx.cfg.join("cache/geosite-category-ads-all.srs").is_file() {
        let _ = crate::fetch::ads_ruleset(&ctx.cfg, false);
    }
    crate::fetch::all_geosites(&ctx.cfg);
    match forced.as_str() {
        "host" => {
            sset(ctx, "mode", "host");
            eprintln!("==> mode forced to 'host' (skipping probe)");
        }
        "local" => {
            sset(ctx, "mode", "local");
            eprintln!("==> mode: local — the escape lane routes direct; no tunnel, no server needed");
        }
        "vm" => {
            eprintln!("==> mode: vm (skipping probe) — bringing up the VM first");
            // sets mode=vm + vm_ip only on success; fails otherwise, mode unchanged
            say(&crate::vm::cmd(ctx, here, "up")?);
        }
        // `cmd_probe || true` — a probe that reaches nothing has already said
        // so, and the render below will fail honestly on its own.
        _ => if let Ok(s) = crate::cmd_probe(ctx) { say(&s); },
    }
    let mode = ctx.mode();
    if mode == "vm" && !crate::vm::vm_running() {   // probe may have chosen vm
        say(&crate::vm::cmd(ctx, here, "up")?);
    }
    say(&cmd_render(ctx)?);
    router_stop(ctx);                    // restart, not up, so a mode switch takes
    // …and the result is NOT propagated, because the shell does not check it:
    // `cmd_router restart` failing leaves `up` to go on and set the proxy and
    // print "done", exiting 0 over a router that never came up. That is a bug
    // (PORTING.md §6.7) — `restart` and `reload` both gate on health and refuse
    // to touch the proxy — and it is bash's bug to fix on the bash side. The
    // router's own no-limbo step has already cleared the proxy by now, so the
    // machine is not stranded; only the exit status lies.
    match router_up(ctx) {               // effect on a router that is already live
        Ok(s) => say(&s),
        Err(e) => if e != crate::SILENT { eprintln!("error: {e}"); },
    }
    say(&cmd_proxy_on(ctx, false));
    // Switched to host mode: the VM is now dead weight (RAM/CPU/battery) and
    // its Lima port-forwards would clash with the router — so power it down.
    if mode == "host" && crate::vm::vm_running() {
        eprintln!("==> stopping the '{}' VM — unused in host mode", crate::vm::VM_NAME);
        match crate::vm::cmd(ctx, here, "down") {
            Ok(s) => say(&s),
            Err(e) => eprintln!("error: {e}"),
        }
    }
    println!();
    let mut out = Vec::new();
    if mode == "local" {
        eprintln!("==> done — mode=local. The escape lane routes direct; block and corp are unchanged.");
        out.push(format!("  back to the tunnel: {} up host", crate::PROG));
    } else {
        eprintln!("==> done — mode={mode}. Listed domains now route through your escape VPN.");
        out.push(format!("  switch anytime: {p} up host / {p} up vm / {p} up local", p = crate::PROG));
    }
    out.push(format!("  edit lanes:     {p} escape add <domain> / {p} corp add <domain>", p = crate::PROG));
    Ok(out.join("\n"))
}

/// `cmd_restart` — bounce the tunnel in place: no re-render, no mode change.
///
/// Strict order so a mid-way failure never leaves an inconsistent state: the
/// system proxy goes OFF first, the components bounce, and it comes back ON
/// only once the router is confirmed up. A failed bounce leaves the proxy off,
/// which is safe; leaving it on would point the machine at a dead port.
pub fn cmd_restart(ctx: &Ctx, here: &Path) -> Result<String, String> {
    if !has_servers(ctx) {
        return Err(format!("nothing to restart — run '{} up' first", crate::PROG));
    }
    eprintln!("==> restart: turning the system proxy off first…");
    say(&cmd_proxy_off(ctx));
    let up_ok = if ctx.sget("mode") == "vm" {
        match crate::vm::cmd(ctx, here, "restart") {
            Ok(s) => { say(&s); host_running(ctx).is_some() }
            Err(e) => { eprintln!("error: {e}"); false }
        }
    } else {
        router_stop(ctx);
        match router_up(ctx) {
            Ok(s) => { say(&s); true }
            Err(e) => { if e != crate::SILENT { eprintln!("error: {e}"); } false }
        }
    };
    if !up_ok {
        eprintln!("error: router did not come back up healthy (exited or clash API wedged) — leaving the system proxy OFF (no limbo). Check '{p} status' / '{p} router log'.",
                  p = crate::PROG);
        return Err(crate::SILENT.into());
    }
    say(&cmd_proxy_on(ctx, false));
    sset(ctx, "intent", "up");            // a live restart means "should be up"
    sset(ctx, "boot", &boot_id());
    Ok(String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_one_in_the_pipeline() {
        // `head -c N /dev/urandom | base64` — standard alphabet, padded.
        let n = |k: usize| b64(&(0..k as u8).collect::<Vec<u8>>());
        assert_eq!(n(1), "AA==");
        assert_eq!(n(2), "AAE=");
        assert_eq!(n(3), "AAEC");
        assert_eq!(n(4), "AAECAw==");
        // The width `clash_secret` actually asks for: 24 bytes, no padding.
        assert_eq!(n(24), "AAECAwQFBgcICQoLDA0ODxAREhMUFRYX");
    }

    #[test]
    fn a_minted_secret_is_at_most_24_alphanumerics() {
        // `| tr -dc 'A-Za-z0-9' | cut -c1-24` — the filter runs BEFORE the cut,
        // so 32 base64 characters yield 24 only when few of them were `+` or
        // `/`. They usually are few, and the shell has always been willing to
        // mint a shorter secret rather than draw again.
        let mint = |raw: &[u8]| -> String {
            b64(raw).chars().filter(|c| c.is_ascii_alphanumeric()).take(24).collect()
        };
        assert_eq!(mint(&(0..24u8).collect::<Vec<u8>>()), "AAECAwQFBgcICQoLDA0ODxAR");
        // The pathological draw: every sextet lands on `+` or `/` and the
        // secret comes out EMPTY. Vanishingly unlikely and not impossible, and
        // an empty secret is an unauthenticated clash API — which is why this
        // is written down rather than assumed away.
        assert_eq!(mint(&[0xfb, 0xff, 0xbf].repeat(8)), "");
    }
}
