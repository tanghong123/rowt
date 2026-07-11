//! Drive the update function for each interaction and assert the state change
//! (README "Interactions & keymap"). The fixture is deterministic (`still`):
//! 10 connections (6 escape / 1 corp / 3 direct), 10 error rows, window = 10m.

use rowt_monitor::app::{Action, App, Focus};
use rowt_monitor::model::{Lane, Window};
use rowt_monitor::source::FixtureSource;

fn app() -> App {
    // No real clipboard side effects during tests.
    std::env::set_var("ROWT_MONITOR_NO_CLIPBOARD", "1");
    let mut a = App::new(Box::new(FixtureSource::still()));
    // Simulate a drawn geometry so scroll clamping has real heights.
    a.conn_h = 6;
    a.err_h = 6;
    a.side_by_side = true;
    a
}

#[test]
fn move_selection_up_down() {
    let mut a = app();
    assert_eq!(a.conn_sel, 0);
    a.update(Action::Down);
    assert_eq!(a.conn_sel, 1);
    a.update(Action::Down);
    assert_eq!(a.conn_sel, 2);
    a.update(Action::Up);
    assert_eq!(a.conn_sel, 1);
    // clamp at top
    a.update(Action::Up);
    a.update(Action::Up);
    assert_eq!(a.conn_sel, 0);
}

#[test]
fn selection_scrolls_on_overflow() {
    let mut a = app(); // conn_h = 6, 10 rows
    for _ in 0..7 {
        a.update(Action::Down);
    }
    assert_eq!(a.conn_sel, 7);
    assert!(a.conn_scroll > 0, "list should have scrolled to keep sel visible");
    assert!(a.conn_sel < a.conn_scroll + a.conn_h);
}

#[test]
fn lane_filter_cycles_and_scopes_connections() {
    let mut a = app();
    assert_eq!(a.conns_view().len(), 10);
    a.update(Action::LaneCycle);
    assert_eq!(a.lane_filter, Some(Lane::Escape));
    assert_eq!(a.conns_view().len(), 6);
    a.update(Action::LaneCycle);
    assert_eq!(a.lane_filter, Some(Lane::Corp));
    assert_eq!(a.conns_view().len(), 1);
    a.update(Action::LaneCycle);
    assert_eq!(a.lane_filter, Some(Lane::Direct));
    assert_eq!(a.conns_view().len(), 3);
    a.update(Action::LaneCycle);
    assert_eq!(a.lane_filter, None);
    assert_eq!(a.conns_view().len(), 10);
}

#[test]
fn lane_jump_and_clear() {
    let mut a = app();
    a.update(Action::LaneSet(Some(Lane::Direct)));
    assert_eq!(a.lane_filter, Some(Lane::Direct));
    assert_eq!(a.conns_view().len(), 3);
    assert_eq!(a.conn_sel, 0, "filter resets selection");
    a.update(Action::LaneSet(None));
    assert_eq!(a.lane_filter, None);
}

#[test]
fn focus_switches_when_side_by_side() {
    let mut a = app();
    assert_eq!(a.focus, Focus::Conn);
    a.update(Action::FocusRight);
    assert_eq!(a.focus, Focus::Err);
    a.update(Action::FocusLeft);
    assert_eq!(a.focus, Focus::Conn);
    a.update(Action::CycleFocus);
    assert_eq!(a.focus, Focus::Err);
    a.update(Action::CycleFocus);
    assert_eq!(a.focus, Focus::Conn);
}

#[test]
fn stacked_left_right_is_noop_but_falls_through_vertically() {
    let mut a = app();
    a.side_by_side = false;
    a.update(Action::FocusRight);
    assert_eq!(a.focus, Focus::Conn, "left/right do nothing when stacked");

    // Down off the bottom of connections falls into errors' top.
    a.conn_sel = a.conns_view().len() - 1;
    a.update(Action::Down);
    assert_eq!(a.focus, Focus::Err);
    assert_eq!(a.err_sel, 0);

    // Up off the top of errors falls back into connections' bottom.
    a.update(Action::Up);
    assert_eq!(a.focus, Focus::Conn);
    assert_eq!(a.conn_sel, a.conns_view().len() - 1);
}

#[test]
fn window_cycle_and_step_and_set() {
    let mut a = app();
    assert_eq!(a.window, Window::M10);
    a.update(Action::WindowCycle);
    assert_eq!(a.window, Window::H1);
    a.update(Action::WindowStep(-1));
    assert_eq!(a.window, Window::M10);
    a.update(Action::WindowStep(-1));
    assert_eq!(a.window, Window::M5);
    a.update(Action::WindowStep(-1));
    assert_eq!(a.window, Window::H24, "wraps around");
    a.update(Action::WindowSet(Window::M10));
    assert_eq!(a.window, Window::M10);
}

#[test]
fn yank_copies_the_focused_key_field() {
    let mut a = app();
    // Connections: the domain of the selected row (no :port).
    assert_eq!(a.yank_target().as_deref(), Some("i.ytimg.com"));
    a.update(Action::Yank);
    assert_eq!(a.last_yank.as_deref(), Some("i.ytimg.com"));

    // Errors: the domain of the selected row.
    a.update(Action::FocusRight);
    assert_eq!(a.yank_target().as_deref(), Some("vpn.corp.example.com"));
    a.update(Action::Yank);
    assert_eq!(a.last_yank.as_deref(), Some("vpn.corp.example.com"));
}

#[test]
fn pause_and_help_toggle() {
    let mut a = app();
    assert!(!a.paused);
    a.update(Action::TogglePause);
    assert!(a.paused);
    a.update(Action::TogglePause);
    assert!(!a.paused);

    assert!(!a.help);
    a.update(Action::ToggleHelp);
    assert!(a.help);
}

#[test]
fn focus_and_selection_persist_across_reflow() {
    let mut a = app();
    a.update(Action::FocusRight);
    a.update(Action::Down);
    a.update(Action::Down);
    assert_eq!(a.focus, Focus::Err);
    assert_eq!(a.err_sel, 2);
    // A reflow only changes `side_by_side`; selection + focus are independent.
    a.side_by_side = false;
    assert_eq!(a.focus, Focus::Err);
    assert_eq!(a.err_sel, 2);
}
