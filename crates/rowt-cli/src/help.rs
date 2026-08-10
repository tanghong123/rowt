//! Help routing — `help [cmd]`, `-h`/`--help`, and `<cmd> --help` for every arm.
//!
//! The text itself is not here: it is lifted out of bin/rowt at build time (see
//! build.rs) because those heredocs ARE the help, and a second copy would drift
//! within a release. What is here is what an unquoted heredoc does to that text
//! — the backslash escapes, the `$VAR` expansions, and the two pages that are
//! genuinely computed — and the same dispatch `show_help` does.

use std::path::Path;

const USAGE_HEAD: &str = include_str!(concat!(env!("OUT_DIR"), "/usage_head.txt"));
const USAGE_TAIL: &str = include_str!(concat!(env!("OUT_DIR"), "/usage_tail.txt"));
const REGISTRY: &str = include_str!(concat!(env!("OUT_DIR"), "/registry.txt"));
const DETAIL: &str = include_str!(concat!(env!("OUT_DIR"), "/help_detail.txt"));

/// The shell variables that appear inside the help heredocs, with the same
/// defaults bin/rowt gives them.
fn vars(cfg: &Path) -> Vec<(&'static str, String)> {
    let c = cfg.display().to_string();
    let env = |k: &str, d: &str| -> String {
        std::env::var(k).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| d.to_string())
    };
    let v: Vec<(&'static str, String)> = vec![
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
    v
}

/// One left-to-right pass over an unquoted heredoc body: the backslash escapes,
/// the `$VAR` expansions, and — for the two pages that need it — `$1` and the
/// `$( … )` the shell would have run. `None` means a substitution this does not
/// understand, and the caller hands the page back to bin/rowt.
///
/// A single pass, not a sequence of `replace`s, because both orders of the
/// two-phase version are wrong on the same input. The shell-init page contains
/// `\$SHELL`, which bash prints as the literal `$SHELL`: expanding first leaves
/// a stray backslash in front of the VALUE, and unescaping first turns it into
/// a variable that then gets expanded. Only a pass that sees the backslash
/// before it sees the dollar gets it right.
fn render(text: &str, cfg: &Path, arg: &str) -> Option<String> {
    let b: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            // In an unquoted heredoc a backslash escapes only `$`, a backtick,
            // another backslash, and a newline (which it removes). In front of
            // anything else it is an ordinary character and stays.
            '\\' => match b.get(i + 1) {
                Some(c @ ('$' | '`' | '\\')) => { out.push(*c); i += 2 }
                Some('\n') => i += 2,
                _ => { out.push('\\'); i += 1 }
            },
            '$' => match b.get(i + 1) {
                Some('(') => {
                    let end = closing_paren(&b, i + 2)?;
                    let body: String = b[i + 2..end].iter().collect();
                    out.push_str(&substitute(&body, cfg, arg)?);
                    i = end + 1;
                }
                // `$1` is the command the page is being shown for. Only the
                // escape/corp page uses it — it is one page for two lanes.
                Some('1') => { out.push_str(arg); i += 2 }
                Some(c) if c.is_ascii_alphabetic() || *c == '_' => {
                    let mut j = i + 1;
                    while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == '_') {
                        j += 1;
                    }
                    let name: String = b[i + 1..j].iter().collect();
                    match vars(cfg).into_iter().find(|(k, _)| *k == name) {
                        Some((_, val)) => out.push_str(&val),
                        // Not one of ours: left as written rather than
                        // expanded to the empty string bash would give it, so
                        // a new marker in the shell shows up as itself.
                        None => out.push_str(&format!("${name}")),
                    }
                    i = j;
                }
                _ => { out.push('$'); i += 1 }
            },
            c => { out.push(c); i += 1 }
        }
    }
    Some(out)
}

/// The index of the `)` that closes a `$(` opened just before `from`, skipping
/// anything inside quotes. The corp conditional's message contains parentheses
/// — "(default 'corp'; …)" — so counting them naively ends the substitution in
/// the middle of a sentence.
fn closing_paren(b: &[char], from: usize) -> Option<usize> {
    let mut depth = 1usize;
    let mut i = from;
    let mut quote: Option<char> = None;
    while i < b.len() {
        let c = b[i];
        match quote {
            Some(q) => {
                if c == '\\' && q == '"' { i += 1 } else if c == q { quote = None }
            }
            None => match c {
                '\'' | '"' => quote = Some(c),
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            },
        }
        i += 1;
    }
    None
}

