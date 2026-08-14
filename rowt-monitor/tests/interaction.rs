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
    // …and neither do their shifted forms.
    a.update(Action::RouteSuffix(Lane::Escape));
    assert!(a.armed.is_none());
    a.update(Action::UnrouteSuffix);
    assert!(a.armed.is_none());
}

#[test]
fn shifted_lane_edit_arms_the_parent_suffix() {
    let mut a = app();
    a.update(Action::Down); // lock i.ytimg.com
    a.update(Action::RouteSuffix(Lane::Escape));
    let armed = a.armed.clone().expect("first press arms");
    assert_eq!(armed.domain, "ytimg.com", "the edit targets the suffix, not the host");
    assert_eq!(armed.key, 'E');
    assert_eq!(armed.label(), "ytimg.com → escape", "the confirm bar shows the literal entry");
    a.update(Action::RouteSuffix(Lane::Escape));
    assert!(a.armed.is_none(), "second press commits + disarms");
    assert!(a.pending_reload.is_some(), "commit schedules the debounced reload");
}

#[test]
fn shifted_unroute_arms_the_parent_suffix() {
    let mut a = app();
    a.update(Action::Down);
    a.update(Action::UnrouteSuffix);
    let armed = a.armed.clone().expect("D arms");
    // Symmetric undo of `E`/`C`/`B`: it removes the suffix ENTRY, not everything
    // the suffix would cover (lane `rm` is an exact-line match).
    assert_eq!(armed.domain, "ytimg.com");
    assert_eq!(armed.lane, None);
    assert_eq!(armed.key, 'D');
}

#[test]
fn the_two_forms_never_cross_commit() {
    let mut a = app();
    a.update(Action::Down);
    a.update(Action::Route(Lane::Corp)); // c → i.ytimg.com
    a.update(Action::RouteSuffix(Lane::Corp)); // C → .ytimg.com: a different edit
    assert!(a.armed.is_some(), "the shifted form re-arms rather than committing");
    assert_eq!(a.armed.as_ref().unwrap().domain, "ytimg.com");
    assert!(a.pending_reload.is_none(), "nothing was written");
}

#[test]
fn shift_is_inert_when_there_is_nothing_to_broaden() {
    let mut a = app();
    a.update(Action::FocusErr);
    // Walk to the fixture's two-label error row.
    for _ in 0..20 {
        a.update(Action::Down);
        if a.selected_domain().as_deref() == Some("x.com") {
            break;
        }
    }
    assert_eq!(a.selected_domain().as_deref(), Some("x.com"));
    a.update(Action::RouteSuffix(Lane::Escape));
    assert!(a.armed.is_none(), "x.com IS its own parent suffix — nothing broader to add");
    assert!(
        a.toast.as_ref().is_some_and(|(m, _)| m.contains("no parent suffix")),
        "and it says so rather than silently doing the lowercase edit"
    );
    // The lowercase form is unaffected — it still adds the host itself.
    a.update(Action::Route(Lane::Escape));
    assert_eq!(a.armed.as_ref().unwrap().domain, "x.com");
}

#[test]
fn shifted_control_keys_map_to_the_suffix_actions() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use rowt_monitor::input;
    let a = app();
    // Terminals deliver a shifted letter as an uppercase `Char` (with or without
    // the SHIFT modifier set, depending on the terminal) — both must map.
    let key = |c: char, m: KeyModifiers| input::key(KeyEvent::new(KeyCode::Char(c), m), &a);
    for m in [KeyModifiers::NONE, KeyModifiers::SHIFT] {
        assert_eq!(key('E', m), Some(Action::RouteSuffix(Lane::Escape)));
        assert_eq!(key('C', m), Some(Action::RouteSuffix(Lane::Corp)));
        assert_eq!(key('B', m), Some(Action::RouteSuffix(Lane::Block)));
        assert_eq!(key('D', m), Some(Action::UnrouteSuffix));
    }
    // The lowercase four are untouched, and Ctrl-C still quits.
    assert_eq!(key('e', KeyModifiers::NONE), Some(Action::Route(Lane::Escape)));
    assert_eq!(key('d', KeyModifiers::NONE), Some(Action::Unroute));
    assert_eq!(key('c', KeyModifiers::CONTROL), Some(Action::Quit));
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

