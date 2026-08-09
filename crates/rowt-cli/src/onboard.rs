//! `rowt onboard` — the state-aware getting-started checklist.
//!
//! One surface serves two readers. A human runs it to see how far along setup
//! is and the exact next command; the AI "rowt" skill runs it as the
//! authoritative, version-current guide. That is why it ALWAYS prints the full
//! reference after the checklist rather than only when something is unfinished
//! — a guide that hides itself once you are set up is no use to the second
//! reader at all.
//!
//! Reads state, changes nothing. Safe to run at any time.

use crate::lifecycle::{self, Ctx};
use crate::{env_or, fetch, read, PROG};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// `_ob` — a checklist line, plus an indented command block when there is one.
fn ob(o: &mut Vec<String>, done: bool, label: &str, cmds: &str) {
    o.push(format!("  [{}] {label}", if done { "✓" } else { " " }));
    if !cmds.is_empty() {
        for l in cmds.lines() {
            o.push(format!("        {l}"));
        }
    }
}

fn have(cmd: &str) -> bool {
    rowt_platform::which(cmd)
}

/// Which supported clients on THIS machine have importable data.
///
/// Time-boxed at 6s each, and that is not a nicety: these scan client config
/// directories including iCloud paths, which can stall for a long time on a
/// dataless volume. `onboard` must never hang.
fn detect_import_sources(here: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for script in ["config/sr-import.py", "config/foreign-import.py"] {
        let p = here.join(script);
        if !p.is_file() {
            continue;
        }
        let body = Command::new("perl")
            .args(["-e", "alarm shift; exec @ARGV", "6", "python3"])
            .arg(&p).arg("--detect")
            .stderr(Stdio::null()).output().ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        for l in body.lines() {
            if !l.trim().is_empty() {
                out.push(l.to_string());
            }
        }
    }
    out
}

fn rc_has_shell_init() -> bool {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    for rc in [".zshrc", ".bashrc", ".bash_profile", ".profile"] {
        let b = read(&home.join(rc));
        if b.contains("rowt shell integration") || b.contains("rowt shell-init") {
            return true;
        }
    }
    false
}

