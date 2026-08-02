//! Design tokens — the truecolor palettes from the handoff (`COLORS.md`). Two
//! themes, one contract: same layout, same glyphs, same control behaviour; only
//! the color values change. Every **dark** value is copied from the byte-exact
//! `.ansi` renders; do not drift it.
//!
//! A terminal owns its background, so the TUI paints **foregrounds only** plus the
//! one small selection cell background — the handoff's "mock-only surfaces" exist
//! for the HTML prototype and must never reach a cell. Nothing here stores a
//! background color; `emphasize`/`fade` know only which *direction* the terminal's
//! background lies in, from the active theme.

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use ratatui::style::{Color, Modifier, Style};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Theme {
    Dark,
    Light,
}

/// One theme's token values. Field names are the handoff's token names, and every
/// one of them has at least one render site — the palette carries no spares.
pub struct Palette {
    pub bright: Color,     // every value that isn't a label and isn't a state
    pub dim: Color,        // secondary text / lane values / help prose
    pub dimmer: Color,     // labels, table headers
    pub border: Color,     // frame, box-drawing, rules
    pub escape: Color,     // escape lane (logo blue-purple)
    pub corp: Color,       // corp lane (teal)
    pub direct: Color,     // direct lane (green)
    pub block: Color,      // block lane / blocked errors (purple)
    pub up: Color,         // upload arrow/rate; also `transient` and `armed`
    pub down: Color,       // download arrow/rate
    pub persistent: Color, // one red for "this is failing": persistent errors,
    // DOWN servers, and worst-tier latency
    pub up_table: Color,   // ↑ inside the connections table (de-emphasized)
    pub down_table: Color, // ↓ inside the connections table (de-emphasized)
    pub lat_ok: Color,     // < 70 ms
    pub lat_warn: Color,   // < 140 ms
    pub lat_bad: Color,    // >= 140 ms (= `persistent`)
    /// The only cell background the TUI paints (selected row / selected chip).
    pub selection_bg: Color,
}
// Five tokens from the handoff are deliberately absent, their roles folded into a
// survivor — a terminal cell is not a CSS box, and each was an affordance that
// doesn't survive the translation:
//   rule    -> border      a 1.2:1 hairline is a CSS trick; a terminal draws a
//                          rule as a full `─` cell row, which at 1.2:1 on light
//                          reads as a rendering fault. One frame color.
//   dimmest -> dimmer      three grey steps is one too many at a single terminal
//                          font size, and on light it lands at 3.4:1 — under AA.
//   body    -> dim         only the help overlay wanted it; `dim` reads fine as
//                          prose in both themes (5.6:1 dark, 7.4:1 light).
//   refused -> persistent  mislabeled upstream: #d3788c was never an error kind.
//                          One red means "this is failing".
//   value   -> bright      a fourth text tier that only one cell ever used. The
//                          band reads as three kinds of thing — label, state,
//                          and everything else — and "everything else" is one
//                          weight. (`draw_identity` spells the rule out.)

const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

pub const DARK: Palette = Palette {
    bright: rgb(0xe9ecf3),
    dim: rgb(0x8b90a4),
    dimmer: rgb(0x656a82),
    border: rgb(0x4c5064),
    escape: rgb(0x7c9df0),
    corp: rgb(0x56c7be),
    direct: rgb(0x86c07a),
    block: rgb(0xa98ad6),
    up: rgb(0xe0a35e),
    down: rgb(0x56c7be),
    persistent: rgb(0xe0655e),
    up_table: rgb(0xc9a06e),
    down_table: rgb(0x6fb8b0),
    lat_ok: rgb(0x86c07a),
    lat_warn: rgb(0xe0a35e),
    lat_bad: rgb(0xe0655e),
    selection_bg: rgb(0x262b3e), // dark slate; pairs with an emphasized fg
};