// ---- host search (`/`) ------------------------------------------------------

#[test]
fn search_editor_cursor_and_edits() {
    let mut a = app();
    a.update(Action::SearchOpen);
    assert!(a.search.editing);
    for c in "abc".chars() {
        a.update(Action::SearchInput(c));
    }
    assert_eq!(a.search.buf, "abc");
    assert_eq!(a.search.cursor, 3);
    // Mid-string insert: move left one, type X → "abXc".
    a.update(Action::SearchCursor(-1));
    assert_eq!(a.search.cursor, 2);
    a.update(Action::SearchInput('X'));
    assert_eq!(a.search.buf, "abXc");
    assert_eq!(a.search.cursor, 3);
    // Backspace removes the char before the cursor (X) → "abc".
    a.update(Action::SearchBackspace);
    assert_eq!(a.search.buf, "abc");
    assert_eq!(a.search.cursor, 2);
    // Delete removes the char AT the cursor (c) → "ab".
    a.update(Action::SearchDelete);
    assert_eq!(a.search.buf, "ab");
    assert_eq!(a.search.cursor, 2);
    // Home / End.
    a.update(Action::SearchHome);
    assert_eq!(a.search.cursor, 0);
    a.update(Action::SearchEnd);
    assert_eq!(a.search.cursor, 2);
    // Ctrl-U clears.
    a.update(Action::SearchKillLine);
    assert_eq!(a.search.buf, "");
    assert_eq!(a.search.cursor, 0);
}

#[test]
fn search_filters_both_panes_incrementally_and_persists() {
    let mut a = app();
    let rows_before = a.rows.len();
    a.update(Action::SearchOpen);
    for c in "google".chars() {
        a.update(Action::SearchInput(c));
    }
    // Incremental (still editing): both panes already filtered off the buffer.
    assert!(!a.visible_rows().is_empty());
    assert!(a.visible_rows().iter().all(|r| r.host.to_lowercase().contains("google")));
    assert!(a.errors_view().iter().all(|e| e.domain.to_lowercase().contains("google")));
    // Header aggregate is untouched: the full row set is unchanged.
    assert_eq!(a.rows.len(), rows_before, "search must not touch the header row set");
    // Commit persists across a view flip.
    a.update(Action::SearchCommit);
    assert!(!a.search.editing);
    assert!(a.search_committed());
    a.update(Action::ConnViewCycle);
    assert!(a.visible_rows().iter().all(|r| r.host.to_lowercase().contains("google")));
}

#[test]
fn search_composes_and_with_lane_filter() {
    let mut a = app();
    a.update(Action::LaneSet(Some(Lane::Direct)));
    a.update(Action::SearchOpen);
    for c in "google".chars() {
        a.update(Action::SearchInput(c));
    }
    a.update(Action::SearchCommit);
    // AND: every visible row is BOTH the direct lane AND matches "google".
    assert!(!a.visible_rows().is_empty());
    assert!(a.visible_rows().iter().all(|r| r.lane == Lane::Direct && r.host.to_lowercase().contains("google")));
}

#[test]
fn search_invalid_regex_falls_back_to_substring() {
    let mut a = app();
    a.update(Action::SearchOpen);
    // A dangling group is not a valid regex → literal substring; nothing contains
    // the literal "google(", so it filters to empty without panicking.
    for c in "google(".chars() {
        a.update(Action::SearchInput(c));
    }
    assert!(a.visible_rows().is_empty(), "literal-substring fallback matches nothing here");
    // A valid regex still works once the pattern compiles again.
    a.update(Action::SearchKillLine);
    for c in "google".chars() {
        a.update(Action::SearchInput(c));
    }
    assert!(!a.visible_rows().is_empty());
    assert!(a.visible_rows().iter().all(|r| r.host.to_lowercase().contains("google")));
}

