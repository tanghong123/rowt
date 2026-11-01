//! `rowt doctor [api-host]` — the Mac AROUND rowt, ending in FLAGS. The
//! shell's `cmd_doctor`, row for row.
//!
//! `onboard` answers how far rowt's own setup is; this answers what decides
//! whether that setup goes smoothly: who holds rowt's ports, what other
//! proxy/VPN software runs and who carries the default route, a PAC that would
//! bypass rowt, what answers over the physical NIC versus through rowt, whether
//! the proxy can be written without a password, and what can be imported.
//!
//! The FLAG names are a contract: `skills/rowt/SKILL.md` maps each one to an
//! action and to who takes it, so they are byte-identical to the shell's and
//! stay in the shell's order. Rename one and that table changes in the same
//! pass. `cli-diff` compares this arm with the shell over the doctor cases, and
//! selftest 52 misspells one FLAG in the shell to prove the comparison sees it.
//!
//! Read-only: no sudo beyond `sudo -n -l` (a listing), no brew update, every
//! network wait bounded by curl's `-m`, and the three probes that are not
//! shimmed in the sandbox (`lsof`, `ps`, `nc`) read the real machine on both
//! sides. Every shimmed command is called with the shell's argv, in the shell's
//! branches, so the trace matches too; the seven curl probes run concurrently
//! on both sides, which is why the cases carry `PARITY_UNORDERED=1`.

use crate::lifecycle::Ctx;
use crate::{die, env_or, read, sh_date, watch_self, PROG};
use rowt_platform::{Mac, Platform};
use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::process::{Command, Stdio};

/// `$(cmd …)`: stdout with its trailing newlines dropped, stderr discarded,
/// a command that cannot run reading as nothing.
fn out(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd).args(args).stderr(Stdio::null()).output().ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').to_string())
        .unwrap_or_default()
}

/// `_dr_row`: `printf '%-8s %s\n'`.
fn row(o: &mut Vec<String>, k: &str, v: &str) {
    o.push(format!("{k:<8} {v}"));
}

/// `_dr_flag`: `printf '  %-22s %s'`, one line per flag.
fn flag(flags: &mut Vec<String>, name: &str, text: &str) {
    flags.push(format!("  {name:<22} {text}"));
}

/// `command -v <name>`: the first executable file of that name on PATH.
fn path_of(name: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    std::env::var("PATH").unwrap_or_default().split(':')
        .filter(|d| !d.is_empty())
        .map(|d| Path::new(d).join(name))
        .find(|c| c.is_file() && std::fs::metadata(c).map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false))
        .map(|c| c.display().to_string())
        .unwrap_or_default()
}

/// `sed -n '1s/.*stable \([0-9][0-9.]*\).*/\1/p'` on brew's first line: the
/// greedy `.*` lands on the LAST "stable " that a digit follows, and the
/// version is the run of digits and dots from there.
pub fn stable_of(line1: &str) -> String {
    let mut from = line1.len();
    while let Some(i) = line1[..from].rfind("stable ") {
        let rest = &line1[i + "stable ".len()..];
        if rest.starts_with(|c: char| c.is_ascii_digit()) {
            let n = rest.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(rest.len());
            return rest[..n].to_string();
        }
        from = i;
    }
    String::new()
}

/// `sort -t. -k1,1n -k2,2n -k3,3n | tail -1` over two versions: three numeric
/// fields (a field that is not a number sorts as 0, as `-n` reads it), then,
/// with every key equal, the whole line byte by byte — sort's last resort when
/// nothing asks it to be stable.
pub fn newest<'a>(a: &'a str, b: &'a str) -> &'a str {
    fn key(v: &str) -> [i64; 3] {
        let mut k = [0i64; 3];
        for (i, f) in v.split('.').take(3).enumerate() {
            let t = f.trim_start();
            let (neg, t) = match t.strip_prefix('-') { Some(r) => (true, r), None => (false, t.strip_prefix('+').unwrap_or(t)) };
            let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
            let n: i64 = digits.parse().unwrap_or(0);
            k[i] = if neg { -n } else { n };
        }
        k
    }
    match key(a).cmp(&key(b)) {
        std::cmp::Ordering::Less => b,
        std::cmp::Ordering::Greater => a,
        std::cmp::Ordering::Equal => if a.as_bytes() <= b.as_bytes() { b } else { a },
    }
}

