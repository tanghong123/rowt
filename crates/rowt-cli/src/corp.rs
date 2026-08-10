//! `rowt corp sync` and `rowt corp suggest` — keeping the corp lane in step with
//! whatever tunnel is up right now.
//!
//! Two independent halves land in the same file, each with its own marker block:
//!
//!   * the CIDR side — a SUPERSET reconcile of the routes the live tunnels
//!     carry, delegated to `rowt_core::reconcile` (already gated at 210 cases
//!     against the Python it replaced)
//!   * the domain side — the DHCP search domains the physical NIC advertises,
//!     via `rowt_core::netdetect`
//!
//! Everything ABOVE the first marker is hand-added and preserved verbatim. That
//! is the contract that makes an auto-managed block safe to live in a file
//! people also edit by hand.

use crate::lifecycle::{self, Ctx};
use crate::{env_or, read, PROG};
use rowt_core::netdetect;
use std::path::Path;
use std::process::{Command, Stdio};

pub const MARKER_CIDR: &str =
    "# --- rowt corp sync (auto-managed; superset of live tunnel routes — do not edit below) ---";
pub const MARKER_DOM: &str =
    "# --- rowt corp sync: DHCP search domains (auto-managed, do not edit below) ---";

fn out(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd).args(args).stderr(Stdio::null()).output().ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

/// An IPv4 address or CIDR alone on its line — how the corp lane's CIDR entries
/// are told apart from its domain suffixes.
fn looks_cidr(l: &str) -> bool {
    let t = l.trim();
    if t.is_empty() {
        return false;
    }
    let (addr, prefix) = match t.split_once('/') {
        Some((a, p)) => (a, Some(p)),
        None => (t, None),
    };
    if let Some(p) = prefix {
        if p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
    }
    let parts: Vec<&str> = addr.split('.').collect();
    // `[0-9]{1,3}(\.[0-9]{1,3}){1,3}` — two to four octets, so netstat's
    // shorthand (`47.88/16`) matches as well as a full address.
    (2..=4).contains(&parts.len())
        && parts.iter().all(|o| !o.is_empty() && o.len() <= 3 && o.chars().all(|c| c.is_ascii_digit()))
}

/// Everything above the first sync marker: hand-added domains AND CIDRs, kept
/// verbatim. Trailing blank lines are trimmed so the block below does not
/// accumulate gaps across rewrites.
pub fn head(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for l in body.lines() {
        if l.starts_with("# --- rowt corp sync") {
            break;
        }
        out.push(l.to_string());
    }
    while out.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        out.pop();
    }
    out
}

/// The CIDRs a human put there — the ones sync must NEVER touch.
pub fn handadded_cidrs(body: &str) -> Vec<String> {
    head(body).iter().filter(|l| looks_cidr(l))
        .map(|l| l.chars().filter(|c| !c.is_whitespace()).collect()).collect()
}

/// The CIDRs currently in the auto-managed block. Old per-label markers are
/// recognised too, so a file written by an earlier version migrates.
pub fn block_cidrs(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inblk = false;
    for l in body.lines() {
        if l.starts_with("# --- rowt corp sync") {
            inblk = true;
            continue;
        }
        if inblk && looks_cidr(l) {
            out.push(l.chars().filter(|c| !c.is_whitespace()).collect());
        }
    }
    out
}

/// The domains in the auto-managed DHCP block — and ONLY that block, which is
/// why the marker is compared exactly rather than by prefix.
pub fn block_domains(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inblk = false;
    for l in body.lines() {
        if l.starts_with("# --- rowt corp sync") {
            inblk = l == MARKER_DOM;
            continue;
        }
        if inblk {
            let t: String = l.chars().filter(|c| !c.is_whitespace()).collect();
            if !t.is_empty() && !t.starts_with('#') {
                out.push(t);
            }
        }
    }
    out
}

