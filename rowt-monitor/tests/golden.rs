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

/// Blank the regions of the frame that intentionally diverge from the frozen
/// design capture, so the byte-exact diff still verifies everything else
/// (panes, columns, borders, reflow). Masked: the identity band (rows 1..=4) —
/// the logo art (bottom row shifted left one) and the right fact column (moved
/// right for breathing room); and the two server-health content rows (stats no
/// longer repeats the active server; the strip marks the active with a ▶).
/// The masked behaviours have their own dedicated assertions below.
fn mask(s: &str) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let n = lines.len();
    lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let health = n >= 4 && (i == n - 4 || i == n - 3); // stats + chips rows
            if (1..=4).contains(&i) {
                line.chars()
                    .enumerate()
                    .map(|(c, ch)| if (1..=27).contains(&c) || c >= 60 { ' ' } else { ch })
                    .collect::<String>()
            } else if health {
                " ".repeat(line.chars().count())
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn golden_96_stacked() {
    assert_eq!(mask(&render_text(96, 41)), mask(G96));
}

#[test]
fn golden_150_side_by_side() {
    assert_eq!(mask(&render_text(150, 30)), mask(G150));
}

#[test]
fn golden_212_wide() {
    assert_eq!(mask(&render_text(212, 30)), mask(G212));
}

#[test]
fn logo_bottom_row_aligned() {
    // The bottom row's left stem must sit at the same column as the rows above
    // it (col 3), not one space right as in the raw design capture.
    let frame = render_text(96, 41);
    let rows: Vec<&str> = frame.lines().collect();
    let col = |row: &str| row.chars().position(|c| c == '|');
    // rows 2 and 3 (0-indexed) start their left stem at col 3; row 4 must too.
    assert_eq!(col(rows[2]), Some(3));
    assert_eq!(col(rows[3]), Some(3));
    assert_eq!(col(rows[4]), Some(3), "bottom logo row should be left-aligned with the rows above");
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

    // Errors TYPE is colored by category. In the stacked 96 frame the errors
    // data rows begin at row 28: row 28 is `timeout` (persistent red), row 31
    // is `dns` (transient orange). TYPE column is at x = pane_x0(3) + 9 = 12.
    let (sym, fg, _) = at(12, 28);
    assert_eq!(sym, "t"); // timeout
    assert_eq!(fg, Color::Rgb(224, 101, 94), "persistent = red");
    let (sym, fg, _) = at(12, 31);
    assert_eq!(sym, "d"); // dns
    assert_eq!(fg, Color::Rgb(224, 163, 94), "transient = orange");
}
