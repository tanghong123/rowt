use ratatui::{backend::TestBackend, Terminal};
use rowt_monitor::{app::{Action, App, Focus, ServerMode}, model::Server, source::FixtureSource, ui};

mod common;

fn app() -> App {
    let mut app = App::new(Box::new(FixtureSource::still()));
    app.server_mode = ServerMode::List;
    app.snap.chips = vec![
        Server { down: false, name: "slow-active-server-region".into(), ms: Some(200), active: true },
        Server { down: false, name: "unknown-server-region".into(), ms: None, active: false },
        Server { down: false, name: "fast-server-region".into(), ms: Some(10), active: false },
        Server { down: false, name: "medium-server-region".into(), ms: Some(50), active: false },
    ];
    app
}

fn draw(app: &App, w: u16, h: u16) -> (ratatui::buffer::Buffer, ui::Hit) {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    let mut hit = ui::Hit::default();
    term.draw(|f| { let area = f.area(); hit = ui::draw(f.buffer_mut(), area, app, false); }).unwrap();
    (term.backend().buffer().clone(), hit)
}

#[test]
fn pending_servers_are_visible_and_pageable_even_when_the_router_is_down() {
    let mut app = app();
    app.snap.servers_total = app.snap.chips.len() as u32;
    app.snap.servers_up = 0;
    app.snap.servers_down = 0;
    for server in &mut app.snap.chips { server.ms = None; }
    app.focus = Focus::Health;
    for router_up in [true, false] {
        app.snap.identity.router_up = router_up;
        app.strip_page = 0;
        let mut seen = Vec::new();
        for _ in 0..4 {
            let (buf, hit) = draw(&app, 40, 11);
            assert!(!hit.chips.is_empty());
            for (r, i) in &hit.chips {
                let text: String = (r.x..r.right()).map(|x| buf[(x, r.y)].symbol()).collect();
                assert!(text.ends_with(" —"), "{text}");
                seen.push(*i);
            }
            app.feed_strip(&hit);
            app.update(Action::Down);
        }
        assert_eq!(seen, app.server_order());
    }
}

#[test]
fn probe_age_is_visible_in_wide_and_minimum_windows_without_covering_paging() {
    let mut app = app();
    for (age, label) in [(None, "probe —"), (Some(12), "probe 12s ago"), (Some(125), "probe 2m ago")] {
        app.snap.probe_age = age;
        for (w, h) in [(150, 30), (40, 11)] {
            let (buf, hit) = draw(&app, w, h);
            let text: String = buf.content.iter().map(|c| c.symbol()).collect();
            assert!(text.contains(label), "{w}x{h}: missing {label}");
            if w == 40 {
                assert!(text.contains("↑↓ 1/4"));
                assert_eq!(hit.strip_rows, 1);
            }
        }
    }
}

#[test]
fn active_server_leads_and_remaining_servers_wrap_in_latency_order() {
    let app = app();
    let (_, hit) = draw(&app, 96, 30);
    assert_eq!(hit.chips.iter().map(|(_, i)| *i).collect::<Vec<_>>(), vec![0, 2, 3, 1]);
    assert!(hit.chips.last().unwrap().0.y > hit.chips[0].0.y);
    for (r, _) in &hit.chips {
        assert!(r.x >= 2 && r.right() <= 94 && r.bottom() < 30);
    }
    assert!(hit.conn_h >= 3, "leave room for connection rows");
}

#[test]
fn active_server_has_a_separator_in_list_mode_even_when_another_is_selected() {
    let mut app = app();
    for width in [64, 96] {
        for selected in [0, 2] {
            app.update(Action::SelectServer(selected));
            let (buf, hit) = draw(&app, width, 30);
            let active = hit.chips.iter().find(|(_, i)| *i == 0).unwrap().0;
            let separator = active.right() + 1;
            assert_eq!(buf[(separator, active.y)].symbol(), "│");
            assert_eq!(buf[(separator, active.y)].fg, rowt_monitor::theme::border());
            assert!(!hit.chips.iter().any(|(r, _)| r.y == active.y && r.x <= separator && separator < r.right()));
        }
    }
    app.snap.chips[0].name = "long-active-server".repeat(10);
    app.update(Action::SelectServer(0));
    let (buf, hit) = draw(&app, 40, 11);
    let active = hit.chips[0].0;
    assert_eq!(hit.chips[0].1, 0);
    assert_eq!(buf[(38, active.y)].symbol(), " ");
    assert_eq!(buf[(39, active.y)].symbol(), "│");
}

#[test]
fn active_server_stays_first_even_when_its_probe_fails() {
    let mut app = app();
    app.snap.chips[0].ms = None;
    app.snap.chips[0].down = true;
    let (buf, hit) = draw(&app, 96, 30);
    assert_eq!(hit.chips.iter().map(|(_, i)| *i).collect::<Vec<_>>(), vec![0, 2, 3, 1]);
    let r = hit.chips[0].0;
    let text: String = (r.x..r.right()).map(|x| buf[(x, r.y)].symbol()).collect();
    assert!(text.starts_with("▶ ") && text.ends_with(" down"), "{text}");
}