/// Tuned for a near-paper background (relative luminance >= `LIGHT_MIN_LUMA`);
/// `resolve` falls back to `DARK` for anything dimmer. Each hue is its dark
/// counterpart rotated <= 6° and dropped in lightness, so a lane reads as the
/// same color in both themes.
pub const LIGHT: Palette = Palette {
    bright: rgb(0x14161d),
    dim: rgb(0x4a4f63),
    dimmer: rgb(0x6a708a),
    border: rgb(0xa9aec0),
    escape: rgb(0x3546b8),
    corp: rgb(0x0b6b67),
    direct: rgb(0x2f6b23),
    block: rgb(0x6d43ad),
    up: rgb(0x8a5208),
    down: rgb(0x0b6b67),
    persistent: rgb(0xb32a22),
    up_table: rgb(0xa8721f),
    down_table: rgb(0x2d817c),
    lat_ok: rgb(0x2f6b23),
    lat_warn: rgb(0x8a5208),
    lat_bad: rgb(0xb32a22),
    // The handoff's unfocused selection is rgba(20,25,50,.05) over the screen
    // surface; composited here, since a terminal can't blend cell backgrounds.
    selection_bg: rgb(0xe8eaed),
};

// The active theme, set once at startup — `Dark` until then, so `--render` and
// the golden tests always see the frozen capture's palette.
static ACTIVE: AtomicU8 = AtomicU8::new(0);

pub fn set(t: Theme) {
    ACTIVE.store(t as u8, Ordering::Relaxed);
}

pub fn current() -> Theme {
    if ACTIVE.load(Ordering::Relaxed) == 1 {
        Theme::Light
    } else {
        Theme::Dark
    }
}

pub fn palette() -> &'static Palette {
    match current() {
        Theme::Dark => &DARK,
        Theme::Light => &LIGHT,
    }
}

// Token accessors, one per field — `Palette` carries no spares, so this list and
// the struct stay in lockstep. `transient`/`blocked`/`armed`/`border_focus` are
// named roles that resolve onto one of them.
pub fn bright() -> Color {
    palette().bright
}
pub fn dim() -> Color {
    palette().dim
}
pub fn dimmer() -> Color {
    palette().dimmer
}
pub fn border() -> Color {
    palette().border
}
pub fn escape() -> Color {
    palette().escape
}
pub fn corp() -> Color {
    palette().corp
}
pub fn direct() -> Color {
    palette().direct
}
pub fn block() -> Color {
    palette().block
}
pub fn up() -> Color {
    palette().up
}
pub fn down() -> Color {
    palette().down
}
pub fn up_table() -> Color {
    palette().up_table
}
pub fn down_table() -> Color {
    palette().down_table
}
pub fn persistent() -> Color {
    palette().persistent
}
pub fn transient() -> Color {
    up()
}
pub fn blocked() -> Color {
    block()
}
/// Amber accent for the control layer: a locked/frozen selection and the
/// press-again-to-confirm bar (CONTROLS.md §4.2, §5.2).
pub fn armed() -> Color {
    up()
}
/// The focused pane's "cursor" — its border, lifted off the frame.
///
/// Dark slams to `bright`: maximum lift off a dark frame, which reads as emitted
/// light. The same treatment on paper reads as a smear (a heavy black slab drawn
/// around the pane), so light takes a **single-weight step** to `dim` instead —
/// unmistakably focused against `border`'s 2.0:1, without the slab. That's the
/// terminal form of the handoff's "1px accent + soft halo, not 2px + inset glow":
/// a cell has one weight, so the softening has to land in the color step.
pub fn border_focus() -> Color {
    match current() {
        Theme::Dark => bright(),
        Theme::Light => dim(),
    }
}
/// Subtle background for the selected row / chip (the only bg the TUI paints).
pub fn selection_bg() -> Color {
    palette().selection_bg
}

/// Latency color by threshold: green < 70, orange < 140, red >= 140.
pub fn latency_color(ms: u32) -> Color {
    let p = palette();
    if ms < 70 {
        p.lat_ok
    } else if ms < 140 {
        p.lat_warn
    } else {
        p.lat_bad
    }
}

pub fn fg(c: Color) -> Style {
    Style::default().fg(c)
}

pub fn bold(c: Color) -> Style {
    Style::default().fg(c).add_modifier(Modifier::BOLD)
}

/// Blend `c` toward `t` by fraction `f` (0 = unchanged, 1 = fully `t`).
/// Non-RGB passes through.
pub fn mix(c: Color, t: Color, f: f32) -> Color {
    let f = f.clamp(0.0, 1.0);
    match (c, t) {
        (Color::Rgb(r, g, b), Color::Rgb(tr, tg, tb)) => {
            let m = |v: u8, tv: u8| (v as f32 + (tv as f32 - v as f32) * f).round() as u8;
            Color::Rgb(m(r, tr), m(g, tg), m(b, tb))
        }
        _ => c,
    }
}