#[test]
fn search_cancel_reverts_and_esc_clears() {
    let mut a = app();
    a.update(Action::SearchOpen);
    for c in "google".chars() {
        a.update(Action::SearchInput(c));
    }
    a.update(Action::SearchCommit);
    let committed_n = a.visible_rows().len();
    assert!(committed_n > 0 && a.search_committed());
    // Re-open (prefilled), edit, then cancel → reverts to the committed pattern.
    a.update(Action::SearchOpen);
    assert_eq!(a.search.buf, "google", "editor opens prefilled with the committed pattern");
    a.update(Action::SearchInput('X'));
    a.update(Action::SearchCancel);
    assert!(!a.search.editing);
    assert_eq!(a.search.committed, "google");
    assert_eq!(a.visible_rows().len(), committed_n, "cancel reverts to the committed filter");
    // Esc (nothing focused) clears the committed filter entirely.
    a.update(Action::Escape);
    assert!(!a.search_committed());
    assert_eq!(a.search.committed, "");
    assert!(a.visible_rows().len() > committed_n, "clearing the filter restores the full list");
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

// --- editable confirm bar -------------------------------------------------
// The armed entry is a one-line editor: ↵ applies whatever is in it. Editing is
// additive — while the buffer is untouched every previous armed behaviour holds.

use rowt_monitor::app::Edit;

/// Replace the whole entry, now that there is no kill-line: the cursor sits at
/// the left, so forward-Delete eats it and the typed text lands in its place.
fn retype(a: &mut App, to: &str) {
    let n = a.armed.as_ref().unwrap().domain.chars().count();
    for _ in 0..n {
        a.update(Action::ArmEdit(Edit::Delete));
    }
    for c in to.chars() {
        a.update(Action::ArmEdit(Edit::Insert(c)));
    }
}

/// The toast is written from the same `entry` string that's handed to the
/// source, so it's a faithful witness for what would be applied.
fn applied(a: &App) -> String {
    a.toast.as_ref().map(|(m, _)| m.clone()).unwrap_or_default()
}

#[test]
fn armed_entry_can_be_edited_and_enter_applies_the_edit() {
    let mut a = app();
    a.update(Action::Down); // lock i.ytimg.com
    a.update(Action::Route(Lane::Escape));
    assert_eq!(a.armed.as_ref().unwrap().domain, "i.ytimg.com");
    // The cursor is already at the left, so Delete eats the leading label…
    for _ in 0.."i.".len() {
        a.update(Action::ArmEdit(Edit::Delete));
    }
    assert_eq!(a.armed.as_ref().unwrap().domain, "ytimg.com");
    // …and typing there puts a new one in its place.
    for c in "img.".chars() {
        a.update(Action::ArmEdit(Edit::Insert(c)));
    }
    assert_eq!(a.armed.as_ref().unwrap().domain, "img.ytimg.com");
    a.update(Action::Confirm);
    assert!(a.armed.is_none());
    assert_eq!(applied(&a), "img.ytimg.com → escape", "the EDITED entry is what gets applied");
    assert!(a.pending_reload.is_some());
}

#[test]
fn double_tap_still_commits_the_unedited_entry() {
    let mut a = app();
    a.update(Action::Down);
    a.update(Action::Route(Lane::Escape));
    a.update(Action::Route(Lane::Escape));
    assert_eq!(applied(&a), "i.ytimg.com → escape", "double-tap = no edit, then apply");
}

#[test]
fn ctrl_w_drops_the_leading_label() {
    let mut a = app();
    a.update(Action::Down);
    a.update(Action::Route(Lane::Block)); // i.ytimg.com
    // Narrowing a hostname means dropping labels off the FRONT, which is also
    // what `E` computes automatically — `^W` is the manual version of it.
    a.update(Action::ArmEdit(Edit::DropLabel));
    assert_eq!(a.armed.as_ref().unwrap().domain, "ytimg.com");
    a.update(Action::ArmEdit(Edit::DropLabel));
    assert_eq!(a.armed.as_ref().unwrap().domain, "com");
    // No dot left: a no-op, not an emptied field.
    a.update(Action::ArmEdit(Edit::DropLabel));
    assert_eq!(a.armed.as_ref().unwrap().domain, "com");
    assert_eq!(a.armed.as_ref().unwrap().cursor, 0, "and the cursor stays at the left");
}

/// The cursor starts at the LEFT, so the first thing typed lands at the front.
#[test]
fn the_cursor_starts_at_the_left() {
    let mut a = app();
    a.update(Action::Down);
    a.update(Action::Route(Lane::Escape));
    assert_eq!(a.armed.as_ref().unwrap().cursor, 0);
    a.update(Action::ArmEdit(Edit::Insert('x')));
    assert_eq!(a.armed.as_ref().unwrap().domain, "xi.ytimg.com");
}

#[test]
fn editing_to_empty_cancels_instead_of_applying() {
    let mut a = app();
    a.update(Action::Down);
    a.update(Action::Route(Lane::Escape));
    let n = a.armed.as_ref().unwrap().domain.chars().count();
    for _ in 0..n {
        a.update(Action::ArmEdit(Edit::Delete));
    }
    a.update(Action::Confirm);
    assert!(a.armed.is_none());
    assert!(applied(&a).contains("empty entry"), "an empty buffer is a cancel: {:?}", applied(&a));
    assert!(a.pending_reload.is_none(), "nothing was written");
}

#[test]
fn editing_restarts_the_arm_timeout() {
    use std::time::{Duration, Instant};
    let mut a = app();
    a.update(Action::Down);
    a.update(Action::Route(Lane::Escape));
    // Backdate the arm to just inside the window, then type.
    a.armed.as_mut().unwrap().at = Instant::now() - Duration::from_secs(4);
    a.update(Action::ArmEdit(Edit::Insert('x')));
    assert!(
        a.armed.as_ref().unwrap().at.elapsed() < Duration::from_millis(500),
        "the 5s window measures inactivity, not total time spent typing"
    );
}

#[test]
fn the_confirm_bar_claims_text_keys_but_not_the_arm_keys_until_edited() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use rowt_monitor::input;
    let mut a = app();
    a.update(Action::Down);
    a.update(Action::Route(Lane::Escape));
    let k = |a: &App, c: KeyCode, m: KeyModifiers| input::key(KeyEvent::new(c, m), a);
    let n = KeyModifiers::NONE;

    // Pristine: the eight arm keys keep their meaning, so double-tap and re-arm
    // survive; every other printable starts an edit.
    assert_eq!(k(&a, KeyCode::Char('e'), n), Some(Action::Route(Lane::Escape)));
    assert_eq!(k(&a, KeyCode::Char('C'), n), Some(Action::RouteSuffix(Lane::Corp)));
    assert_eq!(k(&a, KeyCode::Char('x'), n), Some(Action::ArmEdit(Edit::Insert('x'))));
    assert_eq!(k(&a, KeyCode::Backspace, n), Some(Action::ArmEdit(Edit::Backspace)));
    assert_eq!(k(&a, KeyCode::Left, n), Some(Action::ArmEdit(Edit::Cursor(-1))));
    assert_eq!(k(&a, KeyCode::Char('w'), KeyModifiers::CONTROL), Some(Action::ArmEdit(Edit::DropLabel)));
    // ↵ / Esc stay with the global keymap.
    assert_eq!(k(&a, KeyCode::Enter, n), Some(Action::Confirm));
    assert_eq!(k(&a, KeyCode::Esc, n), Some(Action::Escape));
    // Ctrl-C cancels the edit rather than quitting.
    assert_eq!(k(&a, KeyCode::Char('c'), KeyModifiers::CONTROL), Some(Action::Escape));
    // Keys that aren't text still fall through — and so still cancel the arm.
    assert_eq!(k(&a, KeyCode::Down, n), Some(Action::Down));

    // Once edited, the arm keys type like anything else.
    a.update(Action::ArmEdit(Edit::Insert('x')));
    assert_eq!(k(&a, KeyCode::Char('e'), n), Some(Action::ArmEdit(Edit::Insert('e'))));
    assert_eq!(k(&a, KeyCode::Char('D'), n), Some(Action::ArmEdit(Edit::Insert('D'))));
    // …but ↵ and Esc are unchanged, so there's always a way out.
    assert_eq!(k(&a, KeyCode::Enter, n), Some(Action::Confirm));
    assert_eq!(k(&a, KeyCode::Esc, n), Some(Action::Escape));
}

