//! Golden-render tests: the still-fixture frame must equal the design captures
//! byte-for-byte (glyphs), and a few key cells must carry the expected color.

use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;
use rowt_monitor::app::{Action, App, Focus};
use rowt_monitor::model::Lane;
use rowt_monitor::source::FixtureSource;
use rowt_monitor::{render_text, theme, ui};

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
    assert!(footer.contains("i.ytimg.com"), "…and the entry it would write: {footer:?}");
    // A freshly armed bar is in its double-tap phase, which names the KEY rather
    // than the lane — `b` is the lane, so the two say the same thing. The lane is
    // spelled out once the bar becomes editable
    // (`footer_confirm_bar_is_a_plain_confirmation_then_an_editor`).
    assert!(footer.contains("press b again to apply"), "{footer:?}");
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
fn footer_search_editor_shows_prompt_and_block_cursor() {
    std::env::set_var("ROWT_MONITOR_NO_CLIPBOARD", "1");
    let mut app = App::new(Box::new(FixtureSource::still()));
    app.side_by_side = true;
    app.conn_h = 6;
    app.err_h = 6;
    app.update(Action::SearchOpen);
    for c in "goog".chars() {
        app.update(Action::SearchInput(c));
    }
    // Move the block cursor into the middle so we can assert a reversed cell there.
    app.update(Action::SearchCursor(-1));

    let (w, h) = (150u16, 41u16);
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| {
        let a = f.area();
        ui::draw_footer(f.buffer_mut(), a, &app);
    })
    .unwrap();
    let buf = term.backend().buffer();
    let footer = row_text(buf, w, h - 1);
    assert!(footer.starts_with("/goog"), "editor shows the / prompt + pattern: {footer:?}");
    assert!(footer.contains("esc cancel"), "editor shows the key hint: {footer:?}");
    // The cursor sits on the 4th char (index 3 = 'g'): that cell is REVERSED.
    let cur = buf.cell((4, h - 1)).unwrap(); // col 0='/', 1='g',2='o',3='o',4='g'
    assert_eq!(cur.symbol(), "g");
    assert!(cur.modifier.contains(Modifier::REVERSED), "block cursor is a reversed cell");
}

#[test]
fn footer_search_indicator_after_commit() {
    std::env::set_var("ROWT_MONITOR_NO_CLIPBOARD", "1");
    let mut app = App::new(Box::new(FixtureSource::still()));
    app.side_by_side = true;
    app.conn_h = 6;
    app.err_h = 6;
    app.update(Action::SearchOpen);
    for c in "google".chars() {
        app.update(Action::SearchInput(c));
    }
    app.update(Action::SearchCommit);

    let (w, h) = (150u16, 41u16);
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| {
        let a = f.area();
        ui::draw_footer(f.buffer_mut(), a, &app);
    })
    .unwrap();
    let footer = row_text(term.backend().buffer(), w, h - 1);
    // Wide terminal: the full indicator fits, right-aligned, and the hints remain.
    assert!(footer.contains("/google/"), "committed indicator echoes the pattern: {footer:?}");
    assert!(footer.contains("/ search"), "the / search hint stays on the left: {footer:?}");
    let (n, m) = app.search_counts();
    assert!(footer.contains(&format!("({n}/{m})")), "indicator shows the n/m match count: {footer:?}");
}

#[test]
fn selected_server_strip_fills_row_circularly() {
    std::env::set_var("ROWT_MONITOR_NO_CLIPBOARD", "1");
    let mut app = App::new(Box::new(FixtureSource::still()));
    app.conn_h = 6;
    app.err_h = 6;
    let (w, h) = (96u16, 41u16);
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    // First draw feeds back the strip geometry (needed for circular paging).
    let mut hit = ui::Hit::default();
    term.draw(|f| {
        let a = f.area();
        hit = ui::draw(f.buffer_mut(), a, &app, false);
    })
    .unwrap();
    app.feed_strip(&hit);
    app.side_by_side = false;
    // Focus the strip and select a chip partway down the (overflowing) pool. The
    // first ←/→ lands on the pinned (active) chip, so 6 presses reach index 5.
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
    let mut hit = ui::Hit::default();
    term.draw(|f| {
        let a = f.area();
        hit = ui::draw(f.buffer_mut(), a, &app, false);
    })
    .unwrap();
    app.feed_strip(&hit);
    app.side_by_side = false;
    app.focus = Focus::Health;
    // Freeze the ring scrolled a few cells in, so the first *ring* chip (index 1 —
    // index 0 is pinned outside the ring) is clipped on its left edge and the last
    // visible chip is clipped on its right edge.
    app.strip_sel = Some(2);
    app.strip_off = 4;
    let mut chips = Vec::new();
    term.draw(|f| {
        let a = f.area();
        chips = ui::draw(f.buffer_mut(), a, &app, false).chips;
    })
    .unwrap();
    assert!(chips.iter().any(|(_, i)| *i == 1), "the left-clipped first ring chip is clickable: {chips:?}");
    // The pinned chip sits at the strip's left edge; the left-clipped ring chip
    // starts right after it (pin + the 3-cell ` │ ` seam), showing its right portion.
    let (r0, _) = chips.iter().find(|(_, i)| *i == 0).expect("the pinned chip is clickable");
    let (r1, _) = chips.iter().find(|(_, i)| *i == 1).unwrap();
    assert_eq!(r1.x, r0.x + r0.width + 3, "left-clipped chip anchored at the ring's left edge");
}

