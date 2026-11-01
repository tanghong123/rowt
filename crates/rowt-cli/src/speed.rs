//! `rowt speed` — sustained throughput per lane, proxied vs bypassed.
//!
//! `cmd_speed` and its helpers (`_speed_one`, `_speed_cell`, `_speed_verdict`,
//! `_speed_pad`, `_speed_human`) from bin/rowt, reproduced rather than tidied:
//! the shell's table, its roll-up and its wording are the specification, and
//! `parity cli-diff` compares them byte for byte over canned curl answers.
//! A diagnosis, never a gate: whatever the lanes measure, the exit status is 0.

use crate::lifecycle::{self, Ctx};
use crate::{die, PROG};
use std::process::{Command, Stdio};

/// `SPEED_URLS_DEFAULT` — one target per geography, each answering bypassed
/// too, so every row has a control (see the shell for why these three).
const URLS_DEFAULT: &str = "https://mirrors.aliyun.com/ubuntu/ls-lR.gz https://ash-speed.hetzner.com/100MB.bin https://speed.cloudflare.com/__down?bytes=10000000";

fn usage() -> String {
    format!("usage: {PROG} speed [--repeat N] [url …]")
}

pub fn cmd_speed(ctx: &Ctx, args: &[String]) -> Result<String, String> {
    if lifecycle::host_running(ctx).is_none() {
        die(&ctx.cfg, &format!("router is not running — '{PROG} up' first"));
    }
    // Flags BEFORE the URL list, as the shell's loop takes them.
    let mut repeat = "1".to_string();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--repeat" {
            if i + 1 >= args.len() {
                die(&ctx.cfg, &format!("--repeat needs a number — {}", usage()));
            }
            repeat = args[i + 1].clone();
            i += 2;
        } else if let Some(v) = a.strip_prefix("--repeat=") {
            repeat = v.to_string();
            i += 1;
        } else if a == "--" {
            i += 1;
            break;
        } else if a.starts_with('-') {
            die(&ctx.cfg, &format!("unknown flag '{a}' — {}", usage()));
        } else {
            break;
        }
    }
    if repeat.is_empty() || !repeat.bytes().all(|b| b.is_ascii_digit()) {
        die(&ctx.cfg, &format!("--repeat wants a positive integer, got '{repeat}'"));
    }
    let n: u64 = repeat.parse().unwrap_or(u64::MAX);
    if n < 1 {
        die(&ctx.cfg, &format!("--repeat wants a positive integer, got '{repeat}'"));
    }
    let mut urls: Vec<String> = args[i..].to_vec();
    if urls.is_empty() {
        // Split on whitespace, so ROWT_SPEED_URL can name a default SET.
        let set = std::env::var("ROWT_SPEED_URL").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| URLS_DEFAULT.into());
        urls = set.split_whitespace().map(String::from).collect();
    }
    let bytes = lifecycle::env_or("ROWT_SPEED_BYTES", "3000000");
    let timeout = lifecycle::env_or("ROWT_SPEED_TIMEOUT", "8");
    let b: f64 = bytes.parse().unwrap_or(0.0);
    let cap = if b < 1_000_000.0 { format!("{} KB", (b / 1000.0) as i64) } else { format!("{} MB", fmt_g(b / 1_000_000.0)) };
    eprintln!(
        "==> {} fetches — {} url(s) x {repeat} x2 (through rowt, then bypassed), up to {cap} and {timeout}s each; a stalled lane shows as a small number, not an error",
        urls.len() as u64 * n * 2,
        urls.len()
    );
    let mut out: Vec<String> = vec![String::new()];
    out.push(format!("  {} {} {} LANE", pad("URL", 42), pad("VIA ROWT", 12), pad("BYPASSED", 12)));

    let lanes = crate::load_lanes(&ctx.cfg);
    let final_route = lifecycle::env_or("ROWT_FINAL", "direct");
    // lane, proxied B/s, bypassed B/s, ratio (-1: no ratio)
    let mut rows: Vec<(String, i64, i64, i64)> = Vec::new();
    for u in &urls {
        let host = host_of(u);
        let lane = match rowt_core::classify::longest_domain_hit(&host, &lanes.escape, &lanes.corp, &lanes.block) {
            Some((l, _, _)) => l.as_str().to_string(),
            None => final_route.clone(),
        };
        let p = cell(ctx, u, false, n, &bytes, &timeout);
        let y = cell(ctx, u, true, n, &bytes, &timeout);
        let shown: String = u.chars().take(42).collect();
        out.push(format!("  {} {} {} {lane}", pad(&shown, 42), pad(&human(p.median), 12), pad(&human(y.median), 12)));
        if p.ok < p.n || y.ok < y.n {
            out.push(format!(
                "    · completed {}/{} through rowt, {}/{} bypassed — failed samples are excluded from the median, not counted as zero",
                p.ok, p.n, y.ok, y.n
            ));
        }
        out.push(format!("    {}", verdict(p.median, y.median, &p.code, &y.code)));
        let ratio = if p.median > 0 && y.median > 0 { round0(100.0 * p.median as f64 / y.median as f64) } else { -1 };
        rows.push((lane, p.median, y.median, ratio));
    }
    if urls.len() > 1 {
        out.push(String::new());
        out.push("  per lane — the tunnel as a fraction of the same fetch bypassed:".into());
        let mut lanelist: Vec<String> = rows.iter().map(|r| r.0.clone()).collect();
        lanelist.sort();
        lanelist.dedup();
        for l in &lanelist {
            let mine: Vec<&(String, i64, i64, i64)> = rows.iter().filter(|r| &r.0 == l).collect();
            let mut ratios: Vec<i64> = mine.iter().map(|r| r.3).filter(|r| *r >= 0).collect();
            ratios.sort();
            let mut tun: Vec<i64> = mine.iter().map(|r| r.1).filter(|t| *t > 0).collect();
            tun.sort();
            let rmed = if ratios.is_empty() { -1 } else { ratios[(ratios.len() + 1) / 2 - 1] };
            let mut spread = String::new();
            if let (Some(lo), Some(hi)) = (ratios.first(), ratios.last()) {
                if lo != hi {
                    spread.push_str(&format!(", ratios {lo}%–{hi}%"));
                }
            }
            match (tun.first(), tun.last()) {
                (Some(lo), Some(hi)) if lo != hi => spread.push_str(&format!(", tunnel {}–{}", human(*lo), human(*hi))),
                (Some(lo), _) => spread.push_str(&format!(", tunnel {}", human(*lo))),
                _ => {}
            }
            out.push(format!("    {} {} url(s){spread}", pad(l, 8), mine.len()));
            if rmed >= 0 {
                out.push(format!("      {}", verdict(rmed, 100, "200", "200")));
            } else {
                out.push("      · no verdict — no URL on this lane completed both fetches".into());
            }
            if l == "direct" {
                out.push("      · both columns are the same physical path for this lane, so this".into());
                out.push("        measures rowt's own overhead — it is not one lane against another.".into());
            }
        }
        if lanelist.len() <= 1 {
            out.push("    · every URL here routed the same way, so there are no two lanes to".into());
            out.push(format!("      compare. Add one your rules send the other way — '{PROG} route <host>'"));
            out.push("      tells you where a host lands before you spend a fetch on it.".into());
        }
    }
    out.push(String::new());
    for l in [
        "  VIA ROWT is the lane your rules chose for that host; BYPASSED is the same",
        "  fetch straight out the physical NIC, which IS the direct path. So on an",
        "  escape-routed URL the two columns are already the tunnel against direct.",
        "  A lane can pass every other rowt check and still fail here: reachability",
        "  probes use small responses and cannot see a throughput collapse.",
        "  Below 50% of the bypassed column is called out as rowt's own bottleneck.",
        "  A single lane's repeats vary by ~1.4x, so a tighter line would report noise;",
        "  '--repeat N' takes the MEDIAN of N samples per cell when one run is not enough.",
        "  rowt never refuses or re-routes a lane for being slow: this is a diagnosis,",
        "  and it never changes the exit status.",
    ] {
        out.push(l.into());
    }
    Ok(out.join("\n"))
}