/// `_dr_port_owner`: lsof sees only this user's processes; a root service
/// holds a port invisibly, so no answer falls back to simply connecting.
fn port_owner(port: &str) -> String {
    let pid = out("lsof", &["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-t"])
        .lines().next().unwrap_or("").to_string();
    if !pid.is_empty() {
        let cmd = out("ps", &["-o", "command=", "-p", &pid]);
        if cmd.contains("/rowt/") {
            return format!("rowt ({pid})");
        }
        let name = out("ps", &["-o", "comm=", "-p", &pid]);
        let name = name.rsplit('/').next().unwrap_or("");
        return format!("{} (pid {pid})", if name.is_empty() { "?" } else { name });
    }
    let listening = Command::new("nc").args(["-z", "-G", "1", "127.0.0.1", port])
        .stdout(Stdio::null()).stderr(Stdio::null()).status()
        .map(|s| s.success()).unwrap_or(false);
    if listening { "an unseen owner (a root service?)".into() } else { "free".into() }
}

const PROXY_APPS: [(&str, &[&str]); 8] = [
    ("Shadowrocket", &["shadowrocket"]),
    ("Clash", &["clash", "mihomo", "verge"]),
    ("FlClash", &["flclash"]),
    ("V2Box", &["v2box"]),
    ("V2Ray/Xray", &["v2ray", "xray"]),
    ("Surge/Stash/Quantumult", &["surge", "stash", "quantumult"]),
    ("Hiddify/NekoRay", &["hiddify", "nekoray", "nekobox"]),
    ("another sing-box", &[]),   // `^sing-box$`: the exact name, below
];
const VPN_APPS: [(&str, &[&str]); 7] = [
    ("Cisco Secure Client", &["vpnagentd", "acvpn", "cisco"]),
    ("GlobalProtect", &["globalprotect", "pangp"]),
    ("Zscaler", &["zscaler"]),
    ("FortiClient", &["forti"]),
    ("Ivanti/Pulse", &["pulse", "ivanti", "dsaccess"]),
    ("OpenVPN", &["openvpn", "tunnelblick"]),
    ("WireGuard", &["wireguard"]),
];
const OVERLAY_APPS: [(&str, &[&str]); 2] = [
    ("Tailscale", &["tailscale", "ipnextension"]),
    ("ZeroTier", &["zerotier"]),
];