#[test]
fn active_chip_is_pinned_while_the_ring_scrolls() {
    std::env::set_var("ROWT_MONITOR_NO_CLIPBOARD", "1");
    let mut app = App::new(Box::new(FixtureSource::still()));
    app.conn_h = 6;
    app.err_h = 6;
    let (w, h) = (96u16, 41u16);
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    app.focus = Focus::Health;
    app.strip_sel = Some(1); // freeze the ring so the offset is ours, not the clock's
    // Snapshot the strip's left edge at two very different frozen ring offsets.
    let shot = |term: &mut Terminal<TestBackend>, app: &App| -> (String, String, ui::Hit) {
        let mut hit = ui::Hit::default();
        term.draw(|f| {
            let a = f.area();
            hit = ui::draw(f.buffer_mut(), a, app, false);
        })
        .unwrap();
        let buf = term.backend().buffer();
        let stats = (0..h).find(|&yy| row_text(buf, w, yy).contains("servers ·")).expect("stats row");
        // The strip starts at column 2; `▶ JP-Tokyo  42 ms` is 17 cells, then the
        // 3-cell ` │ ` seam, then the ring runs to the frame.
        let row: Vec<char> = row_text(buf, w, stats + 1).chars().collect();
        (row[2..19].iter().collect(), row[22..(w - 2) as usize].iter().collect(), hit)
    };
    app.strip_off = 0;
    let (pin_a, ring_a, hit_a) = shot(&mut term, &app);
    app.strip_off = 37;
    let (pin_b, ring_b, _) = shot(&mut term, &app);
    assert_eq!(hit_a.strip_pin, Some(0), "the active server is the pinned chip");
    assert_eq!(hit_a.strip_w, 92 - 17 - 3, "the fed-back viewport is the *ring* width (strip − pin − seam)");
    assert_eq!(pin_a, "▶ JP-Tokyo  42 ms", "the active chip is drawn at the strip's left edge: {pin_a:?}");
    assert_eq!(pin_a, pin_b, "the pinned chip holds its cells while the ring scrolls under it");
    assert_ne!(ring_a, ring_b, "…and the rest of the pool really did scroll between the two offsets");
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

    // Header aggregate rows are per-lane: the `escape` row name at (2,8) is bold
    // escape-blue. (At this width the Live ↑/↓ rate columns are dropped; the
    // aligned #conns/byte aggregates remain.)
    let (sym, fg, m) = at(2, 8);
    assert_eq!(sym, "e");
    assert_eq!(fg, Color::Rgb(124, 157, 240));
    assert!(m.contains(Modifier::BOLD));

    // The session-facts band is exactly three tiers: labels are `dimmer`, values
    // that carry a working/failed/warning state are semantic, and everything else
    // is plain `bright` — there is no fourth text weight. `mode` and `router` are
    // both in that last group; beside `router` it's the CPU% that carries state,
    // not the word "running".
    let (sym, fg, _) = at(47, 2);
    assert_eq!(sym, "h"); // "host · en0" — mode
    assert_eq!(fg, Color::Rgb(233, 236, 243), "mode carries no state — plain bright");
    let (sym, fg, _) = at(70, 3);
    assert_eq!(sym, "r"); // "running" — router
    assert_eq!(fg, Color::Rgb(233, 236, 243), "router carries no state — plain bright");
    let (sym, fg, _) = at(62, 3);
    assert_eq!(sym, "r"); // the "router" label
    assert_eq!(fg, Color::Rgb(101, 106, 130), "labels are dimmer");

    // The status label takes the dot's own color in every state, healthy
    // included — one signal in two glyphs, not a colored dot beside a plain word.
    let (sym, fg, m) = at(31, 2);
    assert_eq!(sym, "L"); // "LIVE"
    assert_eq!(fg, Color::Rgb(134, 192, 122), "the LIVE label is green, like its dot");
    assert!(m.contains(Modifier::BOLD));

    // The block lane is purple wherever it appears — both the errors header's
    // `blocked` bucket (row 10) and the connections header's `block` lane row.
    // (The HTML prototype's `_laneCol` had it rose; that was a stale pass.)
    let (sym, fg, _) = at(57, 10);
    assert_eq!(sym, "b"); // "blocked"
    assert_eq!(fg, Color::Rgb(169, 138, 214), "blocked = lane_block purple");

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

/// Adding `E`/`C`/`B`/`D` grew the help box from 22 rows to 24. `draw_help`
/// centres it with a `saturating_sub` that clamps to 0 and then wrote each row
/// unguarded — and `set_string` clips the X axis but *indexes* Y, so a pane
/// shorter than the box panicked instead of clipping. Two more lines moved that
/// threshold onto terminal heights people actually use, so the rows are now
/// gated and this sweep covers both sides of the box.
///
/// Bounded below by 13 rows / 80 cols, which is where the base frame itself
/// becomes renderable (a separate, pre-existing limit — this test is about the
/// overlay, not the layout).
#[test]
fn help_overlay_clips_instead_of_panicking_on_a_short_terminal() {
    let mut app = App::new(Box::new(FixtureSource::still()));
    app.update(Action::ToggleHelp);
    assert!(app.help);
    for h in 13..=26u16 {
        for w in [80u16, 96, 150] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| {
                let a = f.area();
                ui::draw(f.buffer_mut(), a, &app, false);
            })
            .unwrap_or_else(|e| panic!("help overlay at {w}x{h}: {e}"));
        }
    }
}
/// The confirm bar has two phases, and the whole point is that only COLOUR and
/// the cursor distinguish them — nothing moves, and no wording changes.
#[test]
fn footer_confirm_bar_is_a_plain_confirmation_then_an_editor() {
    use rowt_monitor::app::DOUBLE_TAP_WINDOW;
    std::env::set_var("ROWT_MONITOR_NO_CLIPBOARD", "1");
    let (w, h) = (150u16, 41u16);
    let mut app = App::new(Box::new(FixtureSource::still()));
    app.side_by_side = true;
    app.conn_h = 6;
    app.err_h = 6;
    app.update(Action::Down);
    app.update(Action::Route(Lane::Escape));

    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    let snap = |term: &mut Terminal<TestBackend>, app: &App| {
        term.draw(|f| {
            let a = f.area();
            ui::draw_footer(f.buffer_mut(), a, app);
        })
        .unwrap();
        let buf = term.backend().buffer();
        let row = row_text(buf, w, h - 1);
        let cur: Vec<u16> = (0..w)
            .filter(|x| buf.cell((*x, h - 1)).unwrap().modifier.contains(Modifier::REVERSED))
            .collect();
        let fg = buf.cell((row.find("CONFIRM").unwrap() as u16, h - 1)).unwrap().fg;
        (row, cur, fg)
    };

    // Phase 1 — a plain confirmation. No cursor, and NOT the armed colour.
    let (row1, cur1, fg1) = snap(&mut term, &app);
    assert!(cur1.is_empty(), "no cursor inside the double-tap window: {row1:?}");
    assert_ne!(fg1, theme::armed(), "phase 1 is not the editing colour");
    assert!(row1.contains("i.ytimg.com"), "{row1:?}");
    // Phase 1 names only the key that works in it — and that key encodes the lane.
    assert!(row1.contains("press e again to apply"), "{row1:?}");
    assert!(!row1.contains("↵ apply"), "no apply/cancel hint yet: {row1:?}");

    // Phase 2 — the window closes: amber, with a cursor. Nothing else changes.
    app.armed.as_mut().unwrap().at = std::time::Instant::now() - DOUBLE_TAP_WINDOW;
    let (row2, cur2, fg2) = snap(&mut term, &app);
    assert_eq!(cur2.len(), 1, "a block cursor once editable: {row2:?}");
    assert_eq!(fg2, theme::armed(), "phase 2 is the armed colour");
    assert_ne!(fg1, fg2, "the colour also tells the two phases apart");
    assert!(row2.contains("→ escape"), "phase 2 names the lane: {row2:?}");
    assert!(row2.contains("↵ apply"), "{row2:?}");
    assert!(!row2.contains("press e again"), "the double-tap is gone: {row2:?}");
    // The hints differ in length, so the SHORTER is padded — nothing may move.
    assert_eq!(
        row1.find("i.ytimg.com"),
        row2.find("i.ytimg.com"),
        "the entry sits in the same column in both phases:\n{row1:?}\n{row2:?}"
    );
    // The cursor lands on the reserved append column, one past the entry.
    let entry_end = row2.find("i.ytimg.com").unwrap() as u16 + "i.ytimg.com".len() as u16;
    assert_eq!(cur2[0], entry_end);

    // Right-aligned: the bar ends at the right edge, and the left is empty.
    assert!(row2.trim_end().ends_with("esc cancel"), "{row2:?}");
    assert!(row2.starts_with("  "), "{row2:?}");
}