/// The host the LANE column is predicted for: scheme, path and port gone,
/// lowercased — `sed -E 's#^[a-zA-Z]+://##; s#/.*$##; s#:[0-9]+$##'` + `tr`.
fn host_of(u: &str) -> String {
    let mut s = u;
    if let Some(p) = s.find("://") {
        if p > 0 && s[..p].bytes().all(|b| b.is_ascii_alphabetic()) {
            s = &s[p + 3..];
        }
    }
    if let Some(p) = s.find('/') {
        s = &s[..p];
    }
    if let Some(p) = s.rfind(':') {
        if p + 1 < s.len() && s[p + 1..].bytes().all(|b| b.is_ascii_digit()) {
            s = &s[..p];
        }
    }
    s.to_ascii_lowercase()
}

struct Cell {
    median: i64,
    ok: u64,
    n: u64,
    code: String,
}

/// `_speed_one`: "<bytes> <B/s> <code>" for one fetch, `0 0 000` when curl
/// printed nothing. Same argv as the shell's, which the argv trace compares.
fn one(ctx: &Ctx, url: &str, bypass: bool, bytes: &str, timeout: &str) -> String {
    let range = format!("0-{bytes}");
    let mut c = Command::new("curl");
    c.arg("-sS");
    if bypass {
        c.args(["--noproxy", "*"]);
    } else {
        c.args(["-x", &format!("http://127.0.0.1:{}", ctx.port)]);
    }
    c.args(["-o", "/dev/null", "-r", &range, "-w", "%{size_download} %{speed_download} %{http_code}"]);
    c.args(["--connect-timeout", "5", "-m", timeout, url]);
    let out = c.stdin(Stdio::null()).stderr(Stdio::null()).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    // `$(…)` drops trailing newlines; `${out:-0 0 000}` stands in for nothing.
    let out = out.trim_end_matches('\n').to_string();
    if out.is_empty() { "0 0 000".into() } else { out }
}

