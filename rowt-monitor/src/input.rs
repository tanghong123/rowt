//! Map raw crossterm events to `Action`s (README "Interactions & keymap").

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::app::{Action, App};
use crate::model::Lane;
use crate::ui::Hit;

pub fn key(k: KeyEvent, app: &App) -> Option<Action> {
    if k.kind == KeyEventKind::Release {
        return None;
    }
    // Help overlay swallows everything except its own toggle and quit.
    if app.help {
        return match k.code {
            KeyCode::Char('?') | KeyCode::Esc => Some(Action::ToggleHelp),
            KeyCode::Char('q') => Some(Action::Quit),
            _ => None,
        };
    }
    Some(match k.code {
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => Action::Quit,
        KeyCode::Up | KeyCode::Char('k') => Action::Up,
        KeyCode::Down | KeyCode::Char('j') => Action::Down,
        KeyCode::Left | KeyCode::Char('h') => Action::FocusLeft,
        KeyCode::Right | KeyCode::Char('l') => Action::FocusRight,
        KeyCode::Tab => Action::CycleFocus,
        KeyCode::BackTab => Action::CycleFocusBack,
        KeyCode::Char('f') => Action::LaneCycle,
        KeyCode::Char('1') => Action::LaneSet(Some(Lane::Escape)),
        KeyCode::Char('2') => Action::LaneSet(Some(Lane::Corp)),
        KeyCode::Char('3') => Action::LaneSet(Some(Lane::Direct)),
        KeyCode::Char('0') => Action::LaneSet(None),
        KeyCode::Char('v') => Action::ConnViewCycle,
        KeyCode::Char('w') => Action::WindowCycle,
        KeyCode::Char(']') => Action::WindowStep(1),
        KeyCode::Char('[') => Action::WindowStep(-1),
        KeyCode::Char('y') => Action::Yank,
        KeyCode::Char('r') => Action::ForceProbe,
        KeyCode::Char('p') => Action::TogglePause,
        KeyCode::Char('?') => Action::ToggleHelp,
        // Control layer (CONTROLS.md): contextual routing edits (armed → commit),
        // immediate server switch / proxy toggle, confirm + cancel.
        KeyCode::Char('e') => Action::Route(Lane::Escape),
        KeyCode::Char('c') => Action::Route(Lane::Corp),
        KeyCode::Char('b') => Action::Route(Lane::Block),
        KeyCode::Char('d') => Action::Unroute,
        KeyCode::Char('u') => Action::UseServer,
        KeyCode::Char('o') => Action::ToggleProxy,
        KeyCode::Enter => Action::Confirm,
        KeyCode::Esc => Action::Escape,
        _ => return None,
    })
}

pub fn mouse(m: MouseEvent, hit: &Hit) -> Option<Action> {
    let (col, row) = (m.column, m.row);
    match m.kind {
        // Wheel scrolls (and focuses) the list *under the pointer* — and only if
        // the pointer is actually over a pane; over anything else it's a no-op.
        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
            let d = if matches!(m.kind, MouseEventKind::ScrollDown) { 1 } else { -1 };
            if in_rect(hit.err_pane, col, row) || in_rect(hit.err_list, col, row) {
                Some(Action::ScrollErr(d))
            } else if in_rect(hit.conn_pane, col, row) || in_rect(hit.conn_list, col, row) {
                Some(Action::ScrollConn(d))
            } else {
                None
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            // "sys proxy on/off" toggles when clicked.
            if in_rect(hit.sysproxy, col, row) {
                return Some(Action::ToggleProxy);
            }
            for (r, win) in &hit.windows {
                if in_rect(*r, col, row) {
                    return Some(Action::WindowSet(*win));
                }
            }
            for (r, lane) in &hit.lanes {
                if in_rect(*r, col, row) {
                    return Some(Action::LaneSet(*lane));
                }
            }
            // Click a server chip → focus the strip + select it in place.
            for (r, i) in &hit.chips {
                if in_rect(*r, col, row) {
                    return Some(Action::SelectServer(*i));
                }
            }
            if in_rect(hit.conn_list, col, row) {
                return Some(Action::SelectConn((row - hit.conn_list.top()) as usize));
            }
            if in_rect(hit.err_list, col, row) {
                return Some(Action::SelectErr((row - hit.err_list.top()) as usize));
            }
            // click anywhere else in a pane just focuses it
            if in_rect(hit.err_pane, col, row) {
                return Some(Action::FocusErr);
            }
            if in_rect(hit.conn_pane, col, row) {
                return Some(Action::FocusConn);
            }
            None
        }
        _ => None,
    }
}

fn in_rect(r: ratatui::layout::Rect, col: u16, row: u16) -> bool {
    r.width > 0 && r.height > 0 && col >= r.left() && col < r.right() && row >= r.top() && row < r.bottom()
}