/// The inside of a `$( … )`, for the two shapes the help text actually uses:
///
///     lane_log <name>
///     [ "$1" = <word> ] && <emit> [ || <emit> ]
///
/// where `<emit>` is `echo <string>` or `printf '%s' <string>`. The two differ
/// only in a trailing newline, and command substitution strips those, so both
/// are just the string.
///
/// Deliberately not a shell. A shape this does not recognize returns `None` and
/// the page is rendered by bin/rowt, which is the one interpreter guaranteed to
/// agree with itself — better than a half-evaluator quietly printing something
/// close.
fn substitute(body: &str, cfg: &Path, arg: &str) -> Option<String> {
    let b = body.trim();
    if let Some(name) = b.strip_prefix("lane_log ") {
        let n = name.trim();
        let ok = !n.is_empty()
            && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        // `lane_log() { printf '%s/lane-%s.log' "$LOGDIR" "$1"; }`, LOGDIR=$CFG/log.
        return ok.then(|| format!("{}/log/lane-{n}.log", cfg.display()));
    }
    let rest = b.strip_prefix("[ \"$1\" = ")?;
    let (word, rest) = rest.split_once(" ] && ")?;
    let (yes, no) = match split_outside_quotes(rest, "||") {
        Some((y, n)) => (y, Some(n)),
        None => (rest, None),
    };
    let branch = if arg == word.trim() { Some(yes) } else { no };
    match branch {
        None => Some(String::new()),
        Some(cmd) => emit(cmd, cfg, arg),
    }
}

/// `echo "…"` / `printf '%s' "…"` — the argument, unquoted. A double-quoted one
/// is rendered again, since the shell would expand what is inside it.
fn emit(cmd: &str, cfg: &Path, arg: &str) -> Option<String> {
    let c = cmd.trim();
    let s = c.strip_prefix("echo ").or_else(|| c.strip_prefix("printf '%s' "))?.trim();
    let inner = &s[1..s.len().checked_sub(1)?];
    match s.chars().next()? {
        '\'' if s.ends_with('\'') => Some(inner.to_string()),
        '"' if s.ends_with('"') => render(inner, cfg, arg),
        _ => None,
    }
}