/// Scale an RGB color's brightness by `f` (clamped 0..1). Non-RGB passes through.
pub fn scale(c: Color, f: f32) -> Color {
    let f = f.clamp(0.0, 1.0);
    match c {
        Color::Rgb(r, g, b) => Color::Rgb(
            (r as f32 * f).round() as u8,
            (g as f32 * f).round() as u8,
            (b as f32 * f).round() as u8,
        ),
        other => other,
    }
}

/// A smooth pulse intensity in [0.5, 1.0] from wall-clock seconds — a sine so
/// the `● LIVE` dot breathes to show sampling is live. Time-based (not tick-
/// based) so the cadence is steady regardless of redraw timing.
pub fn pulse(secs: f32) -> f32 {
    const PERIOD: f32 = 2.6; // seconds per breath
    let s = ((secs / PERIOD) * std::f32::consts::TAU).sin(); // -1..1
    0.75 + 0.25 * s // 0.5..1.0
}

/// Lighten an RGB color toward white by fraction `f`.
pub fn brighten(c: Color, f: f32) -> Color {
    mix(c, Color::Rgb(255, 255, 255), f)
}

/// Darken an RGB color toward black by fraction `f`.
pub fn darken(c: Color, f: f32) -> Color {
    mix(c, Color::Rgb(0, 0, 0), f)
}

/// Push a foreground *away* from the theme's background by fraction `f` — the
/// selected-row / hover lift. Brightening (which lifts text off a dark terminal)
/// would wash the same text out on paper, so light darkens instead.
pub fn emphasize(c: Color, f: f32) -> Color {
    match current() {
        Theme::Dark => brighten(c, f),
        Theme::Light => darken(c, f),
    }
}

/// Sink a foreground *toward* the terminal's background, keeping fraction `f` of
/// it — the `● LIVE` dot's breathing pulse. On dark that's the plain brightness
/// scale the capture was rendered with; on light it fades toward white instead,
/// so the dot dims rather than hardening to black. White, not the mock's screen
/// color: the terminal owns its background and we never assume a specific one.
pub fn fade(c: Color, f: f32) -> Color {
    match current() {
        Theme::Dark => scale(c, f),
        Theme::Light => mix(c, Color::Rgb(255, 255, 255), 1.0 - f),
    }
}

// ---------------- theme selection ----------------

/// What the operator asked for on the command line (`--theme`), or `Auto`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThemeArg {
    Dark,
    Light,
    Auto,
}

impl ThemeArg {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "dark" => Some(ThemeArg::Dark),
            "light" => Some(ThemeArg::Light),
            "auto" => Some(ThemeArg::Auto),
            _ => None,
        }
    }
}

/// The light tokens are tuned for a near-paper background (L* >= 92, i.e.
/// `#eaeaea`–`#ffffff`, warm paper included). Below this relative luminance their
/// contrast ratios degrade, so a dim-but-not-black background stays on dark.
pub const LIGHT_MIN_LUMA: f32 = 0.75;

/// How long to wait for the terminal to answer an OSC 11 query before giving up.
const OSC11_TIMEOUT: Duration = Duration::from_millis(100);

/// Decide the theme for this run. Explicit beats detected; detection reads the
/// terminal's actual background — `COLORFGBG` first, then an OSC 11 query — and
/// falls back to dark. `$TERM` is never consulted: it says which escape codes the
/// terminal understands, not what color the operator painted it.
pub fn resolve(arg: ThemeArg) -> Theme {
    match arg {
        ThemeArg::Dark => Theme::Dark,
        ThemeArg::Light => Theme::Light,
        ThemeArg::Auto => detect().unwrap_or(Theme::Dark),
    }
}

fn detect() -> Option<Theme> {
    if let Some(t) = std::env::var("COLORFGBG").ok().and_then(|v| from_colorfgbg(&v)) {
        return Some(t);
    }
    from_osc11(OSC11_TIMEOUT)
}

