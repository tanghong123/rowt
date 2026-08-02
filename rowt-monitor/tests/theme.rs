//! Light-theme rendering, plus the check that `COLORS.md` matches what ships.
//!
//! This lives in its own test binary because the active theme is a process
//! global: `cargo test` runs each target in its own process, so flipping it here
//! can't disturb the dark-palette assertions in `golden.rs`. For the same reason
//! **only one test in this file may touch the global** — tests within a binary run
//! concurrently, and two of them fighting over it would be flaky. The palette-doc
//! test below reads the `DARK`/`LIGHT` consts only, never the global, so it is
//! safe to run alongside.

use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;
use rowt_monitor::app::{App, Focus};
use rowt_monitor::source::FixtureSource;
use rowt_monitor::theme::{self, Palette, Theme, DARK, LIGHT};
use rowt_monitor::{render_text, ui};

fn frame(w: u16, h: u16, app: &App, present: bool) -> ratatui::buffer::Buffer {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| {
        let a = f.area();
        ui::draw(f.buffer_mut(), a, app, present);
    })
    .unwrap();
    term.backend().buffer().clone()
}

fn luma(c: Color) -> f32 {
    let Color::Rgb(r, g, b) = c else { panic!("expected truecolor, got {c:?}") };
    theme::relative_luminance(r, g, b)
}

#[test]
fn light_theme_recolors_without_changing_the_frame() {
    std::env::set_var("ROWT_MONITOR_NO_CLIPBOARD", "1");
    let (w, h) = (96u16, 41u16);

    // ---- the layout contract holds across themes -------------------------
    // "Same structure, same glyphs, same layout — only the color values change."
    let dark_glyphs = render_text(w, h);
    theme::set(Theme::Light);
    assert_eq!(theme::current(), Theme::Light);
    let light_glyphs = render_text(w, h);
    assert_eq!(dark_glyphs, light_glyphs, "the light theme must not move a single cell");

    // ---- the same probes as golden.rs::colors_spot_check, in light --------
    let app = App::new(Box::new(FixtureSource::still()));
    let buf = frame(w, h, &app, true);
    let at = |x: u16, y: u16| {
        let c = buf.cell((x, y)).unwrap();
        (c.symbol().to_string(), c.fg, c.modifier)
    };

    let (sym, fg, m) = at(4, 1); // logo '_' — escape, bold
    assert_eq!(sym, "_");
    assert_eq!(fg, LIGHT.escape);
    assert!(m.contains(Modifier::BOLD), "bold is not a lightness substitute — it stays exactly where it was");

    let (sym, fg, _) = at(29, 1); // 'MONITOR' — dimmer
    assert_eq!(sym, "M");
    assert_eq!(fg, LIGHT.dimmer);

    let (sym, fg, _) = at(29, 2); // '●' LIVE dot — direct green
    assert_eq!(sym, "●");
    assert_eq!(fg, LIGHT.direct);
    let (sym, fg, _) = at(31, 2); // …and its label takes the same color
    assert_eq!(sym, "L");
    assert_eq!(fg, LIGHT.direct, "the status label mirrors its dot in every state");

    let (sym, fg, _) = at(70, 3); // 'running' — carries no state, so plain
    assert_eq!(sym, "r");
    assert_eq!(fg, LIGHT.bright, "the band has three tiers, not four");

    let (sym, fg, _) = at(79, 2); // '42 ms' — latency under 70
    assert_eq!(sym, "4");
    assert_eq!(fg, LIGHT.lat_ok);

    let (sym, fg, m) = at(2, 7); // 'all' row name — bright, bold
    assert_eq!(sym, "a");
    assert_eq!(fg, LIGHT.bright);
    assert!(m.contains(Modifier::BOLD));

    let (sym, fg, _) = at(65, 13); // errors TYPE 'timeout' — persistent
    assert_eq!(sym, "t");
    assert_eq!(fg, LIGHT.persistent);
    let (sym, fg, _) = at(65, 16); // errors TYPE 'dns' — transient (= up)
    assert_eq!(sym, "d");
    assert_eq!(fg, LIGHT.up);

    // Every foreground the frame paints must be darker than the lightest paper a
    // terminal could have — the whole point of the light palette. The threshold
    // is the handoff's stated floor (`#eaeaea`), not a screen color: the TUI
    // paints no background and must not assume one.
    let floor = theme::relative_luminance(0xea, 0xea, 0xea);
    for y in 0..h {
        for x in 0..w {
            let c = buf.cell((x, y)).unwrap();
            if c.symbol().trim().is_empty() {
                continue;
            }
            assert!(luma(c.fg) < floor, "cell ({x},{y}) {:?} is too light for a near-paper terminal", c.fg);
        }
    }

    // §2: the mock-only surfaces exist so the HTML prototype can render. None of
    // them may reach a cell — not as a foreground, and not as a background.
    for mock in [
        Color::Rgb(0x16, 0x17, 0x1e), // screen (dark)
        Color::Rgb(0x10, 0x11, 0x16), // pane (dark)
        Color::Rgb(0x1c, 0x1e, 0x26), // raised (dark)
        Color::Rgb(0xf4, 0xf5, 0xf7), // screen (light)
        Color::Rgb(0xfb, 0xfb, 0xfd), // pane (light)
        Color::Rgb(0xee, 0xf0, 0xf4), // raised (light)
    ] {
        for y in 0..h {
            for x in 0..w {
                let c = buf.cell((x, y)).unwrap();
                assert_ne!(c.fg, mock, "mock-only surface {mock:?} painted at ({x},{y})");
                assert_ne!(c.bg, mock, "mock-only surface {mock:?} painted at ({x},{y})");
            }
        }
    }

    // ---- selection: the one background the TUI paints --------------------
    let mut app = App::new(Box::new(FixtureSource::still()));
    app.conn_h = 6;
    app.err_h = 6;
    app.focus = Focus::Conn;
    app.update(rowt_monitor::app::Action::Down); // lock row 0
    let buf = frame(w, h, &app, false);
    let row = (0..h).find(|&y| buf.cell((2, y)).is_some_and(|c| c.bg == LIGHT.selection_bg)).expect("a selected row");
    let cell = buf.cell((4, row)).unwrap();
    assert_eq!(cell.bg, LIGHT.selection_bg, "selected rows use the light selection background");
    // …and its text is *darkened* into that background, not brightened out of it.
    assert!(luma(cell.fg) < luma(LIGHT.selection_bg), "selected text must deepen on light, not wash out");

    // ---- the focus ring softens on light -------------------------------
    // Dark slams `border_focus` to `bright` (reads as emitted light); on paper
    // that's a black slab, so light steps to `dim` instead. `border_focus` draws
    // the help overlay's frame — the panes signal focus in their captions.
    let mut app = App::new(Box::new(FixtureSource::still()));
    app.help = true;
    let buf = frame(w, h, &app, false);
    // The outer frame owns the `╭` at (0,0); the overlay's is the inset one.
    let ring = (1..h)
        .flat_map(|y| (1..w).map(move |x| (x, y)))
        .find_map(|(x, y)| buf.cell((x, y)).filter(|c| c.symbol() == "╭").map(|c| c.fg))
        .expect("the help overlay draws a rounded frame");
    assert_eq!(ring, LIGHT.dim, "light's focus ring is a single-weight step, not a black slab");
    assert_ne!(ring, LIGHT.bright, "…specifically not dark's maximum-lift treatment");

    theme::set(Theme::Dark); // leave the global as we found it
    let buf = frame(w, h, &app, false);
    // The outer frame owns the `╭` at (0,0); the overlay's is the inset one.
    let ring = (1..h)
        .flat_map(|y| (1..w).map(move |x| (x, y)))
        .find_map(|(x, y)| buf.cell((x, y)).filter(|c| c.symbol() == "╭").map(|c| c.fg))
        .expect("the help overlay draws a rounded frame");
    assert_eq!(ring, rowt_monitor::theme::DARK.bright, "dark's focus ring is unchanged");
}

