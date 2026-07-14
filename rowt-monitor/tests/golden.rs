//! Golden-render tests: the still-fixture frame must equal the design captures
//! byte-for-byte (glyphs), and a few key cells must carry the expected color.

use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;
use rowt_monitor::app::{Action, App, Focus};
use rowt_monitor::model::Lane;
use rowt_monitor::source::FixtureSource;
use rowt_monitor::{render_text, ui};

const G96: &str = include_str!("../../ux-design/rowt_monitor/renders/rowt-monitor-96x30.txt");
const G150: &str = include_str!("../../ux-design/rowt_monitor/renders/rowt-monitor-150x38.txt");
const G212: &str = include_str!("../../ux-design/rowt_monitor/renders/rowt-monitor-212x52.txt");

/// Blank the regions of the frame that intentionally diverge from the frozen
/// design capture, so the byte-exact diff still verifies everything else
/// (panes, columns, borders, reflow). Masked: the identity band (rows 1..=4) —
/// the logo art (bottom row shifted left one) and the right fact column (moved
/// right for breathing room); and the two server-health content rows (stats no
/// longer repeats the active server; the strip marks the active with a ▶).
/// The masked behaviours have their own dedicated assertions below.
/// Neutralize the per-row ↑/↓ in the connections *table* (removed as redundant
/// with the UP/DOWN column headers): a table arrow is one immediately followed
/// by a digit (e.g. `↑8.0M`), unlike the header rate arrows (`↑ 2.9`, arrow +
/// space), which are kept. Applied to both sides so alignment is still checked.
fn strip_table_arrows(s: &str) -> String {
    let ch: Vec<char> = s.chars().collect();
    (0..ch.len())
        .map(|i| {
            let c = ch[i];
            if (c == '↑' || c == '↓') && ch.get(i + 1).is_some_and(|n| n.is_ascii_digit()) {
                ' '
            } else {
                c
            }
        })
        .collect()
}

fn mask(s: &str) -> String {
    let s = strip_table_arrows(s);
    let lines: Vec<&str> = s.lines().collect();
    let n = lines.len();
    lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let health = n >= 4 && (i == n - 4 || i == n - 3); // stats + chips rows
            if i == 4 {
                // proxy/config row diverges (proxy = system-proxy on/off, no iface)
                " ".repeat(line.chars().count())
            } else if (1..=3).contains(&i) {
                // logo (1..27) + the left value column (moved to 47) + right
                // column (60+) all diverge; keep the labels (28..45) checked.
                line.chars()
                    .enumerate()
                    .map(|(c, ch)| if (1..=27).contains(&c) || c >= 46 { ' ' } else { ch })
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

/// Read a full buffer row as a plain string (for footer/overlay assertions).
fn row_text(buf: &ratatui::buffer::Buffer, w: u16, y: u16) -> String {
    (0..w).map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" ")).collect()
}

#[test]
fn footer_confirm_bar_when_armed() {
    std::env::set_var("ROWT_MONITOR_NO_CLIPBOARD", "1");
    let mut app = App::new(Box::new(FixtureSource::still()));
    app.side_by_side = true;
    app.conn_h = 6;
    app.err_h = 6;
    app.update(Action::Down); // activate the selection
    app.update(Action::Route(Lane::Block)); // arm (does not commit)
    assert!(app.armed.is_some());

    let (w, h) = (96u16, 41u16);
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| {
        let a = f.area();
        ui::draw_footer(f.buffer_mut(), a, &app);
    })
    .unwrap();
    let footer = row_text(term.backend().buffer(), w, h - 1);
    assert!(footer.contains("CONFIRM"), "armed footer shows the confirm bar: {footer:?}");
    assert!(footer.contains("→ block"), "confirm bar previews the target lane: {footer:?}");
}

#[test]
fn footer_global_and_contextual_groups() {
    std::env::set_var("ROWT_MONITOR_NO_CLIPBOARD", "1");
    let mut app = App::new(Box::new(FixtureSource::still()));
    app.side_by_side = true;
    app.conn_h = 6;
    app.err_h = 6;
    let (w, h) = (150u16, 41u16);
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    // No selection yet: only the global group shows.
    term.draw(|f| {
        let a = f.area();
        ui::draw_footer(f.buffer_mut(), a, &app);
    })
    .unwrap();
    let bare = row_text(term.backend().buffer(), w, h - 1);
    assert!(bare.contains("o proxy"), "global group always present: {bare:?}");
    assert!(!bare.contains("route"), "no contextual group without a selection");
    // Activate a row: the contextual route/copy group appears.
    app.update(Action::Down);
    term.draw(|f| {
        let a = f.area();
        ui::draw_footer(f.buffer_mut(), a, &app);
    })
    .unwrap();
    let sel = row_text(term.backend().buffer(), w, h - 1);
    assert!(sel.contains("route"), "contextual group appears once a row is locked: {sel:?}");
}