#[test]
fn server_positions_do_not_animate_or_move_when_selected() {
    let mut app = app();
    let (before, hit) = draw(&app, 96, 30);
    app.started -= std::time::Duration::from_secs(20);
    app.update(Action::SelectServer(2));
    let (after, selected) = draw(&app, 96, 30);
    assert_eq!(hit.chips, selected.chips);
    for (r, _) in &hit.chips {
        for x in r.x..r.right() {
            assert_eq!(before[(x, r.y)].symbol(), after[(x, r.y)].symbol());
        }
    }
}

#[test]
fn keyboard_follows_latency_order() {
    let mut app = app();
    app.focus = Focus::Health;
    for i in [0, 2, 3, 1, 0] {
        app.update(Action::FocusRight);
        assert_eq!(app.strip_sel, Some(i));
    }
    app.update(Action::FocusLeft);
    assert_eq!(app.strip_sel, Some(1));
}

#[test]
fn down_servers_sort_last_and_remain_selectable_but_cannot_be_used() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind, KeyModifiers};
    let source = common::Recording::new(common::Mode::Manual);
    let calls = source.calls.clone();
    let mut app = App::new(Box::new(source));
    app.server_mode = ServerMode::List;
    app.snap.chips = vec![
        Server { name: "a-down".into(), ms: None, down: true, active: false },
        Server { name: "z-pending".into(), ms: None, down: false, active: true },
        Server { name: "up".into(), ms: Some(20), down: false, active: false },
    ];
    assert_eq!(app.server_order(), vec![1, 2, 0]);
    let (buf, hit) = draw(&app, 96, 30);
    let (r, i) = *hit.chips.last().unwrap();
    let text: String = (r.x..r.right()).map(|x| buf[(x, r.y)].symbol()).collect();
    assert!(text.contains("a-down down"), "{text}");
    let click = MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: r.x, row: r.y,
        modifiers: KeyModifiers::NONE };
    app.update(rowt_monitor::input::mouse(click, &hit).unwrap());
    assert_eq!(app.strip_sel, Some(i));
    app.update(Action::UseServer);
    assert!(calls.lock().unwrap().is_empty());
    assert!(app.toast.as_ref().unwrap().0.contains("down"));
    app.update(Action::FocusRight);
    assert_eq!(app.strip_sel, Some(1));
    app.update(Action::FocusLeft);
    assert_eq!(app.strip_sel, Some(0), "keyboard can select down servers too");
    app.snap.chips[0].down = false;
    app.snap.chips[0].ms = Some(30);
    app.update(Action::UseServer);
    assert_eq!(*calls.lock().unwrap(), vec!["a-down"]);
}

#[test]
fn a_selected_server_that_goes_down_cannot_be_used() {
    let source = common::Recording::new(common::Mode::Manual);
    let calls = source.calls.clone();
    let mut app = App::new(Box::new(source));
    app.server_mode = ServerMode::List;
    app.update(Action::SelectServer(1));
    app.snap.chips[1].down = true;
    app.snap.chips[1].ms = None;
    app.update(Action::UseServer);
    assert_eq!(app.strip_sel, Some(1));
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn polling_keeps_selection_on_the_same_server() {
    let mut app = App::new(Box::new(FixtureSource::still()));
    app.server_mode = ServerMode::List;
    app.snap.chips.reverse();
    let name = app.snap.chips[0].name.clone();
    app.update(Action::SelectServer(0));
    app.tick();
    assert_eq!(app.snap.chips[app.strip_sel.unwrap()].name, name);
    app.snap.chips[app.strip_sel.unwrap()].name = "removed".into();
    app.tick();
    assert_eq!(app.strip_sel, None);
}

#[test]
fn server_pages_have_at_most_two_rows_and_cover_the_pool() {
    let mut app = app();
    app.snap.chips = (0..20).map(|i| Server {
        down: false,
        name: format!("server-{i:02}-with-a-long-name-that-occupies-most-of-a-row"), ms: Some(i + 10), active: i == 19,
    }).collect();
    app.focus = Focus::Health;
    let mut seen = Vec::new();
    for _ in 0..10 {
        let (_, hit) = draw(&app, 96, 30);
        let mut rows: Vec<_> = hit.chips.iter().map(|(r, _)| r.y).collect();
        rows.sort();
        rows.dedup();
        assert!(rows.len() <= 2);
        seen.extend(hit.chips.iter().map(|(_, i)| *i));
        app.feed_strip(&hit);
        app.update(Action::Down);
    }
    assert_eq!(seen, std::iter::once(19).chain(0..19).collect::<Vec<_>>());
    app.update(Action::Up);
    let (_, hit) = draw(&app, 96, 30);
    assert_eq!(hit.chips[0].1, 15);
}

#[test]
fn paging_keys_and_mouse_use_the_server_list() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
    use rowt_monitor::input;
    let mut app = app();
    app.focus = Focus::Health;
    let (_, hit) = draw(&app, 96, 30);
    for (key, step) in [(KeyCode::PageUp, -1), (KeyCode::PageDown, 1)] {
        assert_eq!(input::key(KeyEvent::new(key, KeyModifiers::NONE), &app), Some(Action::PageServers(step)));
    }
    for (kind, step) in [(MouseEventKind::ScrollUp, -1), (MouseEventKind::ScrollDown, 1)] {
        let mouse = MouseEvent { kind, column: hit.server_list.x, row: hit.server_list.y, modifiers: KeyModifiers::NONE };
        assert_eq!(input::mouse(mouse, &hit), Some(Action::PageServers(step)));
    }
    for (rect, i) in hit.chips {
        let mouse = MouseEvent { kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: rect.x, row: rect.y, modifiers: KeyModifiers::NONE };
        let (_, targets) = draw(&app, 96, 30);
        assert_eq!(input::mouse(mouse, &targets), Some(Action::SelectServer(i)));
    }
}