/// `_speed_cell`: N samples reduced to the median of the ones that completed,
/// with how many did. A failure is excluded, never folded in as zero.
fn cell(ctx: &Ctx, url: &str, bypass: bool, n: u64, bytes: &str, timeout: &str) -> Cell {
    let mut vals: Vec<i64> = Vec::new();
    let mut code = "000".to_string();
    for _ in 0..n {
        let s = one(ctx, url, bypass, bytes, timeout);
        // `read -r _sz _bps _code` — the first line, split on blanks; the last
        // field takes the rest of the line.
        let line = s.lines().next().unwrap_or("");
        let mut f = line.split_whitespace();
        let (_sz, bps, c) = (f.next(), f.next().unwrap_or(""), f.collect::<Vec<_>>().join(" "));
        if c.is_empty() || c == "000" {
            continue;
        }
        let bps = bps.split('.').next().unwrap_or("");
        if let Ok(v) = bps.parse::<i64>() {
            if v > 0 {
                vals.push(v);
                code = c;
            }
        }
    }
    if vals.is_empty() {
        return Cell { median: 0, ok: 0, n, code: "000".into() };
    }
    vals.sort();
    Cell { median: vals[(vals.len() + 1) / 2 - 1], ok: vals.len() as u64, n, code }
}

/// `_speed_verdict`: one ratio, no absolute floor, decided on the number that
/// gets printed.
fn verdict(p: i64, b: i64, pc: &str, bc: &str) -> String {
    if p <= 0 || pc == "000" {
        return format!("· no verdict — the fetch THROUGH rowt did not complete (HTTP {pc})");
    }
    if b <= 0 || bc == "000" {
        return format!("· no verdict — the BYPASSED control did not complete (HTTP {bc}), so there is nothing to compare against");
    }
    let r = round0(100.0 * p as f64 / b as f64);
    if r < 50 {
        format!("⚠ rowt is the bottleneck — {r}% of what the same fetch does with the proxy bypassed")
    } else {
        format!("✓ not rowt — {r}% of the bypassed control, so the path itself is the limit")
    }
}

/// awk's `sprintf("%.0f", x)`: C rounding, ties to even.
fn round0(x: f64) -> i64 {
    format!("{x:.0}").parse().unwrap_or(0)
}

/// `_speed_human`: coarse on purpose — a triage signal, not a benchmark.
fn human(b: i64) -> String {
    if b <= 0 {
        "—".into()
    } else if b < 1024 {
        format!("{b} B/s")
    } else if b < 1_048_576 {
        format!("{:.0} KB/s", b as f64 / 1024.0)
    } else {
        format!("{:.1} MB/s", b as f64 / 1_048_576.0)
    }
}

/// `_speed_pad`: pad to a width by `${#t}`, which bash counts in CHARACTERS
/// under a UTF-8 locale and in BYTES under C — so the em dash of a dead cell
/// pads the way the shell's table does in either.
fn pad(t: &str, w: usize) -> String {
    let len = if utf8_locale() { t.chars().count() } else { t.len() };
    format!("{t}{}", " ".repeat(w.saturating_sub(len)))
}

fn utf8_locale() -> bool {
    let v = ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .unwrap_or_default()
        .to_ascii_lowercase();
    v.contains("utf-8") || v.contains("utf8")
}

/// awk's `%g`: six significant digits, trailing zeros dropped.
fn fmt_g(x: f64) -> String {
    if x == 0.0 {
        return "0".into();
    }
    let e = x.abs().log10().floor() as i32;
    if !(-4..6).contains(&e) {
        let s = format!("{:.5e}", x);
        let (m, ex) = s.split_once('e').unwrap_or((&s, "0"));
        let m = if m.contains('.') { m.trim_end_matches('0').trim_end_matches('.') } else { m };
        let ex: i32 = ex.parse().unwrap_or(0);
        return format!("{m}e{}{:02}", if ex < 0 { '-' } else { '+' }, ex.abs());
    }
    let s = format!("{:.*}", (5 - e).max(0) as usize, x);
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_lose_scheme_path_and_port_only() {
        assert_eq!(host_of("https://Mirrors.Example.com/ubuntu/x.gz"), "mirrors.example.com");
        assert_eq!(host_of("http://a.example:8080/x"), "a.example");
        assert_eq!(host_of("https://speed.example/__down?bytes=1"), "speed.example");
        assert_eq!(host_of("a.example:99"), "a.example");
    }

    #[test]
    fn the_shells_number_formats() {
        assert_eq!(human(0), "—");
        assert_eq!(human(700), "700 B/s");
        assert_eq!(human(900_000), "879 KB/s");
        assert_eq!(human(2_500_000), "2.4 MB/s");
        assert_eq!(fmt_g(3.0), "3");
        assert_eq!(fmt_g(2.5), "2.5");
        assert_eq!(fmt_g(10.0), "10");
        // C's %.0f, ties to even — the 50% line is decided on these.
        assert_eq!(round0(49.5), 50);
        assert_eq!(round0(48.5), 48);
        assert_eq!(round0(49.4), 49);
    }
}