/// `COLORS.md` documents the shipped palette, so it has to *be* the shipped
/// palette. This parses its token table and diffs it against `DARK`/`LIGHT` both
/// ways — a wrong hex, a renamed token, a stale row, or a token that never made it
/// into the doc all fail here.
///
/// The `let Palette { .. }` destructure below is the other half: it is exhaustive,
/// so adding a field to `Palette` stops compiling until the field is listed here
/// *and* documented. Doc drift is a compile error, not a code review miss.
#[test]
fn colors_md_documents_exactly_the_shipped_palette() {
    let Palette {
        bright,
        dim,
        dimmer,
        border,
        escape,
        corp,
        direct,
        block,
        up,
        down,
        persistent,
        up_table,
        down_table,
        lat_ok,
        lat_warn,
        lat_bad,
        selection_bg,
    } = DARK;
    let shipped = [
        ("bright", bright, LIGHT.bright),
        ("dim", dim, LIGHT.dim),
        ("dimmer", dimmer, LIGHT.dimmer),
        ("border", border, LIGHT.border),
        ("escape", escape, LIGHT.escape),
        ("corp", corp, LIGHT.corp),
        ("direct", direct, LIGHT.direct),
        ("block", block, LIGHT.block),
        ("up", up, LIGHT.up),
        ("down", down, LIGHT.down),
        ("persistent", persistent, LIGHT.persistent),
        ("up_table", up_table, LIGHT.up_table),
        ("down_table", down_table, LIGHT.down_table),
        ("lat_ok", lat_ok, LIGHT.lat_ok),
        ("lat_warn", lat_warn, LIGHT.lat_warn),
        ("lat_bad", lat_bad, LIGHT.lat_bad),
        ("selection_bg", selection_bg, LIGHT.selection_bg),
    ];

    // Rows look like: | `token` | `#rrggbb` | 15.1 | `#rrggbb` | 16.6 | notes |
    // (`selection_bg` has `—` where a contrast ratio would go.)
    let hex = |s: &str| -> Option<Color> {
        let h = s.trim().trim_matches('`').strip_prefix('#')?;
        let v = u32::from_str_radix(h, 16).ok().filter(|_| h.len() == 6)?;
        Some(Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
    };
    let doc = include_str!("../COLORS.md");
    let documented: Vec<(String, Color, Color)> = doc
        .lines()
        .filter_map(|l| {
            let cols: Vec<&str> = l.split('|').collect();
            if cols.len() < 7 {
                return None;
            }
            let name = cols[1].trim().trim_matches('`');
            Some((name.to_string(), hex(cols[2])?, hex(cols[4])?))
        })
        .collect();

    for (name, d, l) in &shipped {
        let Some((_, dd, dl)) = documented.iter().find(|(n, _, _)| n == name) else {
            panic!("`{name}` is in Palette but has no row in COLORS.md");
        };
        assert_eq!(dd, d, "COLORS.md has the wrong dark value for `{name}`");
        assert_eq!(dl, l, "COLORS.md has the wrong light value for `{name}`");
    }
    for (name, _, _) in &documented {
        assert!(
            shipped.iter().any(|(n, _, _)| n == name),
            "COLORS.md documents `{name}`, which is not a Palette field — stale row?"
        );
    }
    assert_eq!(documented.len(), shipped.len(), "COLORS.md token table and Palette disagree in size");
}