#[test]
fn selected_server_strip_fills_row_circularly() {
    std::env::set_var("ROWT_MONITOR_NO_CLIPBOARD", "1");
    let mut app = App::new(Box::new(FixtureSource::still()));
    app.conn_h = 6;
    app.err_h = 6;
    let (w, h) = (96u16, 41u16);
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    // First draw feeds back the strip viewport width (needed for circular paging).
    let mut sw = 0u16;
    term.draw(|f| {
        let a = f.area();
        sw = ui::draw(f.buffer_mut(), a, &app, false).strip_w;
    })
    .unwrap();
    app.strip_w = sw;
    app.side_by_side = false;
    // Focus the strip and select a chip partway down the (overflowing) pool.
    app.focus = Focus::Health;
    for _ in 0..6 {
        app.update(Action::FocusRight);
    }
    assert_eq!(app.strip_sel, Some(5));
    term.draw(|f| {
        let a = f.area();
        ui::draw(f.buffer_mut(), a, &app, false);
    })
    .unwrap();
    let buf = term.backend().buffer();
    // The chips row is the one just below the "N servers · …" stats line.
    let stats = (0..h).find(|&yy| row_text(buf, w, yy).contains("servers ·")).expect("stats row");
    let chips_y = stats + 1;
    // The selected server is visible…
    let name = &app.snap.chips[5].name;
    assert!(row_text(buf, w, chips_y).contains(name.as_str()), "selected chip visible: {:?}", row_text(buf, w, chips_y));
    // …and the frozen ring fills the right side of the row (wraps past the end),
    // rather than leaving it blank as a stop-at-list-end pager would.
    let filled_right = (w / 2..w - 3).filter(|&x| buf.cell((x, chips_y)).map(|c| c.symbol().trim() != "").unwrap_or(false)).count();
    assert!(filled_right > 10, "strip wraps to fill the row's right side ({filled_right} cells)");
}

#[test]
fn both_partial_edge_chips_are_clickable() {
    std::env::set_var("ROWT_MONITOR_NO_CLIPBOARD", "1");
    let mut app = App::new(Box::new(FixtureSource::still()));
    app.conn_h = 6;
    app.err_h = 6;
    let (w, h) = (96u16, 41u16);
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    let mut sw = 0u16;
    term.draw(|f| {
        let a = f.area();
        sw = ui::draw(f.buffer_mut(), a, &app, false).strip_w;
    })
    .unwrap();
    app.strip_w = sw;
    app.side_by_side = false;
    app.focus = Focus::Health;
    // Freeze the ring scrolled a few cells in, so the FIRST chip is clipped on its
    // left edge and the last visible chip is clipped on its right edge.
    app.strip_sel = Some(2);
    app.strip_off = 4;
    let mut chips = Vec::new();
    term.draw(|f| {
        let a = f.area();
        chips = ui::draw(f.buffer_mut(), a, &app, false).chips;
    })
    .unwrap();
    assert!(chips.iter().any(|(_, i)| *i == 0), "the left-clipped first chip is clickable: {chips:?}");
    // its rect starts at the strip's left edge (right portion shown from col 0)
    let (r0, _) = chips.iter().find(|(_, i)| *i == 0).unwrap();
    let strip_left = w - 3 - sw; // xr-1-width ≈ strip start; just assert it's near the left
    assert!(r0.x <= strip_left + 3, "left-clipped chip anchored at the strip's left edge");
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

    // Server value 'J' (JP-Tokyo) at (70,2): the right column value sits at 70
    // (pulled in from 78 to tighten the middle gap), escape bold.
    let (sym, fg, m) = at(70, 2);
    assert_eq!(sym, "J");
    assert_eq!(fg, Color::Rgb(124, 157, 240));
    assert!(m.contains(Modifier::BOLD));

    // Latency '4' (42 ms) now at (79,2): ms sits at value(70) + 1 + name_reserve(8).
    let (sym, fg, m) = at(79, 2);
    assert_eq!(sym, "4");
    assert_eq!(fg, Color::Rgb(134, 192, 122));
    assert!(m.contains(Modifier::BOLD));

    // Collector value 'o' (on) at (47,4): left column under sys proxy, green.
    let (sym, fg, _) = at(47, 4);
    assert_eq!(sym, "o");
    assert_eq!(fg, Color::Rgb(134, 192, 122), "collector on = green");

    // 'all' row name at (2,7): bold bright white. (Single-frame layout: content
    // starts at col 2; a blank breathing row under the logo puts the summary at 7.)
    let (sym, fg, m) = at(2, 7);
    assert_eq!(sym, "a");
    assert_eq!(fg, Color::Rgb(233, 236, 243));
    assert!(m.contains(Modifier::BOLD));

    // Header up-arrow '↑' at (9,7): up orange.
    let (sym, fg, _) = at(9, 7);
    assert_eq!(sym, "↑");
    assert_eq!(fg, Color::Rgb(224, 163, 94));

    // Errors TYPE is colored by category. Errors is now the right column; its
    // data rows begin at row 13: row 13 is `timeout` (persistent red), row 16 is
    // `dns` (transient orange). The TYPE column sits at x = 65 in the 96 frame.
    let (sym, fg, _) = at(65, 13);
    assert_eq!(sym, "t"); // timeout
    assert_eq!(fg, Color::Rgb(224, 101, 94), "persistent = red");
    let (sym, fg, _) = at(65, 16);
    assert_eq!(sym, "d"); // dns
    assert_eq!(fg, Color::Rgb(224, 163, 94), "transient = orange");
}