/// `COLORFGBG` is `fg;bg`, sometimes with a middle field — only the last one is
/// the background. A non-numeric value (`default`, which some terminals emit) is
/// *no answer*, not a guess: the caller falls through to OSC 11.
pub fn from_colorfgbg(v: &str) -> Option<Theme> {
    let bg: u16 = v.rsplit(';').next()?.trim().parse().ok()?;
    // ANSI 0–6 and 8 are the dark backgrounds; 7 and 9–15 are the light ones.
    Some(if bg == 7 || (9..=15).contains(&bg) { Theme::Light } else { Theme::Dark })
}

/// WCAG relative luminance of an sRGB triple (0..=1).
pub fn relative_luminance(r: u8, g: u8, b: u8) -> f32 {
    let lin = |v: u8| {
        let c = v as f32 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

/// Parse an OSC 11 reply — `ESC ] 11 ; rgb:RRRR/GGGG/BBBB` terminated by BEL or
/// ST. Components are 1–4 hex digits each, scaled to 8 bits by their own width,
/// so `rgb:f/f/f`, `rgb:ff/ff/ff` and `rgb:ffff/ffff/ffff` all read as white.
pub fn parse_osc11(reply: &str) -> Option<(u8, u8, u8)> {
    let body = reply.split("rgb:").nth(1)?;
    let body = body.split(['\x07', '\x1b']).next()?;
    let mut it = body.split('/');
    let comp = |s: Option<&str>| -> Option<u8> {
        let s = s?.trim();
        if s.is_empty() || s.len() > 4 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let v = u32::from_str_radix(s, 16).ok()?;
        let max = 16u32.pow(s.len() as u32) - 1;
        Some(((v * 255 + max / 2) / max) as u8)
    };
    Some((comp(it.next())?, comp(it.next())?, comp(it.next())?))
}

/// Whether what's arrived so far can still be the head of an `ESC ] 11 ;` reply.
/// The probe reads from the same tty the input loop reads next, so it stops the
/// instant this goes false — a terminal that ignores the query must cost the
/// operator one keystroke, not a drained paste.
pub fn could_be_osc11(buf: &[u8]) -> bool {
    buf.iter().zip(b"\x1b]11;").all(|(a, b)| a == b)
}

/// The theme a reported background implies, applying the near-paper guard.
pub fn theme_for_bg(r: u8, g: u8, b: u8) -> Theme {
    if relative_luminance(r, g, b) >= LIGHT_MIN_LUMA {
        Theme::Light
    } else {
        Theme::Dark
    }
}

/// Ask the terminal what its background is (OSC 11) and decide by luminance.
///
/// The query goes to `/dev/tty`, not stdin/stdout, so a piped or redirected
/// stream can neither swallow the request nor block the read; the fd is opened
/// non-blocking and polled to a deadline, so an unanswering terminal costs
/// `timeout` once at startup and never a wedged reader thread. Requires raw mode
/// to already be on — otherwise the reply is line-buffered and echoed.
///
/// Reading is **one byte at a time**, and stops the moment what's arrived can no
/// longer be an `ESC ] 11 ;` reply or the terminator lands. That matters because
/// this fd is the same one the input loop reads next: anything consumed here is
/// gone. Type-ahead at startup is rare, but a terminal that simply ignores the
/// query must cost the operator at most a single keystroke — never a drained
/// paste or a swallowed mouse burst.
#[cfg(unix)]
fn from_osc11(timeout: Duration) -> Option<Theme> {
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;

    const MAX: usize = 64; // a real reply is ~25 bytes

    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open("/dev/tty")
        .ok()?;
    tty.write_all(b"\x1b]11;?\x07").ok()?;
    tty.flush().ok()?;

    let fd = tty.as_raw_fd();
    let deadline = Instant::now() + timeout;
    let mut buf: Vec<u8> = Vec::with_capacity(MAX);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || buf.len() >= MAX {
            return None;
        }
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        // SAFETY: one well-formed pollfd over a fd we own, with a bounded timeout.
        if unsafe { libc::poll(&mut pfd, 1, left.as_millis() as libc::c_int) } <= 0 {
            return None;
        }
        let mut b = [0u8; 1];
        match tty.read(&mut b) {
            Ok(1) => buf.push(b[0]),
            Ok(_) => return None,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(_) => return None,
        }
        // Not the reply we asked for — stop before eating any more input.
        if !could_be_osc11(&buf) {
            return None;
        }
        // Terminated by BEL or ST; stop exactly there so nothing past it is taken.
        if b[0] == 0x07 || buf.ends_with(b"\x1b\\") {
            let (r, g, b) = parse_osc11(&String::from_utf8_lossy(&buf))?;
            return Some(theme_for_bg(r, g, b));
        }
    }
}

#[cfg(not(unix))]
fn from_osc11(_timeout: Duration) -> Option<Theme> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dark column is the frozen capture. If this drifts, the goldens lie.
    #[test]
    fn dark_palette_matches_the_frozen_capture() {
        assert_eq!(DARK.bright, Color::Rgb(233, 236, 243));
        assert_eq!(DARK.dim, Color::Rgb(139, 144, 164));
        assert_eq!(DARK.dimmer, Color::Rgb(101, 106, 130));
        assert_eq!(DARK.border, Color::Rgb(76, 80, 100));
        assert_eq!(DARK.escape, Color::Rgb(124, 157, 240));
        assert_eq!(DARK.corp, Color::Rgb(86, 199, 190));
        assert_eq!(DARK.direct, Color::Rgb(134, 192, 122));
        assert_eq!(DARK.block, Color::Rgb(169, 138, 214));
        assert_eq!(DARK.up, Color::Rgb(224, 163, 94));
        assert_eq!(DARK.down, Color::Rgb(86, 199, 190));
        assert_eq!(DARK.persistent, Color::Rgb(224, 101, 94));
        assert_eq!(DARK.up_table, Color::Rgb(201, 160, 110));
        assert_eq!(DARK.down_table, Color::Rgb(111, 184, 176));
        assert_eq!(DARK.lat_ok, Color::Rgb(134, 192, 122));
        assert_eq!(DARK.lat_warn, Color::Rgb(224, 163, 94));
        assert_eq!(DARK.lat_bad, Color::Rgb(224, 101, 94));
        assert_eq!(DARK.selection_bg, Color::Rgb(38, 43, 62));
    }

    /// The background the handoff *measured* the light column against. A
    /// measurement reference only — it is a mock-only surface and must never
    /// reach the TUI, which paints no screen background at all.
    const LIGHT_MOCK_SCREEN: Color = rgb(0xf4f5f7);

    fn ratio(c: Color, bg: Color) -> f32 {
        let l = |c: Color| {
            let Color::Rgb(r, g, b) = c else { panic!("token must be truecolor") };
            relative_luminance(r, g, b)
        };
        let (a, b) = (l(c), l(bg));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// The light column, transcribed from the handoff's table (LIGHT-THEME.md §2).
    /// Spelled out as RGB rather than re-deriving from `rgb()`, so a mistyped hex
    /// digit fails here instead of silently shipping an off-spec hue.
    #[test]
    fn light_palette_matches_the_handoff() {
        assert_eq!(LIGHT.bright, Color::Rgb(20, 22, 29));
        assert_eq!(LIGHT.dim, Color::Rgb(74, 79, 99));
        assert_eq!(LIGHT.dimmer, Color::Rgb(106, 112, 138));
        assert_eq!(LIGHT.border, Color::Rgb(169, 174, 192));
        assert_eq!(LIGHT.escape, Color::Rgb(53, 70, 184));
        assert_eq!(LIGHT.corp, Color::Rgb(11, 107, 103));
        assert_eq!(LIGHT.direct, Color::Rgb(47, 107, 35));
        assert_eq!(LIGHT.block, Color::Rgb(109, 67, 173));
        assert_eq!(LIGHT.up, Color::Rgb(138, 82, 8));
        assert_eq!(LIGHT.down, Color::Rgb(11, 107, 103));
        assert_eq!(LIGHT.persistent, Color::Rgb(179, 42, 34));
        assert_eq!(LIGHT.up_table, Color::Rgb(168, 114, 31));
        assert_eq!(LIGHT.down_table, Color::Rgb(45, 129, 124));
        // Latency reuses direct/up/persistent, per the handoff's last row — and
        // `lat_bad` *is* `persistent`, which is why the rose `refused` is gone:
        // one red covers persistent errors, DOWN servers, and >=140 ms alike.
        assert_eq!((LIGHT.lat_ok, LIGHT.lat_warn, LIGHT.lat_bad), (LIGHT.direct, LIGHT.up, LIGHT.persistent));
        assert_eq!((DARK.lat_ok, DARK.lat_warn, DARK.lat_bad), (DARK.direct, DARK.up, DARK.persistent));
    }

    /// The block lane is purple in *both* themes and everywhere it appears — the
    /// rose `#d3788c` the HTML prototype's `_laneCol` used for `block` was a
    /// leftover from an earlier pass, and no longer exists in either column.
    #[test]
    fn the_block_lane_is_purple_and_the_rose_is_gone() {
        assert_eq!(DARK.block, Color::Rgb(169, 138, 214));
        assert_eq!(LIGHT.block, Color::Rgb(109, 67, 173));
        for p in [&DARK, &LIGHT] {
            for c in [p.bright, p.dim, p.dimmer, p.border, p.escape, p.corp, p.direct, p.block] {
                assert_ne!(c, Color::Rgb(0xd3, 0x78, 0x8c), "the block-lane rose must not reappear");
            }
            for c in [p.up, p.down, p.persistent, p.up_table, p.down_table, p.selection_bg] {
                assert_ne!(c, Color::Rgb(0xd3, 0x78, 0x8c), "the block-lane rose must not reappear");
            }
        }
    }

    /// The contrast contract the light column exists to satisfy: every content
    /// token clears WCAG AA on the surface it was measured against (at the
    /// handoff's own 1-dp rounding — `dimmer` is the 4.5 boundary case), except
    /// the two the handoff de-emphasizes on purpose, pinned *below* AA so they
    /// can't drift into load-bearing use. `border` is structure, not content.
    /// (`dimmest`, the third recessive token, was deleted — three grey steps is
    /// one too many in a terminal, and it sat under AA at 3.4:1.)
    #[test]
    fn light_content_tokens_clear_aa_and_recessive_ones_stay_recessive() {
        let rounded = |c| (ratio(c, LIGHT_MOCK_SCREEN) * 10.0).round() / 10.0;
        for (name, c) in [
            ("bright", LIGHT.bright),
            ("dim", LIGHT.dim),
            ("dimmer", LIGHT.dimmer),
            ("escape", LIGHT.escape),
            ("corp", LIGHT.corp),
            ("direct", LIGHT.direct),
            ("block", LIGHT.block),
            ("up", LIGHT.up),
            ("down", LIGHT.down),
            ("persistent", LIGHT.persistent),
            ("lat_ok", LIGHT.lat_ok),
            ("lat_warn", LIGHT.lat_warn),
            ("lat_bad", LIGHT.lat_bad),
        ] {
            assert!(rounded(c) >= 4.5, "light `{name}` is {:.1}:1, below AA", rounded(c));
        }
        for (name, c) in [("up_table", LIGHT.up_table), ("down_table", LIGHT.down_table)] {
            assert!(rounded(c) < 4.5, "`{name}` is no longer de-emphasized ({:.1}:1)", rounded(c));
        }
        // `dim` inherits the help overlay's prose role from the deleted `body`,
        // and the focused pane's border ring — it has to carry both comfortably.
        assert!(rounded(LIGHT.dim) >= 7.0, "light `dim` must read as prose and as a focus ring");
        assert!(ratio(LIGHT.dim, LIGHT.border) >= 3.0, "the light focus ring must separate from the frame");
    }

    #[test]
    fn colorfgbg_reads_only_the_background_field() {
        assert_eq!(from_colorfgbg("15;0"), Some(Theme::Dark));
        assert_eq!(from_colorfgbg("0;15"), Some(Theme::Light));
        assert_eq!(from_colorfgbg("15;default;0"), Some(Theme::Dark)); // rxvt's 3-field form
        assert_eq!(from_colorfgbg("7;7"), Some(Theme::Light));
        assert_eq!(from_colorfgbg("15;8"), Some(Theme::Dark)); // 8 = bright black
        // No answer (not a guess) — the caller falls through to OSC 11.
        assert_eq!(from_colorfgbg("0;default"), None);
        assert_eq!(from_colorfgbg(""), None);
    }

    #[test]
    fn osc11_replies_parse_at_any_component_width() {
        assert_eq!(parse_osc11("\x1b]11;rgb:ffff/ffff/ffff\x07"), Some((255, 255, 255)));
        assert_eq!(parse_osc11("\x1b]11;rgb:ff/ff/ff\x1b\\"), Some((255, 255, 255)));
        assert_eq!(parse_osc11("\x1b]11;rgb:f/f/f\x07"), Some((255, 255, 255)));
        assert_eq!(parse_osc11("\x1b]11;rgb:1616/1717/1e1e\x07"), Some((22, 23, 30)));
        assert_eq!(parse_osc11("\x1b]11;rgb:0000/0000/0000\x07"), Some((0, 0, 0)));
        assert_eq!(parse_osc11("garbage"), None);
        assert_eq!(parse_osc11("\x1b]11;rgb:zz/00/00\x07"), None);
    }

    /// The probe shares a tty with the input loop, so anything it reads is taken
    /// from the operator. It must give up on the first byte that can't belong to
    /// the reply — a keystroke, a paste, a mouse burst — rather than draining.
    #[test]
    fn the_probe_gives_up_on_the_first_byte_that_is_not_the_reply() {
        for good in ["", "\x1b", "\x1b]", "\x1b]11;", "\x1b]11;rgb:1616/1717/1e1e\x07"] {
            assert!(could_be_osc11(good.as_bytes()), "{good:?} is a valid reply prefix");
        }
        for bad in ["q", "\x1b[", "\x1b]12;", "\r", "\x1b[<0;10;5M"] {
            assert!(!could_be_osc11(bad.as_bytes()), "{bad:?} must abort the probe, not be consumed");
        }
    }

    /// The near-paper guard: light only for backgrounds the tokens were tuned
    /// for; a mid-gray or dim terminal stays dark rather than washing out.
    #[test]
    fn only_near_paper_backgrounds_select_light() {
        assert_eq!(theme_for_bg(0xff, 0xff, 0xff), Theme::Light); // white
        assert_eq!(theme_for_bg(0xf4, 0xf5, 0xf7), Theme::Light); // the mock surface
        assert_eq!(theme_for_bg(0xfa, 0xf9, 0xf5), Theme::Light); // warm paper
        assert_eq!(theme_for_bg(0xea, 0xea, 0xea), Theme::Light); // the stated floor
        assert_eq!(theme_for_bg(0xd0, 0xd0, 0xd0), Theme::Dark); // dim gray: not paper
        assert_eq!(theme_for_bg(0x80, 0x80, 0x80), Theme::Dark);
        assert_eq!(theme_for_bg(0x16, 0x17, 0x1e), Theme::Dark); // the dark surface
    }

    #[test]
    fn theme_arg_parses_case_insensitively() {
        assert_eq!(ThemeArg::parse("light"), Some(ThemeArg::Light));
        assert_eq!(ThemeArg::parse("  Dark "), Some(ThemeArg::Dark));
        assert_eq!(ThemeArg::parse("AUTO"), Some(ThemeArg::Auto));
        assert_eq!(ThemeArg::parse("solarized"), None);
        // Explicit never probes the terminal.
        assert_eq!(resolve(ThemeArg::Light), Theme::Light);
        assert_eq!(resolve(ThemeArg::Dark), Theme::Dark);
    }

    /// `emphasize` must move *away* from the terminal's background in both
    /// themes — the "toward white" lift that reads on dark washes out on paper.
    /// Tested through the pure helpers, so no test has to touch the global.
    #[test]
    fn emphasis_moves_away_from_each_theme_background() {
        let l = |c: Color| {
            let Color::Rgb(r, g, b) = c else { panic!() };
            relative_luminance(r, g, b)
        };
        assert!(l(brighten(DARK.dimmer, 0.30)) > l(DARK.dimmer), "dark emphasis lifts");
        assert!(l(darken(LIGHT.dimmer, 0.30)) < l(LIGHT.dimmer), "light emphasis deepens");
        // Worst case in a selected row: the faintest wired token, emphasized,
        // over the selection background — still comfortably readable.
        let r = ratio(darken(LIGHT.dimmer, 0.30), LIGHT.selection_bg);
        assert!(r >= 4.5, "light selected-row text is {r:.1}:1, below AA");
        // And the pulse sinks toward the terminal's background, not through it.
        assert!(l(scale(DARK.direct, 0.5)) < l(DARK.direct), "dark pulse dims");
        let white = Color::Rgb(255, 255, 255);
        assert!(l(mix(LIGHT.direct, white, 0.5)) > l(LIGHT.direct), "light pulse fades toward white");
    }
}