/// head + optional domain block + optional CIDR block. The bytes this produces
/// are compared against the file on disk: identical means no write and no
/// reload, which is what lets the watchdog call sync on every tick.
pub fn assemble(head: &[String], dom: &[String], cidr: &[String]) -> String {
    let mut s = head.join("\n");
    if !head.is_empty() {
        s.push('\n');
    }
    if !dom.is_empty() {
        s.push_str(&format!("\n{MARKER_DOM}\n{}\n", dom.join("\n")));
    }
    if !cidr.is_empty() {
        s.push_str(&format!("\n{MARKER_CIDR}\n{}\n", cidr.join("\n")));
    }
    s
}

/// The corp VPN's tunnel: the utun/ipsec/ppp carrying the most non-default
/// routes, excluding Tailscale's CGNAT interface, and only when it carries at
/// least three — fewer than that is a point-to-point link, not a corp network.
fn vpn_iface() -> String {
    let list = out("ifconfig", &["-l"]);
    let (mut best, mut bestn) = (String::new(), 0i64);
    for ifc in list.split_whitespace() {
        if !(ifc.starts_with("utun") || ifc.starts_with("ipsec") || ifc.starts_with("ppp")) {
            continue;
        }
        if !ifc[4.min(ifc.len())..].starts_with(|c: char| c.is_ascii_digit())
            && !ifc.chars().last().map(|c| c.is_ascii_digit()).unwrap_or(false)
        {
            continue;
        }
        if is_cgnat(ifc) {
            continue;
        }
        let n = route_count(ifc);
        if n > bestn {
            best = ifc.to_string();
            bestn = n;
        }
    }
    if bestn >= 3 { best } else { String::new() }
}

fn is_cgnat(ifc: &str) -> bool {
    out("ifconfig", &[ifc]).lines().any(|l| {
        let t = l.trim();
        t.strip_prefix("inet ").map(|rest| {
            let a = rest.split_whitespace().next().unwrap_or("");
            let o: Vec<&str> = a.split('.').collect();
            o.len() == 4 && o[0] == "100"
                && o[1].parse::<u32>().map(|x| (64..=127).contains(&x)).unwrap_or(false)
        }).unwrap_or(false)
    })
}

fn route_count(ifc: &str) -> i64 {
    out("netstat", &["-rn", "-f", "inet"]).lines()
        .filter(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            f.last() == Some(&ifc) && f.first() != Some(&"default")
        }).count() as i64
}

/// Tailscale's tunnel: the utun whose inet sits in 100.64/10.
fn tailscale_iface() -> String {
    out("ifconfig", &["-l"]).split_whitespace()
        .filter(|i| i.starts_with("utun"))
        .find(|i| is_cgnat(i))
        .unwrap_or("")
        .to_string()
}

fn resolve_iface(label: &str) -> String {
    match label {
        "corp" | "auto" | "vpn" => vpn_iface(),
        "tailscale" | "ts" => tailscale_iface(),
        l if l.starts_with("utun") || l.starts_with("ipsec") || l.starts_with("ppp") => {
            if out("ifconfig", &[l]).is_empty() { String::new() } else { l.to_string() }
        }
        _ => String::new(),
    }
}

/// The CIDRs a tunnel routes: its non-default routes, normalised out of
/// netstat's shorthand (`47.88/16` -> `47.88.0.0/16`, `30` -> `30.0.0.0/8`),
/// minus the tunnel's own host route.
fn vpn_cidrs(ifc: &str) -> Vec<String> {
    let gw = out("ifconfig", &[ifc]).lines()
        .find_map(|l| l.trim().strip_prefix("inet ").map(|r| r.split_whitespace().next().unwrap_or("").to_string()))
        .unwrap_or_default();
    let mut v: Vec<String> = Vec::new();
    for l in out("netstat", &["-rn", "-f", "inet"]).lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 2 || f.last() != Some(&ifc) {
            continue;
        }
        let d = f[0];
        if d == "default" || d == gw {
            continue;
        }
        let (addr, pfx) = match d.split_once('/') {
            Some((a, p)) => (a, Some(p.to_string())),
            None => (d, None),
        };
        let mut octs: Vec<String> = addr.split('.').map(|s| s.to_string()).collect();
        if octs.iter().any(|o| o.is_empty() || !o.chars().all(|c| c.is_ascii_digit())) {
            continue;
        }
        let n = octs.len();
        // No prefix given: netstat's shorthand implies one octet = /8, and a
        // full four-octet address is a host route.
        let pfx = pfx.unwrap_or_else(|| if n == 4 { "32".into() } else { (n * 8).to_string() });
        while octs.len() < 4 {
            octs.push("0".into());
        }
        let c = format!("{}/{pfx}", octs.join("."));
        if !v.contains(&c) {
            v.push(c);
        }
    }
    // `sort -t. -k1,1n -k2,2n -k3,3n -k4,4n -u` — numeric by octet, not lexical.
    v.sort_by_key(|c| {
        let a = c.split('/').next().unwrap_or("");
        let o: Vec<u32> = a.split('.').map(|x| x.parse().unwrap_or(0)).collect();
        (o.first().copied().unwrap_or(0), o.get(1).copied().unwrap_or(0),
         o.get(2).copied().unwrap_or(0), o.get(3).copied().unwrap_or(0))
    });
    v.dedup();
    v
}

