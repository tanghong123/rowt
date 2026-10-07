use ratatui::{backend::TestBackend, layout::Rect, Terminal};
use rowt_monitor::{app::{Action, App, Focus, ServerMode}, model::Server, source::FixtureSource, ui};

fn frame(term: &mut Terminal<TestBackend>, app: &mut App, w: u16, h: u16) -> ui::Hit {
    term.backend_mut().resize(w, h);
    term.resize(Rect::new(0, 0, w, h)).unwrap();
    let mut hit = ui::Hit::default();
    term.draw(|f| {
        let area = f.area();
        let body = Rect::new(area.x, area.y, area.width, area.height.saturating_sub(1));
        hit = ui::draw(f.buffer_mut(), body, app, false);
        ui::draw_footer(f.buffer_mut(), area, app);
    }).unwrap();
    app.feed_strip(&hit);
    hit
}

#[test]
fn resizing_to_a_narrow_terminal_and_back_does_not_crash() {
    let mut app = App::new(Box::new(FixtureSource::still()));
    app.server_mode = ServerMode::List;
    let mut term = Terminal::new(TestBackend::new(150, 30)).unwrap();
    for (w, h) in [(150, 30), (50, 30), (8, 30), (1, 1), (0, 0), (96, 30), (150, 30)] {
        frame(&mut term, &mut app, w, h);
    }
    assert!(!app.should_quit);
    assert!(!frame(&mut term, &mut app, 150, 30).chips.is_empty());
}

#[test]
fn shrinking_height_to_zero_and_back_respects_the_minimum_size() {
    let mut term = Terminal::new(TestBackend::new(150, 40)).unwrap();
    for width in [96, 150, 212] {
        let mut app = App::new(Box::new(FixtureSource::still()));
        app.server_mode = ServerMode::List;
        app.update(Action::SelectServer(2));
        let selected = app.snap.chips[2].name.clone();
        for height in (0..=40).rev().chain(0..=40) {
            let hit = frame(&mut term, &mut app, width, height);
            if height >= 12 {
                assert!(hit.conn_h > 0, "{width}x{height}");
                assert!(hit.chips.iter().any(|(_, i)| *i == 2), "{width}x{height}");
            } else {
                assert!(hit.chips.is_empty(), "{width}x{height}");
                if height >= 2 {
                    let text: String = term.backend().buffer().content.iter().map(|c| c.symbol()).collect();
                    assert!(text.contains("Resize terminal to at least 40x12"), "{width}x{height}");
                }
            }
            assert_eq!(app.snap.chips[app.strip_sel.unwrap()].name, selected);
        }
    }
}

#[test]
fn small_windows_keep_the_existing_tables_and_servers_usable() {
    let mut term = Terminal::new(TestBackend::new(96, 30)).unwrap();
    let mut app = App::new(Box::new(FixtureSource::still()));
    app.server_mode = ServerMode::List;
    for _ in 0..3 {
        for (w, h) in [(40, 12), (40, 30), (60, 15), (80, 15), (80, 20), (150, 12)] {
            let hit = frame(&mut term, &mut app, w, h);
            assert!(hit.conn_h > 0 && hit.err_h > 0, "{w}x{h}");
            assert!(hit.side_by_side, "keep the existing two-pane layout");
            assert!(!hit.chips.is_empty(), "{w}x{h}");
            assert!(hit.strip_rows <= 2);
            assert!(hit.conn_list.bottom() < hit.server_list.top());
            assert!(hit.err_list.bottom() < hit.server_list.top());
            for (r, _) in hit.chips {
                assert!(r.right() < w && r.bottom() < h - 1);
            }
        }
        app.update(Action::ConnViewCycle);
    }
}

#[test]
fn server_list_uses_one_row_only_when_height_is_limited() {
    let mut term = Terminal::new(TestBackend::new(40, 30)).unwrap();
    let mut app = App::new(Box::new(FixtureSource::still()));
    app.server_mode = ServerMode::List;
    let tall = frame(&mut term, &mut app, 40, 30);
    assert_eq!(tall.strip_rows, 2);
    let short = frame(&mut term, &mut app, 40, 12);
    assert_eq!(short.strip_rows, 1);
    app.focus = Focus::Health;
    app.update(Action::Down);
    let next = frame(&mut term, &mut app, 40, 12);
    assert_eq!(next.strip_page, short.strip_page + 1);
    assert_ne!(next.chips, short.chips);
    let footer: String = (0..40).map(|x| term.backend().buffer()[(x, 11)].symbol()).collect();
    assert!(footer.contains("↑↓ page") && footer.contains("q quit"), "{footer}");
}

#[test]
fn tiny_resize_is_safe_in_all_views_and_footer_states() {
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    for state in 0..6 {
        let mut app = App::new(Box::new(FixtureSource::still()));
        app.server_mode = ServerMode::List;
        match state {
            1 => app.update(Action::ConnViewCycle),
            2 => { app.update(Action::ConnViewCycle); app.update(Action::ConnViewCycle); }
            3 => { app.update(Action::SearchOpen); for c in "a long search pattern".chars() { app.update(Action::SearchInput(c)); } }
            4 => { app.update(Action::Down); app.update(Action::Route(rowt_monitor::model::Lane::Escape)); }
            5 => app.update(Action::ToggleHelp),
            _ => {}
        }
        for w in (0..=100).rev() {
            for h in [30, 21, 20, 19, 15, 14, 12, 11, 2, 1, 0] {
                frame(&mut term, &mut app, w, h);
            }
        }
    }
}

#[test]
fn undersized_window_shows_hint_and_preserves_server_page() {
    let mut app = App::new(Box::new(FixtureSource::still()));
    app.server_mode = ServerMode::List;
    app.snap.chips = (0..20).map(|i| Server {
        down: false,
        name: format!("server-{i:02}-with-a-long-name-that-occupies-most-of-a-row"), ms: Some(i), active: false,
    }).collect();
    app.focus = Focus::Health;
    let mut term = Terminal::new(TestBackend::new(96, 30)).unwrap();
    frame(&mut term, &mut app, 96, 30);
    app.update(Action::Down);
    let before = frame(&mut term, &mut app, 96, 30);
    assert_eq!(before.strip_page, 1);
    let small = frame(&mut term, &mut app, 39, 30);
    assert!(small.chips.is_empty());
    assert_eq!(small.server_list, Rect::default());
    let text: String = term.backend().buffer().content.iter().map(|c| c.symbol()).collect();
    assert!(text.contains("Resize"), "{text}");
    assert_eq!(frame(&mut term, &mut app, 96, 30).chips, before.chips);
}
