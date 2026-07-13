//! rowt-monitor library surface — exposed so integration tests can drive the
//! renderer and the update logic headlessly (via ratatui's `TestBackend`).

pub mod app;
pub mod clipboard;
pub mod format;
pub mod input;
pub mod model;
pub mod paint;
pub mod source;
pub mod theme;
pub mod ui;

use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;

/// Render one still-fixture frame at WxH to plain text (glyphs only). Used by
/// `--render` and the golden-diff tests.
pub fn render_text(w: u16, h: u16) -> String {
    let a = app::App::new(Box::new(source::FixtureSource::still()));
    render_app_text(&a, w, h)
}

/// Render one still-fixture frame at WxH to truecolor ANSI (SGR per style change,
/// reset at each line end). Used by `--render-ansi` to regenerate the colored
/// design captures; not consumed by the golden-diff tests.
pub fn render_ansi(w: u16, h: u16) -> String {
    let a = app::App::new(Box::new(source::FixtureSource::still()));
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| {
        let area = f.area();
        ui::draw(f.buffer_mut(), area, &a, true);
    })
    .unwrap();
    let buf = term.backend().buffer();
    let mut out = String::new();
    for y in 0..h {
        let mut cur: Option<(Color, Modifier)> = None;
        for x in 0..w {
            let cell = buf.cell((x, y));
            let (fg, m, sym) = cell
                .map(|c| (c.fg, c.modifier, c.symbol()))
                .unwrap_or((Color::Reset, Modifier::empty(), " "));
            if cur != Some((fg, m)) {
                out.push_str("\x1b[0m");
                if m.contains(Modifier::BOLD) {
                    out.push_str("\x1b[1m");
                }
                if m.contains(Modifier::UNDERLINED) {
                    out.push_str("\x1b[4m");
                }
                if let Color::Rgb(r, g, b) = fg {
                    out.push_str(&format!("\x1b[38;2;{r};{g};{b}m"));
                }
                cur = Some((fg, m));
            }
            out.push_str(sym);
        }
        out.push_str("\x1b[0m\n");
    }
    out
}

/// Render a given app (present mode) to plain text at WxH.
pub fn render_app_text(a: &app::App, w: u16, h: u16) -> String {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| {
        let area = f.area();
        ui::draw(f.buffer_mut(), area, a, true);
    })
    .unwrap();
    let buf = term.backend().buffer();
    let mut out = String::new();
    for y in 0..h {
        for x in 0..w {
            out.push_str(buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "));
        }
        out.push('\n');
    }
    out
}
