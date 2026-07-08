//! Design tokens — truecolor palette from the handoff (README "Design tokens").
//! Every value here is copied from the byte-exact `.ansi` renders; do not drift.

use ratatui::style::{Color, Modifier, Style};

pub const BORDER: Color = Color::Rgb(76, 80, 100); // frame, box-drawing
pub const DIM: Color = Color::Rgb(139, 144, 164); // secondary text / lane values
pub const DIMMER: Color = Color::Rgb(101, 106, 130); // labels, table headers
pub const BRIGHT: Color = Color::Rgb(233, 236, 243); // primary values

pub const ESCAPE: Color = Color::Rgb(124, 157, 240); // escape lane (logo blue-purple)
pub const CORP: Color = Color::Rgb(86, 199, 190); // corp lane (teal)
pub const DIRECT: Color = Color::Rgb(134, 192, 122); // direct lane (green)
pub const BLOCK: Color = Color::Rgb(169, 138, 214); // block / blocked (purple)

pub const UP: Color = Color::Rgb(224, 163, 94); // upload arrow/rate; also `transient`
pub const DOWN: Color = Color::Rgb(86, 199, 190); // download arrow/rate

// Muted arrow shades used inside the connections *table* (distinct from the
// brighter header arrows above) — taken verbatim from the golden `.ansi`.
pub const UP_TABLE: Color = Color::Rgb(201, 160, 110);
pub const DOWN_TABLE: Color = Color::Rgb(111, 184, 176);

pub const LAT_OK: Color = Color::Rgb(134, 192, 122); // < 70 ms
pub const LAT_WARN: Color = Color::Rgb(224, 163, 94); // < 140 ms
pub const LAT_BAD: Color = Color::Rgb(224, 101, 94); // >= 140 ms

pub const TRANSIENT: Color = UP; // dns — orange
pub const PERSISTENT: Color = Color::Rgb(224, 101, 94); // timeout/reset/refused — red
pub const BLOCKED: Color = BLOCK; // purple

/// Brighter border for the focused pane (its "cursor").
pub const BORDER_FOCUS: Color = BRIGHT;

pub fn fg(c: Color) -> Style {
    Style::default().fg(c)
}

pub fn bold(c: Color) -> Style {
    Style::default().fg(c).add_modifier(Modifier::BOLD)
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

/// A gentle pulse intensity in [0.45, 1.0] from an animation phase counter —
/// a triangle wave so the `● LIVE` dot breathes to show sampling is live.
pub fn pulse(phase: usize) -> f32 {
    const PERIOD: usize = 16; // ~2s at the 120ms anim tick
    let t = (phase % PERIOD) as f32 / PERIOD as f32; // 0..1
    let tri = 1.0 - (2.0 * t - 1.0).abs(); // 0..1..0
    0.45 + 0.55 * tri
}

/// Latency color by threshold: green < 70, orange < 140, red >= 140.
pub fn latency_color(ms: u32) -> Color {
    if ms < 70 {
        LAT_OK
    } else if ms < 140 {
        LAT_WARN
    } else {
        LAT_BAD
    }
}