/// The UX property the right-aligned bar exists for: the cursor, and every hint
/// after it, hold still. Typing extends the entry LEFTWARD. The bar's width is a
/// function of the entry alone — one fixed hint, and the cursor's cell reserved
/// from the start — so neither the phase change nor a keystroke can slide it.
#[test]
fn the_cursor_and_the_hint_after_it_never_move() {
    use rowt_monitor::app::{Edit, DOUBLE_TAP_WINDOW};
    std::env::set_var("ROWT_MONITOR_NO_CLIPBOARD", "1");
    let (w, h) = (150u16, 41u16);
    // The anchor is the APPEND column — the reserved cell the cursor occupies.
    // It is the one landmark present in both phases: phase 1 draws no cursor, and
    // an emptied entry (^U) leaves no text to search for, so neither the cursor
    // nor the entry alone can carry the measurement.
    let cols = |edits: &[Edit], age: std::time::Duration| -> u16 {
        let mut app = App::new(Box::new(FixtureSource::still()));
        app.side_by_side = true;
        app.conn_h = 6;
        app.err_h = 6;
        app.update(Action::Down);
        app.update(Action::Route(Lane::Escape));
        for e in edits {
            app.update(Action::ArmEdit(*e));
        }
        app.armed.as_mut().unwrap().at = std::time::Instant::now() - age;
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| {
            let a = f.area();
            ui::draw_footer(f.buffer_mut(), a, &app);
        })
        .unwrap();
        let buf = term.backend().buffer();
        let row = row_text(buf, w, h - 1);
        let entry = app.armed.as_ref().unwrap().domain.clone();
        // Layout is [head][entry][append cell][tail], and every tail opens "  → ",
        // so the arrow sits 3 past the append cell and " → " starts 2 past it.
        let append = row.find(" → ").expect("the tail") as u16 - 2;
        if let Some(cur) = (0..w).find(|x| buf.cell((*x, h - 1)).unwrap().modifier.contains(Modifier::REVERSED)) {
            assert_eq!(cur, append, "the cursor IS the append cell: {row:?}");
        }
        if !entry.is_empty() {
            let end = row.find(&entry).expect("the entry") as u16 + entry.chars().count() as u16;
            assert_eq!(end, append, "the append cell sits one past the entry: {row:?}");
        }
        append
    };

    let zero = std::time::Duration::ZERO;
    let armed = cols(&[], zero);
    // The phase change — the transition that used to jump.
    assert_eq!(cols(&[], DOUBLE_TAP_WINDOW), armed, "going editable must not move anything");
    // …and it stays put as the entry grows and shrinks.
    assert_eq!(cols(&[Edit::Insert('x')], zero), armed, "first keystroke");
    assert_eq!(cols(&[Edit::Insert('x'), Edit::Insert('y'), Edit::Insert('z')], zero), armed);
    assert_eq!(cols(&[Edit::KillWord], zero), armed);
    assert_eq!(cols(&[Edit::KillLine], zero), armed);
}