#[test]
fn oversized_unicode_name_keeps_latency_inside_the_frame() {
    let mut app = app();
    app.snap.chips = vec![Server { down: false, name: "东京🇯🇵".repeat(40), ms: Some(1234), active: true }];
    let (buf, hit) = draw(&app, 96, 30);
    let (r, _) = hit.chips[0];
    let text: String = (r.x..r.right()).map(|x| buf[(x, r.y)].symbol()).collect();
    assert!(!text.contains('…') && text.contains("1234 ms"), "{text}");
    assert_eq!(buf[(95, r.y)].symbol(), "│");
    assert_eq!(hit.strip_rows, 1);
}

#[test]
fn oversized_server_name_shows_its_prefix_and_uses_the_full_name() {
    let source = common::Recording::new(common::Mode::Manual);
    let calls = source.calls.clone();
    let mut app = App::new(Box::new(source));
    app.server_mode = ServerMode::List;
    let name = format!("abcdefghij{}vwxyz", "middle".repeat(20));
    app.snap.chips = vec![Server { down: false, name: name.clone(), ms: Some(1234), active: false }];
    let (buf, hit) = draw(&app, 40, 11);
    let (r, i) = hit.chips[0];
    let text: String = (r.x..r.right()).map(|x| buf[(x, r.y)].symbol()).collect();
    assert_eq!(text, format!("{} 1234 ms", &name[..28]));
    assert!(!text.contains('…') && !text.contains("vwxyz"), "{text}");
    app.update(Action::SelectServer(i));
    app.update(Action::UseServer);
    assert_eq!(*calls.lock().unwrap(), vec![name]);
}

#[test]
fn oversized_unicode_name_is_clipped_without_ellipsis_and_preserves_latency() {
    let mut app = app();
    app.snap.chips = vec![Server { down: false, name: format!("东京{}末尾节点一", "中间".repeat(40)), ms: Some(1234), active: true }];
    let (buf, hit) = draw(&app, 40, 11);
    let r = hit.chips[0].0;
    let text: String = (r.x..r.right()).map(|x| buf[(x, r.y)].symbol()).collect();
    let visible = text.replace(' ', "");
    assert!(visible.starts_with("▶东京"), "{text}");
    assert!(visible.starts_with("▶东京中间中间中间中间中间中"), "{text}");
    assert!(visible.ends_with("1234ms"), "{text}");
    assert!(!visible.contains('…') && !visible.contains("末尾"), "{text}");
    assert_eq!(buf[(39, r.y)].symbol(), "│");
}

#[test]
fn equal_latencies_sort_by_name_and_missing_readings_come_last() {
    let mut app = app();
    app.snap.chips[0].ms = Some(10);
    app.snap.chips[3].ms = Some(10);
    let (_, hit) = draw(&app, 96, 30);
    assert_eq!(hit.chips.iter().map(|(_, i)| *i).collect::<Vec<_>>(), vec![0, 2, 3, 1]);
}

#[test]
fn selection_stays_visible_on_resize_and_page_is_clamped_when_pool_shrinks() {
    let mut app = app();
    app.snap.chips = (0..20).map(|i| Server {
        down: false,
        name: format!("server-{i:02}-with-a-long-name-that-occupies-most-of-a-row"), ms: Some(i), active: false,
    }).collect();
    app.update(Action::SelectServer(19));
    for (w, h) in [(96, 30), (150, 30), (212, 20)] {
        let (_, hit) = draw(&app, w, h);
        assert!(hit.chips.iter().any(|(_, i)| *i == 19));
        assert!(hit.strip_rows <= 2);
        app.feed_strip(&hit);
    }
    app.update(Action::Escape);
    app.snap.chips.truncate(1);
    let (_, hit) = draw(&app, 96, 30);
    assert_eq!(hit.strip_page, 0);
    assert_eq!(hit.chips.len(), 1);
}
