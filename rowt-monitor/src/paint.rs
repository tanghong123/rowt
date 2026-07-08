//! Thin painter over a ratatui `Buffer` for absolute cell writes. All glyphs in
//! this UI are width-1 (ascii, box-drawing, `· ↑ ↓ ●`), so char count == display
//! width; we rely on that for right-alignment.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

/// Display width — valid because every glyph we draw is one cell wide.
pub fn dw(s: &str) -> u16 {
    s.chars().count() as u16
}

/// Write `s` starting at (x, y). Clipped at the buffer edge by ratatui.
pub fn put(buf: &mut Buffer, x: u16, y: u16, s: &str, style: Style) {
    buf.set_string(x, y, s, style);
}

/// Write `s` so its last cell lands on column `right` (inclusive).
pub fn put_right(buf: &mut Buffer, right: u16, y: u16, s: &str, style: Style) {
    let w = dw(s);
    if w == 0 {
        return;
    }
    let x = right.saturating_sub(w - 1);
    buf.set_string(x, y, s, style);
}

/// Repeat `ch` across columns [x0, x1] inclusive on row `y`.
pub fn hfill(buf: &mut Buffer, x0: u16, x1: u16, y: u16, ch: char, style: Style) {
    if x1 < x0 {
        return;
    }
    let s: String = std::iter::repeat_n(ch, (x1 - x0 + 1) as usize).collect();
    buf.set_string(x0, y, s, style);
}

/// Overlay `style` onto every cell of `rect` (used for selection highlight).
pub fn patch_style(buf: &mut Buffer, rect: Rect, style: Style) {
    for y in rect.top()..rect.bottom() {
        for x in rect.left()..rect.right() {
            if let Some(c) = buf.cell_mut((x, y)) {
                c.set_style(style);
            }
        }
    }
}

/// Truncate `s` to at most `max` columns, appending `…` when cut.
pub fn truncate(s: &str, max: u16) -> String {
    let w = dw(s);
    if w <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let keep = (max - 1) as usize;
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}