#[test]
fn the_confirm_bar_flags_an_over_broad_entry_before_you_commit() {
    use rowt_monitor::app::Edit;
    std::env::set_var("ROWT_MONITOR_NO_CLIPBOARD", "1");
    let (w, h) = (150u16, 41u16);
    let entry_fg = |edits: &[Edit]| -> Color {
        let mut app = App::new(Box::new(FixtureSource::still()));
        app.side_by_side = true;
        app.conn_h = 6;
        app.err_h = 6;
        app.update(Action::Down);
        app.update(Action::Route(Lane::Escape));
        for e in edits {
            app.update(Action::ArmEdit(*e));
        }
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| {
            let a = f.area();
            ui::draw_footer(f.buffer_mut(), a, &app);
        })
        .unwrap();
        let buf = term.backend().buffer();
        let row = row_text(buf, w, h - 1);
        let entry = app.armed.as_ref().unwrap().domain.clone();
        buf.cell((row.find(&entry).unwrap() as u16, h - 1)).unwrap().fg
    };
    let typed = |s: &str| -> Vec<Edit> {
        let mut v = vec![Edit::KillLine];
        v.extend(s.chars().map(Edit::Insert));
        v
    };
    // The refusal has to arrive while you are typing, not after `↵`.
    assert_eq!(entry_fg(&typed("com")), theme::block(), "a bare TLD reads as dangerous");
    assert_eq!(entry_fg(&typed("co.uk")), theme::block(), "so does a registry suffix");
    assert_ne!(entry_fg(&typed("bbc.co.uk")), theme::block(), "a real domain does not");
    assert_ne!(entry_fg(&typed("z.com")), theme::block());
}
