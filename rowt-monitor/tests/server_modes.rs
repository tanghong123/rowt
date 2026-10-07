use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, Terminal};
use rowt_monitor::{app::{Action, App, Focus, ServerMode}, input, source::FixtureSource, ui};

fn draw(app: &mut App, w: u16, h: u16) -> ui::Hit {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    let mut hit = ui::Hit::default();
    term.draw(|f| { let area = f.area(); hit = ui::draw(f.buffer_mut(), area, app, false); }).unwrap();
    app.feed_strip(&hit);
    hit
}

#[test]
fn default_scroll_keeps_one_row_and_original_pane_heights() {
    let mut app = App::new(Box::new(FixtureSource::still()));
    assert_eq!(app.server_mode, ServerMode::Scroll);
    let scroll = draw(&mut app, 96, 41);
    assert_eq!(scroll.strip_rows, 1);
    assert_eq!(scroll.conn_h, 23);
    assert_eq!(scroll.err_h, 23);
    app.update(Action::ToggleServers);
    let list = draw(&mut app, 96, 41);
    assert_eq!(list.strip_rows, 2);
    assert_eq!(list.conn_h + 1, scroll.conn_h);
    assert_eq!(list.err_h + 1, scroll.err_h);
    app.update(Action::ToggleServers);
    let restored = draw(&mut app, 96, 41);
    assert_eq!(restored.conn_list, scroll.conn_list);
    assert_eq!(restored.err_list, scroll.err_list);
}

#[test]
fn g_toggles_modes_but_remains_text_in_the_search_editor() {
    let mut app = App::new(Box::new(FixtureSource::still()));
    for c in ['g', 'g'] {
        let action = input::key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), &app).unwrap();
        assert_eq!(action, Action::ToggleServers);
        app.update(action);
    }
    assert_eq!(app.server_mode, ServerMode::Scroll);
    assert_eq!(input::key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT), &app), None);
    app.update(Action::SearchOpen);
    assert_eq!(input::key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE), &app), Some(Action::SearchInput('G')));
}

#[test]
fn selected_server_survives_mode_switches_and_resizes() {
    let mut app = App::new(Box::new(FixtureSource::still()));
    app.focus = Focus::Health;
    draw(&mut app, 96, 30);
    app.update(Action::SelectServer(5));
    for _ in 0..4 {
        app.update(Action::ToggleServers);
        for (w, h) in [(40, 11), (96, 30), (150, 30)] {
            let hit = draw(&mut app, w, h);
            assert_eq!(app.strip_sel, Some(5));
            assert!(hit.chips.iter().any(|(_, i)| *i == 5), "{w}x{h} {:?}", app.server_mode);
        }
    }
}

#[test]
fn mode_switch_is_documented_in_help_and_small_footers() {
    let mut app = App::new(Box::new(FixtureSource::still()));
    for mode in [ServerMode::Scroll, ServerMode::List] {
        app.server_mode = mode;
        for focus in [Focus::Conn, Focus::Err, Focus::Health] {
            app.focus = focus;
            for (w, paused) in [(40, false), (96, false), (150, false), (40, true), (96, true), (150, true)] {
                app.paused = paused;
                let mut term = Terminal::new(TestBackend::new(w, 12)).unwrap();
                term.draw(|f| { let area = f.area(); ui::draw_footer(f.buffer_mut(), area, &app); }).unwrap();
                let text: String = term.backend().buffer().content.iter().map(|c| c.symbol()).collect();
                assert!(text.contains("g servers · ? help"), "{text}");
            }
        }
    }
    app.help = true;
    let text = rowt_monitor::render_app_text(&app, 150, 42);
    let lines: Vec<_> = text.lines().collect();
    let mode_line = lines.iter().position(|line| line.contains("g          servers: scroll / list")).unwrap();
    assert!(lines[mode_line + 1].contains("u          use the selected server"));
    assert!(!text.contains("G / g"));
}

#[test]
fn list_mode_has_its_own_golden_renders() {
    let mut app = App::new(Box::new(FixtureSource::still()));
    app.server_mode = ServerMode::List;
    for (w, h, golden) in [
        (96, 41, include_str!("../renders/rowt-monitor-list-96x30.txt")),
        (150, 30, include_str!("../renders/rowt-monitor-list-150x38.txt")),
        (212, 30, include_str!("../renders/rowt-monitor-list-212x52.txt")),
    ] {
        assert_eq!(rowt_monitor::render_app_text(&app, w, h), golden, "{w}x{h}");
    }
}

#[test]
fn servers_flag_selects_mode_and_rejects_invalid_values() {
    let run = |args: &[&str]| std::process::Command::new(env!("CARGO_BIN_EXE_rowt-monitor")).args(args).output().unwrap();
    let default = run(&["--render", "96x41"]);
    let scroll = run(&["--servers", "scroll", "--render", "96x41"]);
    let list = run(&["--servers", "list", "--render", "96x41"]);
    assert!(default.status.success() && scroll.status.success() && list.status.success());
    assert_eq!(default.stdout, scroll.stdout);
    assert_eq!(String::from_utf8(list.stdout).unwrap(), include_str!("../renders/rowt-monitor-list-96x30.txt"));
    for args in [vec!["--servers"], vec!["--servers", "other"]] {
        let invalid = run(&args);
        assert_eq!(invalid.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&invalid.stderr).contains("scroll|list"));
    }
    let help = run(&["--help"]);
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(help.contains("[--render-ansi WxH] [--servers scroll|list] [--version]"));
    assert!(help.contains("p pause · g servers · ? help · q quit"));
}

#[test]
fn scrolling_pending_and_down_servers_keep_their_status_and_frame_borders() {
    use rowt_monitor::model::Server;
    let mut app = App::new(Box::new(FixtureSource::still()));
    app.snap.chips = vec![
        Server { name: "active".into(), active: true, ms: None, down: false },
        Server { name: "failed".into(), active: false, ms: None, down: true },
    ];
    let text = rowt_monitor::render_app_text(&app, 96, 30);
    assert!(text.contains("active —") && text.contains("failed down"), "{text}");
    app.snap.chips[0].name = "东京🇯🇵".repeat(20);
    app.focus = Focus::Health;
    app.strip_sel = Some(0);
    for off in 0..15 {
        app.strip_off = off;
        for (w, h) in [(40, 11), (96, 30)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| { let area = f.area(); ui::draw(f.buffer_mut(), area, &app, false); }).unwrap();
            assert_eq!(term.backend().buffer()[(w - 1, h - 2)].symbol(), "│", "{w}x{h} offset {off}");
        }
    }
}

#[test]
fn page_keys_only_apply_in_list_mode_and_up_leaves_the_scrolling_strip() {
    let mut app = App::new(Box::new(FixtureSource::still()));
    app.focus = Focus::Health;
    assert_eq!(input::key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE), &app), None);
    app.update(Action::Up);
    assert_eq!(app.focus, Focus::Conn);
    app.update(Action::ToggleServers);
    app.focus = Focus::Health;
    assert_eq!(input::key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE), &app), Some(Action::PageServers(1)));
}