#[test]
fn no_armed_edit_leaves_the_global_keymap_alone() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use rowt_monitor::input;
    let a = app();
    assert!(a.armed.is_none());
    let k = |c| input::key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), &a);
    assert_eq!(k('q'), Some(Action::Quit));
    assert_eq!(k('y'), Some(Action::Yank));
    assert_eq!(k('?'), Some(Action::ToggleHelp));
}

#[test]
fn an_entry_with_a_space_is_refused_not_silently_closed_up() {
    let mut a = app();
    a.update(Action::Down);
    a.update(Action::Route(Lane::Escape));
    a.update(Action::ArmEdit(Edit::Home));
    a.update(Action::ArmEdit(Edit::Insert('x')));
    a.update(Action::ArmEdit(Edit::Insert(' ')));
    assert_eq!(a.armed.as_ref().unwrap().domain, "x i.ytimg.com");
    a.update(Action::Confirm);
    // bash `edit_list` would have written "xi.ytimg.com" — something the bar
    // never showed. Refusing keeps preview and write identical.
    assert!(applied(&a).contains("has a space"), "{:?}", applied(&a));
    assert!(a.pending_reload.is_none(), "nothing was written");
    // Surrounding whitespace is fine — it's only trimmed.
    a.update(Action::Route(Lane::Escape));
    a.update(Action::ArmEdit(Edit::End));
    a.update(Action::ArmEdit(Edit::Insert(' ')));
    a.update(Action::Confirm);
    assert_eq!(applied(&a), "i.ytimg.com → escape");
}