fn sync_labels(cfg: &Path) -> Vec<String> {
    let body = read(&cfg.join("sync-ifaces.txt"));
    let v: Vec<String> = body.lines()
        .map(|l| l.chars().filter(|c| !c.is_whitespace()).collect::<String>())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    if v.is_empty() { vec!["corp".into()] } else { v }
}

/// The suffixes the user has explicitly tunnelled or blocked. Their choice wins
/// over an auto-discovered corp domain — a network must not be able to
/// de-tunnel something you deliberately put in the escape lane.
fn esc_block_suffixes(cfg: &Path) -> Vec<String> {
    use rowt_core::render::{parse_list, Filter};
    let mut v: Vec<String> = parse_list(&read(&cfg.join("escape-domains.txt")), Filter::Domain);
    v.extend(parse_list(&read(&cfg.join("block-domains.txt")), Filter::Domain));
    v.sort();
    v.dedup();
    v
}

/// The DHCP search domains the physical NIC advertises — corp/internal by
/// definition. Native now: this used to be `_tmo 5 python3 net-detect.py`.
fn dhcp_search_domains() -> Vec<String> {
    if env_or("ROWT_AUTO_CORP_DOMAINS", "1") != "1" {
        return Vec::new();
    }
    netdetect::parse(&out("scutil", &["--dns"])).physical_search
}

pub struct Opts {
    pub dry_run: bool,
    pub no_reload: bool,
    pub quiet: bool,
    pub iface: Option<String>,
}

pub fn sync(ctx: &Ctx, quiet: bool) -> Result<String, String> {
    run(ctx, &Opts { dry_run: false, no_reload: false, quiet, iface: None })
}

