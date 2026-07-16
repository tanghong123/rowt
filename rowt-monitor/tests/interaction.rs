//! Drive the update function for each interaction and assert the state change
//! (README "Interactions & keymap"). The fixture is deterministic (`still`):
//! 10 connections (6 escape / 1 corp / 3 direct), 10 error rows, window = 10m.

use rowt_monitor::app::{Action, App, Focus};
use rowt_monitor::model::{ConnView, Lane, MetricsBand, Window};
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
    // Deferred selection (§5.2): a focused pane starts with NO active selection;
    // the first ↑/↓ just activates it (locks row 0), it does not move.
    assert!(!a.conn_active());
    a.update(Action::Down);
    assert!(a.conn_active(), "first Down activates the selection");
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
    // 1 press activates at 0, then 7 more move to index 7.
    for _ in 0..8 {
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
    // Tab now cycles conns → errors → server health → conns (§5.1).
    a.update(Action::CycleFocus);
    assert_eq!(a.focus, Focus::Err);
    a.update(Action::CycleFocus);
    assert_eq!(a.focus, Focus::Health);
    a.update(Action::CycleFocus);
    assert_eq!(a.focus, Focus::Conn);
    // Shift+Tab reverses.
    a.update(Action::CycleFocusBack);
    assert_eq!(a.focus, Focus::Health);
}

#[test]
fn stacked_left_right_is_noop_but_falls_through_vertically() {
    let mut a = app();
    a.side_by_side = false;
    a.update(Action::FocusRight);
    assert_eq!(a.focus, Focus::Conn, "left/right do nothing when stacked");

    // Walk the connections selection to its last row, then Down falls into errors.
    // The pane list is the unified `rows` (live + historical), not just conns_view.
    a.update(Action::Down); // activate at 0
    for _ in 0..a.rows.len() {
        a.update(Action::Down);
    }
    assert_eq!(a.focus, Focus::Err);
    assert_eq!(a.err_sel, 0);

    // Up off the top of errors falls back into connections' bottom.
    a.update(Action::Up);
    assert_eq!(a.focus, Focus::Conn);
    assert_eq!(a.conn_sel, a.rows.len() - 1);
}

#[test]
fn window_cycle_and_step_and_set() {
    let mut a = app();
    // `w`/`[`/`]` cycle the errors window globally (no focus needed) — default
    // focus is the connections pane.
    assert_eq!(a.focus, Focus::Conn);
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

    // `s` from the Live view first engages the ↑ upload span view (band
    // unchanged) — matching `v`'s live→↑ ordering — so the bands become visible…
    assert!(a.conn_view.is_live());
    assert_eq!(a.band, MetricsBand::Recent);
    a.update(Action::BandCycle);
    assert_eq!(a.conn_view, ConnView::Up);
    assert_eq!(a.band, MetricsBand::Recent, "first s just reveals the spans");
    // …then `s` cycles the band.
    a.update(Action::BandCycle);
    assert_eq!(a.band, MetricsBand::Days);
    a.update(Action::BandCycle);
    assert_eq!(a.band, MetricsBand::Year);
    a.update(Action::BandCycle);
    assert_eq!(a.band, MetricsBand::Recent, "wraps");
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
    a.update(Action::Down); // activate at 0
    a.update(Action::Down); // -> 1
    assert_eq!(a.focus, Focus::Err);
    assert_eq!(a.err_sel, 1);
    // A reflow only changes `side_by_side`; selection + focus are independent.
    a.side_by_side = false;
    assert_eq!(a.focus, Focus::Err);
    assert_eq!(a.err_sel, 1);
}

// ---- control layer (CONTROLS.md) --------------------------------------------

#[test]
fn selection_activates_and_esc_clears() {
    let mut a = app();
    assert!(!a.conn_active(), "no selection until the first ↑/↓");
    assert_eq!(a.selected_domain(), None);
    a.update(Action::Down);
    assert!(a.conn_active());
    assert_eq!(a.selected_domain().as_deref(), Some("i.ytimg.com"));
    a.update(Action::Escape);
    assert!(!a.conn_active(), "esc clears the active selection");
    assert_eq!(a.selected_domain(), None);
}

#[test]
fn lane_edit_arms_then_double_tap_commits() {
    let mut a = app();
    a.update(Action::Down); // lock i.ytimg.com
    a.update(Action::Route(Lane::Block));
    assert!(a.armed.is_some(), "first press arms");
    assert!(a.pending_reload.is_none());
    a.update(Action::Route(Lane::Block));
    assert!(a.armed.is_none(), "second press commits + disarms");
    assert!(a.pending_reload.is_some(), "commit schedules the debounced reload");
}

#[test]
fn enter_confirms_armed_edit() {
    let mut a = app();
    a.update(Action::Down);
    a.update(Action::Route(Lane::Escape));
    assert!(a.armed.is_some());
    a.update(Action::Confirm);
    assert!(a.armed.is_none());
    assert!(a.pending_reload.is_some());
}

#[test]
fn any_other_key_cancels_the_arm() {
    let mut a = app();
    a.update(Action::Down);
    a.update(Action::Route(Lane::Corp));
    assert!(a.armed.is_some());
    a.update(Action::Down); // an unrelated key cancels the pending arm
    assert!(a.armed.is_none());
    assert!(a.pending_reload.is_none(), "cancelled arm never committed");
}

#[test]
fn route_is_inert_without_a_selection() {
    let mut a = app();
    // Focused but nothing selected → e/c/b/d do nothing.
    a.update(Action::Route(Lane::Escape));
    assert!(a.armed.is_none());
    a.update(Action::Unroute);
    assert!(a.armed.is_none());
}

