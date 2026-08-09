//! Help routing — `help [cmd]`, `-h`/`--help`, and `<cmd> --help` for every arm.
//!
//! The text itself is not here: it is lifted out of bin/rowt at build time (see
//! build.rs) because those heredocs ARE the help, and a second copy would drift
//! within a release. What is here is the expansion of the `$VAR` markers the
//! shell's unquoted heredocs interpolate, and the same dispatch `show_help` does.

use std::path::Path;

const USAGE_HEAD: &str = include_str!(concat!(env!("OUT_DIR"), "/usage_head.txt"));
const USAGE_TAIL: &str = include_str!(concat!(env!("OUT_DIR"), "/usage_tail.txt"));
const REGISTRY: &str = include_str!(concat!(env!("OUT_DIR"), "/registry.txt"));
const DETAIL: &str = include_str!(concat!(env!("OUT_DIR"), "/help_detail.txt"));

/// The shell variables that appear inside the help heredocs, with the same
/// defaults bin/rowt gives them. Longest name first, so `$PORT` cannot eat the
/// front of `$PROG`-style neighbours and `$AUDIT_LOG` is not truncated to
/// `$AUDIT_L` + "OG" by a shorter key that happens to be a prefix.
fn vars(cfg: &Path) -> Vec<(&'static str, String)> {
    let c = cfg.display().to_string();
    let env = |k: &str, d: &str| -> String {
        std::env::var(k).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| d.to_string())
    };
    let mut v: Vec<(&'static str, String)> = vec![
        ("PROG", crate::PROG.to_string()),
        ("PORT", env("ROWT_PORT", "7890")),
        ("CLASH_PORT", env("ROWT_CLASH_PORT", "9090")),
        ("CFG", c.clone()),
        ("DOMAINS", format!("{c}/escape-domains.txt")),
        ("AUDIT_LOG", format!("{c}/log/audit.log")),
        ("AUDIT_MAX", env("ROWT_AUDIT_MAX", "5000")),
        ("WATCH_LOG", format!("{c}/log/watch.log")),
        ("WATCH_LABEL", "club.annaslife.rowt.watch".into()),
        ("WATCH_SUDOERS", "/etc/sudoers.d/rowt".into()),
        ("SINGBOX_VERSION", env("SINGBOX_VERSION", "1.13.14")),
        ("ROWT_VERSION", env!("ROWT_SHELL_VERSION").to_string()),
        ("FINAL_ROUTE", env("ROWT_FINAL", "direct")),
        ("DNS_DIRECT", env("ROWT_DNS_DIRECT", "223.5.5.5")),
        ("DNS_LOCAL", env("ROWT_DNS_LOCAL", "1.1.1.1")),
        ("AUTO_INTERVAL", env("ROWT_AUTO_INTERVAL", "20m")),
        ("SHELL", env("SHELL", "")),
    ];
    v.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
    v
}

fn expand(text: &str, cfg: &Path) -> String {
    let mut s = text.to_string();
    for (k, val) in vars(cfg) {
        s = s.replace(&format!("${k}"), &val);
    }
    s
}

/// The command table, from the `level@group@syntax@description` registry:
///
///     { if ($2!=g){ g=$2; printf "\n %s\n", g }
///       printf "  %s %s %-42s %s\n", mark($1), prog, $3, $4 }
///
/// The width is a BYTE width in awk. Every syntax field is ASCII, so padding by
/// chars agrees — but the mark glyphs are not, which is why they are printed
/// with `%s` on both sides rather than padded.
fn registry() -> String {
    let mut out = String::new();
    let mut group = String::new();
    for line in REGISTRY.lines() {
        let f: Vec<&str> = line.split('@').collect();
        if f.len() < 4 {
            continue;
        }
        if f[1] != group {
            group = f[1].to_string();
            out.push_str(&format!("\n {group}\n"));
        }
        let mark = match f[0] { "c" => "●", "o" => "◐", _ => "○" };
        out.push_str(&format!("  {mark} {} {:<42} {}\n", crate::PROG, f[2], f[3]));
    }
    out
}

/// One registry row: `level@group@syntax@description`.
pub fn reg_rows() -> impl Iterator<Item = (&'static str, &'static str, &'static str, &'static str)> {
    REGISTRY.lines().filter_map(|l| {
        let f: Vec<&str> = l.split('@').collect();
        (f.len() >= 4).then(|| (f[0], f[1], f[2], f[3]))
    })
}

/// The literal choice-sets inside a syntax field: `<a|b|c>` and `[a|b]`, but not
/// `<tag>` or `[--force]` — a group without a pipe is a placeholder, not a menu.
///
/// `grep -oE '[<[][^]>]*\|[^]>]*[]>]'`: because the body may contain neither `]`
/// nor `>`, a match always ends at the first closer after the opener. So the
/// scan is "opener, next closer, keep it if there is a pipe between" — and on a
/// miss the shell resumes one character along, not past the group.
pub fn choice_tokens(syntax: &str) -> Vec<String> {
    let b: Vec<char> = syntax.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == '<' || b[i] == '[' {
            if let Some(end) = (i + 1..b.len()).find(|&j| b[j] == ']' || b[j] == '>') {
                let body: String = b[i + 1..end].iter().collect();
                if body.contains('|') {
                    for tok in body.split('|') {
                        let t: String = tok.chars().filter(|c| !c.is_whitespace()).collect();
                        if !t.is_empty() && !t.starts_with("--") {
                            out.push(t);
                        }
                    }
                    i = end + 1;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

pub fn usage(cfg: &Path) -> String {
    format!("{}{}{}", expand(USAGE_HEAD, cfg), registry(), expand(USAGE_TAIL, cfg))
        .trim_end()
        .to_string()
}

pub enum Detail {
    Text(String),
    /// The page is built by shell logic, not just interpolation — render it there.
    Shell,
    Unknown,
}

/// `help_detail` — the per-command page.
pub fn detail(cfg: &Path, cmd: &str) -> Detail {
    for rec in DETAIL.split('\u{1e}') {
        let Some((pats, rest)) = rec.split_once('\u{1f}') else { continue };
        let Some((kind, text)) = rest.split_once('\u{1f}') else { continue };
        if pats.trim().split('|').any(|p| p.trim() == cmd) {
            return match kind {
                "shell" => Detail::Shell,
                _ => Detail::Text(expand(text, cfg).trim_end().to_string()),
            };
        }
    }
    Detail::Unknown
}

/// `show_help` — usage for no command or "help", the page for a known one, and
/// for anything else the shell's error-then-usage, which exits 1.
pub fn show(cfg: &Path, cmd: &str) -> Result<String, String> {
    if cmd.is_empty() || cmd == "help" {
        return Ok(usage(cfg));
    }
    match detail(cfg, cmd) {
        Detail::Text(d) => return Ok(d),
        Detail::Shell => crate::delegate(&["help".to_string(), cmd.to_string()]),
        Detail::Unknown => {}
    }
    // `err` goes to stderr, then usage to stdout, then rc 1 — reproduced as
    // written rather than tidied into one stream, because cli-diff compares
    // stdout and the exit status separately and would catch the tidying.
    eprintln!("error: unknown command: {cmd}");
    println!();
    println!("{}", usage(cfg));
    std::process::exit(1);
}