#[test]
fn an_idle_edit_cancels_itself_like_esc() {
    use rowt_monitor::app::ARM_TIMEOUT;
    use std::time::{Duration, Instant};
    let mut a = app();
    a.update(Action::Down);
    a.update(Action::Route(Lane::Escape));
    a.update(Action::ArmEdit(Edit::Insert('x')));

    // The timer is measured from the LAST keypress, so typing keeps it alive —
    // a pause to think about a domain must not discard the work.
    a.armed.as_mut().unwrap().at = Instant::now() - ARM_TIMEOUT + Duration::from_secs(2);
    a.on_frame();
    assert!(a.armed.is_some(), "still within the idle window");
    a.update(Action::ArmEdit(Edit::Insert('y')));
    assert!(a.armed.as_ref().unwrap().at.elapsed() < Duration::from_millis(500), "a keypress restamps it");

    // Idle past the window: gone, and silently — this is Esc, which says nothing.
    let before = a.toast.clone().map(|(m, _)| m);
    a.armed.as_mut().unwrap().at = Instant::now() - ARM_TIMEOUT - Duration::from_secs(1);
    a.on_frame();
    assert!(a.armed.is_none(), "an idle edit cancels itself");
    assert_eq!(a.toast.clone().map(|(m, _)| m), before, "silently, like Esc");
    assert!(a.pending_reload.is_none(), "and writes nothing");
}

#[test]
fn an_over_broad_entry_is_refused() {
    // A lane entry is a domain_suffix, so `com` is every .com — and the editor
    // puts that one ^U and three keystrokes away.
    let mut a = app();
    a.update(Action::Down);
    a.update(Action::Route(Lane::Escape));
    retype(&mut a, "com");
    a.update(Action::Confirm);
    assert!(a.armed.is_none());
    assert!(applied(&a).contains("whole top-level domain"), "{:?}", applied(&a));
    assert!(a.pending_reload.is_none(), "nothing was written");

    // A registry suffix is the same mistake one label further down.
    a.update(Action::Route(Lane::Escape));
    retype(&mut a, "co.uk");
    a.update(Action::Confirm);
    assert!(applied(&a).contains("whole registry namespace"), "{:?}", applied(&a));
    assert!(a.pending_reload.is_none());

    // …but a real domain of the same shape still applies.
    a.update(Action::Route(Lane::Escape));
    retype(&mut a, "bbc.co.uk");
    a.update(Action::Confirm);
    assert_eq!(applied(&a), "bbc.co.uk → escape");
    assert!(a.pending_reload.is_some());
}