/// Split on the first `sep` that is not inside quotes. `||` between two `echo`
/// arguments separates them; one inside a message is part of the message.
fn split_outside_quotes<'a>(s: &'a str, sep: &str) -> Option<(&'a str, &'a str)> {
    let b: Vec<char> = s.chars().collect();
    let mut quote: Option<char> = None;
    let mut byte = 0usize;
    for (i, c) in b.iter().enumerate() {
        match quote {
            Some(q) => {
                if *c == q {
                    quote = None
                }
            }
            None => {
                if *c == '\'' || *c == '"' {
                    quote = Some(*c)
                } else if s[byte..].starts_with(sep) {
                    let _ = i;
                    return Some((&s[..byte], &s[byte + sep.len()..]));
                }
            }
        }
        byte += c.len_utf8();
    }
    None
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

/// Is this a command bin/rowt documents? The question `native()` asks about a
/// name it does not recognize: an unlisted one is a typo, and rowt-rs answers
/// those itself, while a listed one it has no arm for is an unported command
/// and still falls through to the shell (PORTING.md §6.6). Reading the registry
/// rather than a second list keeps the escape hatch honest — a command added to
/// the shell is covered by it the moment it is documented.
pub fn is_registered(cmd: &str) -> bool {
    reg_rows().any(|(_, _, syntax, _)| syntax.split_whitespace().next() == Some(cmd))
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

const ONBOARD_REF: &str = include_str!(concat!(env!("OUT_DIR"), "/onboard_ref.txt"));

/// The reference block `onboard` always prints after its checklist. Extracted
/// from bin/rowt like the rest of the help — it names commands, and a second
/// copy of a command list is a command list that goes stale.
pub fn onboard_reference(cfg: &Path) -> String {
    expand(ONBOARD_REF, cfg).trim_end().to_string()
}

/// A block with no command substitution in it. `render` cannot fail on one, and
/// falling back to the text as written beats printing nothing if that changes.
fn expand(text: &str, cfg: &Path) -> String {
    render(text, cfg, "").unwrap_or_else(|| text.to_string())
}

pub fn usage(cfg: &Path) -> String {
    format!("{}{}{}", expand(USAGE_HEAD, cfg), registry(), expand(USAGE_TAIL, cfg))
        .trim_end()
        .to_string()
}

pub enum Detail {
    Text(String),
    /// A substitution in the page that `render` does not understand — the one
    /// interpreter that certainly agrees with the shell is the shell.
    Shell,
    Unknown,
}

/// The extracted `help_detail` arms: patterns, `text`/`dyn`, and the heredoc.
fn records() -> impl Iterator<Item = (&'static str, &'static str, &'static str)> {
    DETAIL.split('\u{1e}').filter_map(|rec| {
        let (pats, rest) = rec.split_once('\u{1f}')?;
        let (kind, text) = rest.split_once('\u{1f}')?;
        Some((pats, kind, text))
    })
}

/// `help_detail` — the per-command page. `cmd` is also the page's `$1`: the
/// escape/corp arm is a single heredoc that describes whichever lane it was
/// asked about.
pub fn detail(cfg: &Path, cmd: &str) -> Detail {
    for (pats, _kind, text) in records() {
        if pats.trim().split('|').any(|p| p.trim() == cmd) {
            return match render(text, cfg, cmd) {
                Some(t) => Detail::Text(t.trim_end().to_string()),
                None => Detail::Shell,
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
        Detail::Text(d) => Ok(d),
        Detail::Shell => crate::delegate(&["help".to_string(), cmd.to_string()]),
        Detail::Unknown => unknown(cfg, cmd),
    }
}

/// `err "unknown command: $c"; echo; usage; exit 1` — the shell's answer both
/// for `help <nonsense>` and for running one.
///
/// Reproduced as written rather than tidied into one stream: cli-diff compares
/// stdout and the exit status separately, and would catch the tidying. Exiting
/// here rather than returning an error is also behavior — `run_command`'s
/// catch-all calls `exit 1` from inside the audited region, so the trail keeps
/// a BEGIN with no END for an unknown command, and that is what the log says
/// today.
pub fn unknown(cfg: &Path, cmd: &str) -> ! {
    eprintln!("error: unknown command: {cmd}");
    println!();
    println!("{}", usage(cfg));
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> &'static Path {
        Path::new("/tmp/rowt-help-test")
    }

    /// The classification is the contract between build.rs and `render`: a page
    /// marked `text` must not need an evaluator, and one marked `dyn` must
    /// actually be understood by the one we have. Both halves fail loudly here
    /// rather than as a page that prints its own source.
    #[test]
    fn every_page_renders_without_the_shell() {
        for (pats, kind, text) in records() {
            for p in pats.trim().split('|').map(str::trim) {
                let out = render(text, cfg(), p);
                assert!(out.is_some(), "help page `{p}` fell back to the shell");
                // Every escape is consumed. A page may legitimately still SHOW
                // a `$(` — that is the point of writing `\$(` — but nothing
                // should reach the reader with the backslash still attached.
                assert!(!out.unwrap().contains(r"\$"), "`{p}` printed an unprocessed escape");
            }
            if kind == "text" {
                assert!(!text.contains("$1"), "`{pats}` is marked static but uses $1");
            }
        }
    }

    /// The escape/corp page is one heredoc for two lanes, and the difference is
    /// the whole reason it needs evaluating.
    #[test]
    fn the_lane_page_says_something_different_for_each_lane() {
        let (Detail::Text(e), Detail::Text(c)) = (detail(cfg(), "escape"), detail(cfg(), "corp"))
        else {
            panic!("escape/corp did not render")
        };
        assert!(e.contains("domains that use the VPN tunnel"), "{e}");
        assert!(!e.contains("corp also accepts CIDRs"), "escape got corp's text:\n{e}");
        assert!(c.contains("domains/CIDRs sent into the corp VPN"), "{c}");
        assert!(c.contains("corp also accepts CIDRs"), "{c}");
        // The `printf` branch is a multi-line block with parentheses inside it,
        // which is what the quote-aware paren scan exists for.
        assert!(c.contains("sync [--iface I|<label>]"), "corp lost the sync section:\n{c}");
        assert!(!e.contains("sync [--iface"), "escape got corp's sync section:\n{e}");
    }

    #[test]
    fn the_block_page_names_its_log_through_lane_log() {
        let Detail::Text(b) = detail(cfg(), "block") else { panic!("block did not render") };
        assert!(b.contains("/tmp/rowt-help-test/log/lane-block.log"), "{b}");
    }

    /// `\$` is a literal dollar, not an expansion — three pages show the reader
    /// a command to type. Getting this wrong is invisible without a test or a
    /// cli-diff case, because the page still looks like a page.
    #[test]
    fn an_escaped_dollar_stays_a_dollar() {
        let Detail::Text(s) = detail(cfg(), "shell-init") else { panic!() };
        assert!(s.contains("by $SHELL"), "escaped $SHELL was expanded away:\n{s}");
        assert!(s.contains(r#"eval "$(rowt shell-init)""#), "{s}");
        assert!(!s.contains(r"\$"), "a backslash survived into the page:\n{s}");
    }

    /// `native()` routes an unrecognized name by this, so it decides between
    /// "print the usage" and "hand it to bash". The registry's syntax field
    /// leads with the command, and reading anything else out of it would send
    /// every typo to the shell — or, worse, answer an unported command.
    #[test]
    fn the_registry_knows_which_names_are_commands() {
        assert!(is_registered("status"));
        assert!(is_registered("escape"));
        assert!(is_registered("shell-init"));
        assert!(!is_registered("nosuchcommand"));
        // Hidden commands are not in the registry on purpose; `native` claims
        // them by name before the fallback is reached.
        assert!(!is_registered("_complete"));
        // Not a prefix match, and not the description's words either.
        assert!(!is_registered("stat"));
        assert!(!is_registered("domains"));
    }

    #[test]
    fn a_substitution_the_evaluator_does_not_know_goes_back_to_the_shell() {
        assert_eq!(render("a $(uname -a) b", cfg(), ""), None);
        assert_eq!(render(r"a \$(uname -a) b", cfg(), ""), Some("a $(uname -a) b".into()));
    }

    #[test]
    fn a_message_may_contain_the_characters_the_parser_looks_for() {
        // Parentheses inside the string, and a `||` that is text rather than a
        // branch — both end the substitution early if quotes are ignored.
        let t = r#"x$([ "$1" = a ] && echo "one (two) || three" || echo "no")y"#;
        assert_eq!(render(t, cfg(), "a"), Some("xone (two) || threey".into()));
        assert_eq!(render(t, cfg(), "b"), Some("xnoy".into()));
    }
}