/// The shell's awk over `ps -axo comm=`: one label per product, matched on the
/// lowercased basename, every rule tried for every line; rowt's own sing-box
/// (under …/rowt/bin) is skipped. Three lists — proxy clients, VPN clients,
/// overlays — each in the order the rules stand, or "none".
pub fn classify_apps(ps: &str) -> [String; 3] {
    let mut seen: [Vec<&'static str>; 3] = [vec![], vec![], vec![]];
    for line in ps.lines() {
        if line.ends_with("/rowt/bin/sing-box") {
            continue;
        }
        let n = line.rsplit('/').next().unwrap_or("").to_lowercase();
        let groups: [&[(&str, &[&str])]; 3] = [&PROXY_APPS, &VPN_APPS, &OVERLAY_APPS];
        for (g, table) in groups.iter().enumerate() {
            for (label, prefixes) in table.iter() {
                let hit = if prefixes.is_empty() { n == "sing-box" } else { prefixes.iter().any(|p| n.starts_with(p)) };
                if hit && !seen[g].contains(label) {
                    seen[g].push(label);
                }
            }
        }
    }
    let groups: [&[(&str, &[&str])]; 3] = [&PROXY_APPS, &VPN_APPS, &OVERLAY_APPS];
    let mut o: [String; 3] = Default::default();
    for g in 0..3 {
        let names: Vec<&str> = groups[g].iter().map(|(l, _)| *l).filter(|l| seen[g].contains(l)).collect();
        o[g] = if names.is_empty() { "none".into() } else { names.join(", ") };
    }
    o
}

/// `^100\.(6[4-9]|[7-9][0-9]|1[01][0-9]|12[0-7])\.` — an address in the CGNAT
/// range an overlay (Tailscale) uses.
pub fn is_cgnat(addr: &str) -> bool {
    let mut it = addr.split('.');
    let (a, b, c) = (it.next(), it.next(), it.next());
    let (Some(a), Some(b), Some(_)) = (a, b, c) else { return false };
    if a != "100" || b.starts_with('0') || !b.bytes().all(|x| x.is_ascii_digit()) {
        return false;
    }
    b.parse::<u32>().map(|n| (64..=127).contains(&n)).unwrap_or(false)
}

/// The interface an `ifconfig` line belongs to: a line starting in a lowercase
/// letter names one (`utun3: flags=…`), one trailing colon dropped.
fn iface_of(line: &str) -> Option<String> {
    if !line.starts_with(|c: char| c.is_ascii_lowercase()) {
        return None;
    }
    let first = line.split_whitespace().next().unwrap_or("");
    Some(first.strip_suffix(':').unwrap_or(first).to_string())
}

/// Tunnels that are up, overlays aside: every utun/ipsec/ppp with an inet
/// address outside 100.64/10, sorted as `sort -u` sorts, joined ", ".
pub fn tunnels_up(ifconfig: &str) -> String {
    let mut cur = String::new();
    let mut set: BTreeSet<String> = BTreeSet::new();
    for line in ifconfig.lines() {
        if let Some(i) = iface_of(line) {
            cur = i;
        }
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() >= 2 && f[0] == "inet"
            && (cur.starts_with("utun") || cur.starts_with("ipsec") || cur.starts_with("ppp"))
            && !is_cgnat(f[1])
        {
            set.insert(cur.clone());
        }
    }
    set.into_iter().collect::<Vec<_>>().join(", ")
}

/// The first interface carrying a 100.64/10 address, or nothing.
pub fn overlay_iface(ifconfig: &str) -> String {
    let mut cur = String::new();
    for line in ifconfig.lines() {
        if let Some(i) = iface_of(line) {
            cur = i;
        }
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.windows(2).any(|w| w[0] == "inet" && is_cgnat(w[1])) {
            return cur;
        }
    }
    String::new()
}

/// `networksetup -listallhardwareports`: the port whose `Device:` line names
/// the interface.
pub fn hardware_port(listing: &str, dev: &str) -> String {
    let mut p = String::new();
    for line in listing.lines() {
        if line.starts_with("Hardware Port:") {
            p = line.get(15..).unwrap_or("").to_string();
        }
        if line == format!("Device: {dev}") {
            return p;
        }
    }
    String::new()
}

/// `_dr_pv`: the value of a `scutil --proxy` field (`  HTTPEnable : 1`).
pub fn proxy_field(px: &str, k: &str) -> String {
    px.lines().find_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        (f.first() == Some(&k)).then(|| f.get(2).copied().unwrap_or("").to_string())
    }).unwrap_or_default()
}

/// `sed -E 's#//[^@/]*@#//***@#'`: the first `//user:pass@` masked.
pub fn mask_userinfo(s: &str) -> String {
    let mut from = 0;
    while let Some(i) = s[from..].find("//") {
        let start = from + i + 2;
        let rest = &s[start..];
        if let Some(j) = rest.find(['@', '/']) {
            if rest.as_bytes()[j] == b'@' {
                return format!("{}//***@{}", &s[..start - 2], &rest[j + 1..]);
            }
        }
        from = start;
    }
    s.to_string()
}

/// `_dr_served`: an HTTP status that proves the host itself answered. 403 is
/// how an API refuses a region, which is exactly what is being looked for.
pub fn served(code: &str) -> bool {
    let b = code.as_bytes();
    if b.len() != 3 || !b.iter().all(|c| c.is_ascii_digit()) {
        return false;
    }
    match b[0] {
        b'2' | b'3' => true,
        b'4' => code != "403",
        _ => false,
    }
}

/// `_dr_probe`: one bounded curl, its status or why it failed. `||` on the
/// status, as the shell has it — a timeout is a result, not an error.
fn probe(args: &[String]) -> String {
    let r = Command::new("curl")
        .args(["-sS", "-o", "/dev/null", "-m", "5", "-w", "%{http_code}"]).args(args)
        .stderr(Stdio::null()).output();
    let Ok(r) = r else { return "fail(127)".into() };
    let code = String::from_utf8_lossy(&r.stdout).trim_end_matches('\n').to_string();
    if r.status.success() && !code.is_empty() && code != "000" {
        return code;
    }
    let rc = r.status.code().unwrap_or_else(|| {
        use std::os::unix::process::ExitStatusExt;
        128 + r.status.signal().unwrap_or(0)
    });
    match rc {
        6 => "dns-fail".into(),
        7 => "refused".into(),
        28 => "timeout".into(),
        35 => "tls-fail".into(),
        52 => "empty".into(),
        56 => "reset".into(),
        n => format!("fail({n})"),
    }
}