pub fn run(ctx: &Ctx, here: &Path) -> String {
    let cfg = &ctx.cfg;
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let mut o: Vec<String> = Vec::new();
    let mut pending = 0;

    o.push("rowt onboarding — a macOS split-router: chosen sites via a personal tunnel".into());
    o.push("(escape), corp intranet via the corp VPN (corp), everything else direct.".into());
    o.push(String::new());
    o.push(format!("Setup checklist — each line shows the command; do the first [ ], then re-run '{PROG} onboard':"));
    o.push(String::new());
    let mode = { let m = ctx.sget("mode"); if m.is_empty() { "?".to_string() } else { m } };

    // 1. installed on PATH
    if have(PROG) {
        ob(&mut o, true, "installed on PATH", "brew install tanghong123/tap/rowt");
    } else {
        ob(&mut o, false, "install rowt", "brew install tanghong123/tap/rowt   (or from the repo: ./install.sh)");
        pending += 1;
    }

    // 2. sing-box present — auto-fetched, but that needs GitHub reachable
    let sb = ctx.sb();
    if fetch::sb_ok(&sb) {
        let v = Command::new(&sb).arg("version").stderr(Stdio::null()).output().ok()
            .map(|x| String::from_utf8_lossy(&x.stdout).into_owned()).unwrap_or_default();
        let v = v.lines().next().unwrap_or("").split_whitespace().nth(2).unwrap_or("");
        ob(&mut o, true, &format!("sing-box ready ({v})"), &format!("{PROG} fetch host   (re-fetch)"));
    } else {
        ob(&mut o, false, "fetch sing-box (needs GitHub reachable — have a VPN ON)",
           &format!("{PROG} fetch host   ('{PROG} up' also fetches it)"));
        pending += 1;
    }

    // 3. at least one server — importing from an existing client is easiest
    let servers: Vec<Value> =
        serde_json::from_str(&read(&cfg.join("servers.json"))).unwrap_or_default();
    let review = cfg.join("import-review.json");
    if !servers.is_empty() {
        ob(&mut o, true, &format!("{} server(s) configured", servers.len()),
           &format!("{PROG} server list   ·   add more: {PROG} server import --from <src> / {PROG} server add '<url>'"));
    } else {
        let det = detect_import_sources(here);
        let cmds = if !det.is_empty() {
            let mut c = String::from("detected clients — accumulate each into one review file, edit it, then apply:");
            for s in &det {
                c.push_str(&format!("\n{PROG} server import --output {} --from {s}", review.display()));
            }
            c.push_str(&format!("\n# edit {} — keep the servers/subscriptions you want (each has a \"_source\"), then:\n{PROG} server import --apply --input {}",
                                review.display(), review.display()));
            c
        } else {
            format!("{PROG} server import --output <file> --from <shadowrocket|clash-verge|v2box|flclash>   (accumulate; repeat per client)\nedit <file>, then:  {PROG} server import --apply --input <file>\nor add directly:  {PROG} server add '<vless://|vmess://|anytls://|hysteria2://…>'  /  {PROG} sub add <url>")
        };
        ob(&mut o, false, "add servers", &cmds);
        pending += 1;
    }

    // 4. router up
    if lifecycle::host_running(ctx).is_some() {
        ob(&mut o, true, &format!("router running (mode {mode})"),
           &format!("{PROG} status   ·   {PROG} down to stop"));
    } else {
        ob(&mut o, false, "start the router (connect the CORP VPN first, other VPN app OFF)",
           &format!("{PROG} up"));
        pending += 1;
    }

    // 5. an escape server selected — not counted as pending: `up` picks one.
    let sel = ctx.sget("selected");
    if !sel.is_empty() && sel != "none" {
        ob(&mut o, true, &format!("escape server selected ({sel})"),
           &format!("{PROG} ping   (rank by latency)   ·   {PROG} use <tag>|auto"));
    } else {
        ob(&mut o, false, "pick an escape server",
           &format!("{PROG} ping   →   {PROG} use <tag>   (or {PROG} use auto)"));
    }

    // 6. the watchdog
    let plist = home.join(format!("Library/LaunchAgents/{}.plist", crate::watch::LABEL));
    if plist.is_file() {
        ob(&mut o, true, "auto-reload + recovery watcher installed",
           &format!("{PROG} watch status   ·   {PROG} watch install to re-sync"));
    } else {
        ob(&mut o, false, "install the auto-reload + recovery watcher", &format!("{PROG} watch install"));
    }

    // 7. shell integration — matched either as the pasted block or as the bare
    //    recommended line, mirroring exactly what `uninstall` strips.
    if rc_has_shell_init() {
        ob(&mut o, true, "shell aliases + tab-completion in your rc",
           &format!("eval \"$({PROG} shell-init)\"   (already in your rc)"));
    } else {
        ob(&mut o, false, "add shell aliases + tab-completion",
           &format!("{PROG} shell-init --install   (appends to ~/.zshrc; or add by hand: eval \"$({PROG} shell-init)\")"));
    }

    // 8. agent skill
    if home.join(".claude/skills/rowt").symlink_metadata().is_ok()
        || home.join(".agents/skills/rowt").symlink_metadata().is_ok()
    {
        ob(&mut o, true, "rowt agent skill linked (an agent can drive setup)",
           &format!("{PROG} skill status   ·   {PROG} skill install to re-link"));
    } else {
        ob(&mut o, false, "link the rowt agent skill (let an agent drive setup)",
           &format!("{PROG} skill install"));
    }

    // 9. corp lane — auto-discovered from the network's DHCP search domains
    let auto_dom = if env_or("ROWT_AUTO_CORP_DOMAINS", "1") == "1" {
        rowt_core::netdetect::parse(
            &Command::new("scutil").arg("--dns").stderr(Stdio::null()).output().ok()
                .map(|x| String::from_utf8_lossy(&x.stdout).into_owned()).unwrap_or_default(),
        ).physical_search.join(", ")
    } else {
        String::new()
    };
    if !auto_dom.is_empty() {
        ob(&mut o, true, &format!("corp auto-setup active (this network advertises {auto_dom})"),
           &format!("{PROG} corp suggest   ·   add extras: {PROG} corp add '*.corp.example.com' 10.0.0.0/8"));
    } else {
        ob(&mut o, false, "corp lane — auto-fills when you connect your work network/VPN",
           &format!("{PROG} corp suggest   (on corp LAN/VPN)   ·   {PROG} corp add '*.corp.example.com' 10.0.0.0/8"));
    }

    o.push(String::new());
    if lifecycle::host_running(ctx).is_some() {
        o.push("🎉 rowt is running — chosen sites via escape, corp intranet via the corp VPN, the rest direct.".into());
        o.push(format!("   Open a NEW terminal and run  '{PROG} monitor'  to watch connections/throughput/health live."));
    } else if pending == 0 {
        o.push(format!("✓ Core setup complete — run  '{PROG} up'  to start."));
    } else {
        o.push(format!("→ do the first unchecked [ ] above, then re-run '{PROG} onboard'."));
    }

    // The always-on reference, so this doubles as the guide.
    o.push(crate::help::onboard_reference(cfg));

    o.push(String::new());
    o.push(format!("More: {PROG} <command> --help   (per-command detail, version-current)"));
    for (f, note) in [("README.md", "(full user guide)"), ("DESIGN.md", "(how the routing works)")] {
        let p = here.join(f);
        if p.is_file() {
            o.push(format!("      {}   {note}", p.display()));
        }
    }
    o.push(format!("Config lives in: {}", cfg.display()));
    o.join("\n")
}