#[test]
fn wheel_only_scrolls_when_over_a_pane() {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::layout::Rect;
    use rowt_monitor::input;
    use rowt_monitor::ui::Hit;

    let hit = Hit {
        conn_pane: Rect::new(0, 0, 50, 20),
        err_pane: Rect::new(50, 0, 50, 20),
        ..Default::default()
    };
    let wheel = |col, row| MouseEvent { kind: MouseEventKind::ScrollDown, column: col, row, modifiers: KeyModifiers::NONE };

    assert_eq!(input::mouse(wheel(10, 5), &hit), Some(Action::ScrollConn(1)), "over conns → scroll conns");
    assert_eq!(input::mouse(wheel(60, 5), &hit), Some(Action::ScrollErr(1)), "over errors → scroll errors");
    // Over neither pane (e.g. the identity band / server strip / footer): no-op.
    assert_eq!(input::mouse(wheel(10, 30), &hit), None, "wheel over nothing does not scroll");
    // Clicks elsewhere still return None (sanity that we didn't break the arm).
    let click = MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 10, row: 30, modifiers: KeyModifiers::NONE };
    assert_eq!(input::mouse(click, &hit), None);
}

#[test]
fn selection_auto_clears_after_idle_timeout() {
    use rowt_monitor::app::SELECTION_IDLE_TIMEOUT;
    use std::time::Instant;

    let mut a = app();
    a.update(Action::Down); // activate a selection (stamps last_input = now)
    assert!(a.conn_active());
    // Still within the idle window → selection survives.
    a.expire_idle_selection(Instant::now());
    assert!(a.conn_active());
    // Past the idle window with no further action → selection clears, list live again.
    a.expire_idle_selection(Instant::now() + SELECTION_IDLE_TIMEOUT + std::time::Duration::from_secs(1));
    assert!(!a.conn_active(), "idle selection auto-cleared");

    // A fresh action re-arms activity: after selecting again, a later expire that
    // is still within the window (because the action reset the timer) keeps it.
    a.update(Action::Down);
    assert!(a.conn_active());
    a.expire_idle_selection(Instant::now() + std::time::Duration::from_secs(5));
    assert!(a.conn_active(), "recent action keeps the selection alive");
}

#[test]
fn selection_is_forgotten_when_focus_leaves_the_pane() {
    let mut a = app();
    a.update(Action::Down); // activate the connections selection
    assert!(a.conn_active());
    a.update(Action::CycleFocus); // leave Conn → Err
    assert!(!a.conn_active(), "leaving a pane forgets its selection");
    a.update(Action::Down); // activate the errors selection
    assert!(a.err_active());
    a.update(Action::FocusLeft); // leave Err → Conn (side-by-side)
    assert!(!a.err_active(), "errors selection forgotten on leave");
    assert!(!a.conn_active(), "re-focusing Conn starts fresh, not restored");
}

#[test]
fn clicking_a_server_chip_focuses_and_selects_in_place() {
    let mut a = app();
    a.focus = Focus::Conn;
    a.update(Action::Down); // give Conn a selection first
    a.update(Action::SelectServer(3));
    assert_eq!(a.focus, Focus::Health, "click focuses the strip");
    assert_eq!(a.strip_sel, Some(3), "the clicked chip is selected");
    assert!(!a.conn_active(), "leaving Conn forgot its selection");
}

#[test]
fn strip_marquee_resumes_from_frozen_offset_on_unfreeze() {
    let mut a = app();
    a.focus = Focus::Health;
    // Pretend the renderer last drew the marquee at cell offset 7.
    a.strip_render_off = 7;
    a.update(Action::FocusRight); // first ←/→ freezes to that exact offset
    assert!(a.strip_sel.is_some());
    assert_eq!(a.strip_off, 7);
    // Unfreeze (Esc): the marquee baseline is set to the frozen offset so it
    // continues scrolling from there instead of jumping.
    a.update(Action::Escape);
    assert!(a.strip_sel.is_none());
    assert_eq!(a.marquee_off0, 7, "marquee resumes from the frozen offset, not a free-running clock");
}

#[test]
fn strip_selection_wraps_and_anchors() {
    let mut a = app();
    a.focus = Focus::Health;
    let n = a.snap.chips.len();
    assert!(n >= 3, "fixture has a server pool");
    // First ←/→ freezes the scroll and selects the first fully-visible chip.
    a.update(Action::FocusRight);
    assert_eq!(a.strip_sel, Some(0));
    // Walk to the last chip, then one more wraps to the first.
    for _ in 0..n - 1 {
        a.update(Action::FocusRight);
    }
    assert_eq!(a.strip_sel, Some(n - 1));
    a.update(Action::FocusRight);
    assert_eq!(a.strip_sel, Some(0), "wraps past the end back to the first");
    // Left from the first wraps to the last.
    a.update(Action::FocusLeft);
    assert_eq!(a.strip_sel, Some(n - 1), "wraps before the start to the last");
}

#[test]
fn esc_priority_arm_then_selection_then_filter() {
    let mut a = app();
    a.update(Action::LaneSet(Some(Lane::Escape)));
    a.update(Action::Down); // activate
    a.update(Action::Route(Lane::Block)); // arm
    // 1) esc cancels the arm first
    a.update(Action::Escape);
    assert!(a.armed.is_none());
    assert!(a.conn_active(), "selection survives the first esc");
    assert_eq!(a.lane_filter, Some(Lane::Escape));
    // 2) esc clears the selection next
    a.update(Action::Escape);
    assert!(!a.conn_active());
    assert_eq!(a.lane_filter, Some(Lane::Escape), "filter survives");
    // 3) esc finally clears the lane filter
    a.update(Action::Escape);
    assert_eq!(a.lane_filter, None);
}