pub fn run(ctx: &Ctx, here: &Path, args: &[String]) -> Result<String, String> {
    if args.first().map(|a| a.starts_with('-')).unwrap_or(false) {
        die(&ctx.cfg, &format!("usage: {PROG} doctor [api-host]"));
    }
    // The API of the agent driving the setup — the one connection that has to
    // survive the switch from the old VPN.
    let api = args.first().cloned().unwrap_or_else(|| env_or("ROWT_DOCTOR_API", "api.anthropic.com"));
    let api = match api.find("://") { Some(i) => api[i + 3..].to_string(), None => api };
    let api = api.split('/').next().unwrap_or("").to_string();
    let port = ctx.port.to_string();
    let clash = ctx.clash_port.to_string();
    let mut o: Vec<String> = Vec::new();
    let mut flags: Vec<String> = Vec::new();
    o.push(format!("{PROG} doctor (read-only) — {}", sh_date("+%Y-%m-%d %H:%M")));

    // system
    let mut arch = out("uname", &["-m"]);
    if out("sysctl", &["-n", "sysctl.proc_translated"]) == "1" {
        arch.push_str(" (Rosetta; the Mac is arm64)");
    }
    let brew_bin = path_of("brew");
    // `${SHELL:-?}` — but bash sets SHELL itself when it starts without one
    // (the login shell from passwd), so the shell never sees it unset; the
    // sandbox's `env -i` is where that shows. `?` is what is left when passwd
    // has nothing either.
    let sh = crate::diag::login_shell();
    let sh = if sh.is_empty() { "?".to_string() } else { sh };
    let sh = sh.rsplit('/').next().unwrap_or("").to_string();
    row(&mut o, "system", &format!("macOS {} {arch} · brew {} · shell {sh}",
        out("sw_vers", &["-productVersion"]), if brew_bin.is_empty() { "absent" } else { &brew_bin }));
    if brew_bin.is_empty() {
        flag(&mut flags, "NO_BREW", "Homebrew is not installed");
    }
    if arch == "x86_64" {
        flag(&mut flags, "INTEL_MAC", "brew builds rowt from source (needs Rust; several minutes)");
    }

    // rowt. Only a tap that is already tapped is asked about: an untapped one
    // makes `brew info` clone it from GitHub, which can hang on this very network.
    let version = env!("ROWT_SHELL_VERSION");
    let mut stable = String::new();
    if !brew_bin.is_empty() {
        let repo = out("brew", &["--repository"]);
        if Path::new(&format!("{repo}/Library/Taps/tanghong123/homebrew-tap")).is_dir() {
            let info = Command::new("brew").env("HOMEBREW_NO_AUTO_UPDATE", "1")
                .args(["info", "tanghong123/tap/rowt"]).stderr(Stdio::null()).output().ok()
                .map(|r| String::from_utf8_lossy(&r.stdout).into_owned()).unwrap_or_default();
            stable = stable_of(info.lines().next().unwrap_or(""));
        }
    }
    row(&mut o, "rowt", &format!("{version} at {}{}", watch_self().display(),
        if stable.is_empty() { String::new() } else { format!(" · tap has {stable}") }));
    if !stable.is_empty() && version != stable && newest(version, &stable) == stable {
        flag(&mut flags, "ROWT_OUTDATED", &format!("{version} installed, {stable} available"));
    }

    // ports
    let o1 = port_owner(&port);
    let o2 = port_owner(&clash);
    row(&mut o, "ports", &format!("{port}: {o1} · {clash}: {o2}"));
    let busy = |owner: &str| owner != "free" && !owner.starts_with("rowt (");
    if busy(&o1) {
        flag(&mut flags, "PORT_BUSY", &format!("{port} (the proxy port) is held by {o1}"));
    }
    if busy(&o2) {
        flag(&mut flags, "PORT_BUSY", &format!("{clash} (the controller port) is held by {o2}"));
    }
    for p in rowt_core::ports::rows(&read(&ctx.cfg.join("ports.tsv"))) {
        let o3 = port_owner(&p.port);
        row(&mut o, "port", &format!("{} {}: {o3}", p.name, p.port));
        if busy(&o3) {
            flag(&mut flags, "PORT_BUSY", &format!("{} (the port named {}) is held by {o3}", p.port, p.name));
        }
    }

    // other proxy / VPN software, and the tunnels that are up
    let apps = classify_apps(&out("ps", &["-axo", "comm="]));
    let vpn_up: Vec<String> = out("scutil", &["--nc", "list"]).lines()
        .filter(|l| l.contains("(Connected)"))
        .map(|l| l.split('"').nth(1).unwrap_or("").to_string()).collect();
    let vpn_up = vpn_up.join(", ");
    let tun_ifs = tunnels_up(&out("ifconfig", &[]));
    // `VPN clients` is the recognised corp VPN client APPS running — not whether
    // any VPN works. With no known client but a tunnel up, say that; it does not
    // claim the tunnel IS the corp VPN. Whether the VPN WORKS is the `corp`
    // row's, and there only as a checker's verdict.
    let mut corp_app = apps[1].clone();
    if corp_app == "none" && !tun_ifs.is_empty() {
        corp_app = format!("no known client; a tunnel is up ({tun_ifs})");
    }
    row(&mut o, "apps", &format!("proxy clients: {} · VPN clients: {corp_app} · overlay: {}{}", apps[0], apps[2],
        if vpn_up.is_empty() { String::new() } else { format!(" · connected VPN services: {vpn_up}") }));
    row(&mut o, "corp", &crate::corp_view());

    // network
    let dflt: Vec<String> = out("route", &["-n", "get", "default"]).lines()
        .filter(|l| l.contains("interface:"))
        .map(|l| l.split_whitespace().nth(1).unwrap_or("").to_string()).collect();
    let dflt = dflt.join("\n");
    let phys = Mac.detect_iface().unwrap_or_default();
    let hwport = hardware_port(&out("networksetup", &["-listallhardwareports"]), &phys);
    let overlay_if = overlay_iface(&out("ifconfig", &[]));
    row(&mut o, "network", &format!("physical {}{} · default route via {}{}{}",
        if phys.is_empty() { "none" } else { &phys },
        if hwport.is_empty() { String::new() } else { format!(" ({hwport})") },
        if dflt.is_empty() { "none" } else { &dflt },
        if tun_ifs.is_empty() { String::new() } else { format!(" · tunnels up: {tun_ifs}") },
        if overlay_if.is_empty() { String::new() } else { format!(" · 100.64/10 overlay on {overlay_if}") }));
    if phys.is_empty() {
        flag(&mut flags, "NO_PHYSICAL_NETWORK", "no Wi-Fi or Ethernet interface is up");
    }
    if dflt.starts_with("utun") || dflt.starts_with("ipsec") || dflt.starts_with("ppp") {
        flag(&mut flags, "VPN_HOLDS_DEFAULT", &format!("{dflt} carries the default route"));
    }

    // proxy settings, and this shell's own proxy env (user:pass@ masked)
    let px = out("scutil", &["--proxy"]);
    let pv = |k: &str| proxy_field(&px, k);
    let sys = if pv("HTTPEnable") == "1" { format!("{}:{}", pv("HTTPProxy"), pv("HTTPPort")) } else { "off".into() };
    let pac = if pv("ProxyAutoConfigEnable") == "1" { pv("ProxyAutoConfigURLString") } else { String::new() };
    let wpad = pv("ProxyAutoDiscoveryEnable");
    let apx = ["https_proxy", "HTTPS_PROXY", "all_proxy", "ALL_PROXY", "http_proxy", "HTTP_PROXY"].iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty())).unwrap_or_default();
    let apx = mask_userinfo(&apx);
    row(&mut o, "proxy", &format!("system {sys} · PAC {}{} · this shell's proxy env: {}",
        if pac.is_empty() { "off" } else { &pac }, if wpad == "1" { " · WPAD on" } else { "" },
        if apx.is_empty() { "none" } else { &apx }));
    if sys != "off" && sys != format!("127.0.0.1:{port}") {
        flag(&mut flags, "SYSTEM_PROXY_OTHER", &format!("the system proxy points at {sys}"));
    }
    if !pac.is_empty() {
        flag(&mut flags, "PAC_ON", &pac);
    }
    if wpad == "1" {
        flag(&mut flags, "PAC_ON", "proxy auto-discovery (WPAD) is on");
    }

    // reachability — in parallel, each bounded by curl -m
    let api_url = format!("https://{api}/");
    let mut probes: Vec<(&str, Vec<String>)> = Vec::new();
    let nic = |url: &str| vec!["--noproxy".to_string(), "*".into(), "--interface".into(), phys.clone(), url.into()];
    if !phys.is_empty() {
        probes.push(("nic_g", nic("https://www.google.com/generate_204")));
        probes.push(("nic_gh", nic("https://github.com/")));
        probes.push(("nic_bd", nic("https://www.baidu.com/")));
        probes.push(("nic_api", nic(&api_url)));
    }
    probes.push(("def_api", vec!["--noproxy".into(), "*".into(), api_url.clone()]));
    if o1 != "free" {
        let via = format!("http://127.0.0.1:{port}");
        probes.push(("px_g", vec!["-x".into(), via.clone(), "https://www.google.com/generate_204".into()]));
        probes.push(("px_api", vec!["-x".into(), via, api_url.clone()]));
    }
    let results: HashMap<&str, String> = std::thread::scope(|s| {
        let hs: Vec<_> = probes.iter().map(|(n, a)| s.spawn(move || (*n, probe(a)))).collect();
        hs.into_iter().map(|h| h.join().unwrap_or(("", String::new()))).collect()
    });
    let r = |n: &str| results.get(n).cloned().unwrap_or_else(|| "-".into());
    row(&mut o, "reach", &format!("over {} (rowt's direct lane): google {} · github {} · baidu {} · {api} {}",
        if phys.is_empty() { "the NIC" } else { &phys }, r("nic_g"), r("nic_gh"), r("nic_bd"), r("nic_api")));
    row(&mut o, "", &format!("default route, no proxy: {api} {}{}", r("def_api"),
        if o1 != "free" { format!(" · through 127.0.0.1:{port}: google {} · {api} {}", r("px_g"), r("px_api")) } else { String::new() }));
    if !phys.is_empty() && !served(&r("nic_bd")) && !served(&r("nic_g")) {
        flag(&mut flags, "NET_DOWN", &format!("nothing answers over {phys} (offline, a captive portal, or a shell without network)"));
    } else if !phys.is_empty() {
        if served(&r("nic_g")) {
            flag(&mut flags, "OPEN_INTERNET", &format!("Google answers over {phys}: no firewall to escape here"));
        }
        if !served(&r("nic_gh")) {
            flag(&mut flags, "GITHUB_BLOCKED", &format!("github.com over {phys}: {}", r("nic_gh")));
        }
        if !served(&r("nic_api")) {
            flag(&mut flags, "AGENT_API_NEEDS_TUNNEL", &format!("{api} over {phys}: {}", r("nic_api")));
            if !apx.is_empty() {
                let mine = [format!("://127.0.0.1:{port}"), format!("://127.0.0.1:{port}/"),
                            format!("://localhost:{port}"), format!("://localhost:{port}/")];
                if mine.iter().any(|m| apx.ends_with(m.as_str())) {
                    flag(&mut flags, "AGENT_VIA_PROXY_ENV", &format!("this shell's API calls go to {apx} — rowt's port, so they follow rowt"));
                } else {
                    flag(&mut flags, "AGENT_VIA_PROXY_ENV", &format!("this shell's API calls go to {apx} — NOT rowt's port: they stop when that proxy does"));
                }
            } else if served(&r("def_api")) {
                flag(&mut flags, "AGENT_VIA_VPN", &format!("this shell reaches {api} only through {}",
                    if dflt.is_empty() { "the current VPN" } else { &dflt }));
            }
        }
    }

    // admin rights. `sudo -n -l` lists without a password once ANY NOPASSWD rule
    // exists; `sudo -n -l <cmd>` would not do — it also succeeds for commands that
    // are merely allowed WITH a password. Asked only when the rule file exists.
    let sudoers = crate::watch::SUDOERS;
    let rule = Path::new(sudoers).exists() && out("sudo", &["-n", "-l"]).lines().any(|l| {
        l.find("NOPASSWD").map(|i| l[i..].contains("networksetup -setwebproxy ")).unwrap_or(false)
    });
    if rule {
        row(&mut o, "sudo", &format!("passwordless proxy writes: yes ({sudoers})"));
    } else {
        row(&mut o, "sudo", "passwordless proxy writes: no");
        flag(&mut flags, "NO_SUDO_RULE", "setting the system proxy will ask for an admin password");
    }

    // import sources
    let yq_s = if rowt_platform::which("yq") { "present" } else { "absent" };
    let src = crate::onboard::detect_import_sources(here).join(", ");
    row(&mut o, "import", &format!("clients with servers to import: {} · yq {yq_s}", if src.is_empty() { "none" } else { &src }));
    if src.is_empty() {
        flag(&mut flags, "NO_IMPORT_SOURCE", "no Shadowrocket / Clash Verge / V2Box / FlClash config found");
    }
    if (src.contains("clash-verge") || src.contains("flclash")) && yq_s == "absent" {
        flag(&mut flags, "YQ_MISSING", "Clash-family imports need yq");
    }

    o.push(String::new());
    if flags.is_empty() {
        o.push("FLAGS: none".into());
    } else {
        o.push("FLAGS".into());
        o.extend(flags);
    }
    Ok(o.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tap_version_is_the_last_stable_a_digit_follows() {
        assert_eq!(stable_of("==> tanghong123/tap/rowt: stable 3.8.28"), "3.8.28");
        assert_eq!(stable_of("==> tanghong123/tap/rowt: 3.8.8 → stable 3.8.28, HEAD"), "3.8.28");
        assert_eq!(stable_of("==> stable channel: stable 3.9.0 (bottled), stable x"), "3.9.0");
        assert_eq!(stable_of("==> tanghong123/tap/rowt: HEAD"), "");
        assert_eq!(stable_of(""), "");
    }

    #[test]
    fn newest_sorts_three_numeric_fields_then_bytes() {
        assert_eq!(newest("3.8.28", "3.8.9"), "3.8.28");   // 28 > 9 numerically, not as text
        assert_eq!(newest("3.8.28", "3.10.0"), "3.10.0");
        assert_eq!(newest("3.8.28", "3.8.28"), "3.8.28");
        assert_eq!(newest("4.0", "3.99.99"), "4.0");
        assert_eq!(newest("3.8.28", "3.8.28_1"), "3.8.28_1"); // equal keys: the longer line sorts last
        assert_eq!(newest("3.8.28", "3.8"), "3.8.28");      // a missing field reads as 0
    }

    #[test]
    fn apps_are_labelled_once_each_in_the_rules_order_whatever_ps_says() {
        let ps = "/usr/bin/zerotier-one\n/Applications/Shadowrocket.app/Contents/MacOS/Shadowrocket\n/opt/clash\n\
                  /Applications/Tailscale.app/Contents/MacOS/Tailscale\n/x/wireguard-go\n/y/vpnagentd\n/a/b/Clash Verge\n\
                  /Users/x/.config/rowt/bin/sing-box\nsing-box\n/opt/FlClash\n";
        assert_eq!(classify_apps(ps), [
            "Shadowrocket, Clash, FlClash, another sing-box".to_string(),
            "Cisco Secure Client, WireGuard".to_string(),
            "Tailscale, ZeroTier".to_string(),
        ]);
        assert_eq!(classify_apps("/usr/sbin/sshd\n"), ["none".to_string(), "none".to_string(), "none".to_string()]);
        // rowt's own engine — the copy it RUNS, under the config dir — is skipped
        // by its path; a sing-box anywhere else is "another", the formula's
        // bundled copy under libexec included (it is never the one running).
        assert_eq!(classify_apps("/Users/me/.config/rowt/bin/sing-box\n")[0], "none");
        assert_eq!(classify_apps("/opt/homebrew/bin/sing-box\n")[0], "another sing-box");
    }

    #[test]
    fn the_cgnat_range_is_100_64_to_100_127() {
        assert!(is_cgnat("100.64.0.1"));
        assert!(is_cgnat("100.127.255.254"));
        assert!(!is_cgnat("100.63.0.1"));
        assert!(!is_cgnat("100.128.0.1"));
        assert!(!is_cgnat("10.64.0.1"));
        assert!(!is_cgnat("100.064.0.1"));
        assert!(!is_cgnat("100.64"));
    }

    const IFCONFIG: &str = "lo0: flags=8049<UP,LOOPBACK,RUNNING,MULTICAST> mtu 16384\n\
\tinet 127.0.0.1 netmask 0xff000000\n\
en0: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500\n\
\tinet 192.0.2.15 netmask 0xffffff00 broadcast 192.0.2.255\n\
utun4: flags=8051<UP,POINTOPOINT,RUNNING,MULTICAST> mtu 1280\n\
\tinet 100.100.7.9 --> 100.100.7.9 netmask 0xffffffff\n\
utun3: flags=8051<UP,POINTOPOINT,RUNNING,MULTICAST> mtu 1400\n\
\tinet 10.8.0.2 --> 10.8.0.1 netmask 0xffffffff\n\
ppp0: flags=8051<UP,POINTOPOINT,RUNNING,MULTICAST> mtu 1280\n\
\tinet6 fe80::1%ppp0 prefixlen 64\n\
ipsec0: flags=8051<UP,POINTOPOINT,RUNNING,MULTICAST> mtu 1280\n\
\tinet 198.51.100.7 --> 198.51.100.1 netmask 0xffffffff\n";

    #[test]
    fn tunnels_are_the_non_overlay_utuns_sorted_and_the_overlay_is_named() {
        // utun4 carries CGNAT (an overlay), ppp0 has only inet6: neither is a tunnel here.
        assert_eq!(tunnels_up(IFCONFIG), "ipsec0, utun3");
        assert_eq!(overlay_iface(IFCONFIG), "utun4");
        assert_eq!(tunnels_up("en0: flags=0\n\tinet 192.0.2.1\n"), "");
        assert_eq!(overlay_iface("en0: flags=0\n\tinet 192.0.2.1\n"), "");
    }

    #[test]
    fn the_hardware_port_is_the_header_above_the_matching_device() {
        let l = "Hardware Port: Wi-Fi\nDevice: en0\nEthernet Address: 00:00:5e:00:53:00\n\nHardware Port: Thunderbolt Bridge\nDevice: bridge0\n";
        assert_eq!(hardware_port(l, "en0"), "Wi-Fi");
        assert_eq!(hardware_port(l, "bridge0"), "Thunderbolt Bridge");
        assert_eq!(hardware_port(l, "en5"), "");
        assert_eq!(hardware_port(l, ""), "");
    }

    #[test]
    fn scutil_proxy_fields_are_the_third_word_of_their_line() {
        let px = "<dictionary> {\n  HTTPEnable : 1\n  HTTPPort : 7890\n  HTTPProxy : 127.0.0.1\n  ProxyAutoConfigEnable : 0\n}\n";
        assert_eq!(proxy_field(px, "HTTPEnable"), "1");
        assert_eq!(proxy_field(px, "HTTPProxy"), "127.0.0.1");
        assert_eq!(proxy_field(px, "ProxyAutoConfigURLString"), "");
        assert_eq!(proxy_field("", "HTTPEnable"), "");
    }

    #[test]
    fn userinfo_is_masked_once_and_only_before_the_host() {
        assert_eq!(mask_userinfo("http://me:secret@127.0.0.1:7891"), "http://***@127.0.0.1:7891");
        assert_eq!(mask_userinfo("http://127.0.0.1:7890"), "http://127.0.0.1:7890");
        assert_eq!(mask_userinfo("socks5h://a@b//c@d"), "socks5h://***@b//c@d");
        assert_eq!(mask_userinfo("http://host/path@x"), "http://host/path@x");
        assert_eq!(mask_userinfo(""), "");
    }

    #[test]
    fn served_is_2xx_3xx_and_every_4xx_but_403() {
        for c in ["200", "204", "301", "404", "429"] { assert!(served(c), "{c}"); }
        for c in ["403", "500", "000", "", "timeout", "dns-fail", "fail(0)", "20"] { assert!(!served(c), "{c}"); }
    }
}