pub fn run(ctx: &Ctx, o: &Opts) -> Result<String, String> {
    let cfg = &ctx.cfg;
    let path = cfg.join("corp-domains.txt");
    let body = read(&path);
    let labels = match &o.iface {
        Some(i) => vec![i.clone()],
        None => sync_labels(cfg),
    };

    // A = the union of routes across every label whose tunnel is up RIGHT NOW.
    let mut active: Vec<String> = Vec::new();
    let (mut nlabels, mut nup) = (0usize, 0usize);
    let mut up = String::new();
    for label in &labels {
        nlabels += 1;
        let ifc = resolve_iface(label);
        if ifc.is_empty() {
            continue;
        }
        for c in vpn_cidrs(&ifc) {
            if !active.contains(&c) {
                active.push(c);
            }
        }
        up.push_str(&format!(" {label}->{ifc}"));
        nup += 1;
    }
    active.sort();
    active.dedup();
    let na = active.len();

    // ---- CIDR side: superset reconcile. NOCHANGE keeps the block verbatim,
    // which is what stops a cosmetic re-ordering from triggering a reload.
    let cur_cidr = block_cidrs(&body);
    let private: Vec<String> = ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16",
                                "100.64.0.0/10", "169.254.0.0/16"]
        .iter().map(|s| s.to_string()).collect();
    use rowt_core::reconcile::{load, reconcile, Outcome};
    let r = reconcile(
        &load(&active.join("\n")),
        &load(&handadded_cidrs(&body).join("\n")),
        &load(&cur_cidr.join("\n")),
        &load(&private.join("\n")),
    );
    // NOCHANGE keeps the existing block VERBATIM rather than re-emitting an
    // equivalent one — that is what stops a cosmetic re-ordering from rewriting
    // the file and triggering a reload on every watchdog tick.
    let body_cidr: Vec<String> = match r {
        Outcome::NoChange => cur_cidr.clone(),
        Outcome::Change(v) => v.iter().map(|n| n.to_string()).collect(),
    };

    // ---- domain side: a PERSIST-union of (block ∪ advertised), minus anything
    // the user tunnels or blocks. Persisting matters: a domain learned on the
    // corp LAN must survive a day at home, where nothing advertises it.
    let cur_dom = block_domains(&body);
    let mut dropped: Vec<String> = Vec::new();
    let desired_dom: Vec<String> = if env_or("ROWT_AUTO_CORP_DOMAINS", "1") == "1" {
        let mut union: Vec<String> = cur_dom.clone();
        union.extend(dhcp_search_domains());
        let mut union: Vec<String> = union.into_iter()
            .map(|d| d.to_ascii_lowercase()).filter(|d| !d.is_empty()).collect();
        union.sort();
        union.dedup();
        let esc = esc_block_suffixes(cfg);
        dropped = union.iter().filter(|d| esc.contains(d)).cloned().collect();
        union.into_iter().filter(|d| !esc.contains(d)).collect()
    } else {
        cur_dom.clone()
    };

    let want = assemble(&head(&body), &desired_dom, &body_cidr);
    let (nd, nc) = (desired_dom.len(), body_cidr.len());

    if o.dry_run {
        let mut s = format!("corp sync (dry-run): labels [{}]; up:{}; {na} live CIDR(s)",
                            labels.join(" "), if up.is_empty() { " none".into() } else { up });
        if want == body {
            s.push_str("\n  corp lane already in sync — no change, no reload");
        } else {
            s.push_str(&format!("\n  would set corp lane to {nd} DHCP domain(s) + {nc} CIDR(s):"));
            // `diff <(cat $CORP_DOMAINS) $want | grep -E '^[<>]' | head -40`.
            // The same diff(1) the shell runs, rather than a reimplementation
            // of its hunk format — it is a system tool like date or du, and
            // "identical output" is the whole requirement here.
            let tmp = std::env::temp_dir().join(format!("rowt-corpsync-{}", std::process::id()));
            if std::fs::write(&tmp, &want).is_ok() {
                let d = Command::new("diff").arg(&path).arg(&tmp).output().ok()
                    .map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
                for l in d.lines().filter(|l| l.starts_with('<') || l.starts_with('>')).take(40) {
                    s.push('\n');
                    s.push_str(l);
                }
                let _ = std::fs::remove_file(&tmp);
            }
        }
        return Ok(s);
    }

    if want == body {
        if !o.quiet {
            eprintln!("==> corp lane already in sync ({nd} DHCP domain(s), {nc} CIDR(s); {nup}/{nlabels} tunnel(s) up)");
        }
        return Ok(String::new());
    }
    if !o.quiet {
        for d in &dropped {
            eprintln!("error: corp sync: '{d}' is DHCP-advertised but you keep it in escape/block — your rule wins");
        }
    }
    std::fs::write(&path, &want).map_err(|e| format!("write {}: {e}", path.display()))?;
    let _ = crate::set_mode(&path, 0o600);
    // Logged even under --quiet: this is a real change that triggers a reload,
    // so the watchdog log has to record WHY it reloaded. No-op ticks stay silent.
    eprintln!("==> corp sync: updated corp lane — {nd} DHCP domain(s) + {nc} CIDR(s) (superset of {na} live route(s))");
    if !o.no_reload {
        lifecycle::reload_if_running(ctx)?;
    }
    Ok(String::new())
}

