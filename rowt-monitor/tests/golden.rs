//! Golden-render tests: the still-fixture frame must equal the design captures
//! byte-for-byte (glyphs), and a few key cells must carry the expected color.

use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;
use rowt_monitor::app::App;
use rowt_monitor::source::FixtureSource;
use rowt_monitor::{render_text, ui};

const G96: &str = include_str!("../../design_handoff_rowt_monitor/renders/rowt-monitor-96x30.txt");
const G150: &str = include_str!("../../design_handoff_rowt_monitor/renders/rowt-monitor-150x38.txt");
const G212: &str = include_str!("../../design_handoff_rowt_monitor/renders/rowt-monitor-212x52.txt");

#[test]
fn golden_96_stacked() {
    assert_eq!(render_text(96, 41), G96);
}

#[test]
fn golden_150_side_by_side() {
    assert_eq!(render_text(150, 30), G150);
}

#[test]
fn golden_212_wide() {
    assert_eq!(render_text(212, 30), G212);
}

#[test]
fn colors_spot_check() {
    let app = App::new(Box::new(FixtureSource::still()));
    let mut term = Terminal::new(TestBackend::new(96, 41)).unwrap();
    term.draw(|f| {
        let area = f.area();
        ui::draw(f.buffer_mut(), area, &app, true);
    })
    .unwrap();
    let buf = term.backend().buffer();
    let at = |x: u16, y: u16| {
        let c = buf.cell((x, y)).unwrap();
        (c.symbol().to_string(), c.fg, c.modifier)
    };

    // Logo '_' at (4,1): escape blue, bold.
    let (sym, fg, m) = at(4, 1);
    assert_eq!(sym, "_");
    assert_eq!(fg, Color::Rgb(124, 157, 240));
    assert!(m.contains(Modifier::BOLD));

    // MONITOR 'M' at (29,1): dimmer, not bold.
    let (sym, fg, _) = at(29, 1);
    assert_eq!(sym, "M");
    assert_eq!(fg, Color::Rgb(101, 106, 130));

    // Live dot '●' at (29,2): direct green.
    let (sym, fg, _) = at(29, 2);
    assert_eq!(sym, "●");
    assert_eq!(fg, Color::Rgb(134, 192, 122));

    // Server value 'J' (JP-Tokyo) at (46,3): escape bold.
    let (sym, fg, m) = at(46, 3);
    assert_eq!(sym, "J");
    assert_eq!(fg, Color::Rgb(124, 157, 240));
    assert!(m.contains(Modifier::BOLD));

    // Latency '4' (42 ms) at (55,3): latency-ok green (<70), bold.
    let (sym, fg, m) = at(55, 3);
    assert_eq!(sym, "4");
    assert_eq!(fg, Color::Rgb(134, 192, 122));
    assert!(m.contains(Modifier::BOLD));

    // 'all' row name at (4,7): bold bright white.
    let (sym, fg, m) = at(4, 7);
    assert_eq!(sym, "a");
    assert_eq!(fg, Color::Rgb(233, 236, 243));
    assert!(m.contains(Modifier::BOLD));

    // Header up-arrow '↑' at (11,7): up orange.
    let (sym, fg, _) = at(11, 7);
    assert_eq!(sym, "↑");
    assert_eq!(fg, Color::Rgb(224, 163, 94));
}