/// `corp suggest` — what this network advertises, and which of it rowt already
/// handles. Never auto-applies the candidates: a machine legitimately sees many
/// internal domains (a shared resolver publishes its own) and only you know
/// which are actually your intranet.
///
/// The DHCP search domains ARE handled automatically — those come from the
/// physical NIC and `corp sync` mirrors them — so they are marked rather than
/// suggested. Everything else is a scoped resolver's domain and is a candidate.
pub fn suggest(_ctx: &Ctx) -> Result<String, String> {
    let d = netdetect::parse(&out("scutil", &["--dns"]));
    if d.internal_domains.is_empty() {
        eprintln!("==> no internal DNS domains visible right now.");
        return Ok("  This only sees them when the signal is live — on the corp LAN, or with the\n  corp VPN connected. Re-run while on your work network / VPN.".into());
    }
    let mut o = vec!["Internal DNS domains this network advertises:".to_string()];
    let mut extras: Vec<&String> = Vec::new();
    for dom in &d.internal_domains {
        if d.physical_search.contains(dom) {
            o.push(format!("  {dom}  ← DHCP search domain — rowt corp-routes this AUTOMATICALLY"));
        } else {
            o.push(format!("  {dom}"));
            extras.push(dom);
        }
    }
    if !d.corp_nameservers.is_empty() {
        o.push(format!("Corp/private DNS servers seen: {}", d.corp_nameservers.join(", ")));
    }
    o.push(String::new());
    if extras.is_empty() {
        o.push("These are corp-routed for you automatically — nothing to add.".into());
    } else {
        o.push("The DHCP search domain(s) above are added for you. The rest are only candidates".into());
        o.push("(scoped resolvers) — add the ones that are truly your intranet:".into());
        o.push(format!("  {PROG} corp add{}", extras.iter().map(|e| format!(" {e}")).collect::<String>()));
        o.push("Drop any that aren't yours (e.g. a shared resolver's own domain).".into());
    }
    Ok(o.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "# hand-added\n\
                        intranet.corp.example\n\
                        10.99.0.0/16\n\
                        \n\
                        # --- rowt corp sync: DHCP search domains (auto-managed, do not edit below) ---\n\
                        hq.corp.example\n\
                        \n\
                        # --- rowt corp sync (auto-managed; superset of live tunnel routes — do not edit below) ---\n\
                        30.0.0.0/8\n";

    #[test]
    fn the_head_is_everything_above_the_first_marker() {
        assert_eq!(head(FILE), ["# hand-added", "intranet.corp.example", "10.99.0.0/16"]);
    }

    #[test]
    fn hand_added_cidrs_are_never_confused_with_managed_ones() {
        // The invariant sync exists to preserve: a CIDR you typed is untouchable.
        assert_eq!(handadded_cidrs(FILE), ["10.99.0.0/16"]);
        assert_eq!(block_cidrs(FILE), ["30.0.0.0/8"]);
    }

    #[test]
    fn the_domain_block_is_matched_by_its_exact_marker() {
        // Both markers start "# --- rowt corp sync", so a prefix test would pull
        // the CIDR block's contents into the domain list.
        assert_eq!(block_domains(FILE), ["hq.corp.example"]);
    }

    #[test]
    fn assemble_round_trips_a_file_it_wrote() {
        let got = assemble(&head(FILE), &block_domains(FILE), &block_cidrs(FILE));
        assert_eq!(got, FILE, "a no-op sync must produce byte-identical output, or it reloads forever");
    }

    #[test]
    fn an_empty_block_emits_no_marker() {
        let h = vec!["a.example".to_string()];
        assert_eq!(assemble(&h, &[], &[]), "a.example\n");
    }

    #[test]
    fn cidr_detection_accepts_netstats_shorthand() {
        assert!(looks_cidr("10.0.0.0/8"));
        assert!(looks_cidr("47.88/16"));
        assert!(looks_cidr("  192.168.1.1  "));
        assert!(!looks_cidr("corp.example"));
        assert!(!looks_cidr("10.0.0.0/x"));
        assert!(!looks_cidr(""));
    }
}
