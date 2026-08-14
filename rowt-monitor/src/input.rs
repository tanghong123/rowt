//! Map raw crossterm events to `Action`s (README "Interactions & keymap").

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::app::{Action, App, Armed, Edit};
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
    // Search editor owns the keyboard while open: every printable key edits the
    // pattern; Enter commits, Esc (or Ctrl-C) cancels. Nothing else fires.
    if app.search.editing {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        return match k.code {
            KeyCode::Enter => Some(Action::SearchCommit),
            KeyCode::Esc => Some(Action::SearchCancel),
            KeyCode::Char('c') if ctrl => Some(Action::SearchCancel),
            KeyCode::Left => Some(Action::SearchCursor(-1)),
            KeyCode::Right => Some(Action::SearchCursor(1)),
            KeyCode::Home => Some(Action::SearchHome),
            KeyCode::End => Some(Action::SearchEnd),
            KeyCode::Char('a') if ctrl => Some(Action::SearchHome),
            KeyCode::Char('e') if ctrl => Some(Action::SearchEnd),
            KeyCode::Char('u') if ctrl => Some(Action::SearchKillLine),
            KeyCode::Char('w') if ctrl => Some(Action::SearchKillWord),
            KeyCode::Backspace => Some(Action::SearchBackspace),
            KeyCode::Delete => Some(Action::SearchDelete),
            KeyCode::Char(c) if !ctrl && !alt => Some(Action::SearchInput(c)),
            _ => None,
        };
    }
    // An armed lane edit turns the confirm bar into a one-line editor. This is
    // deliberately NOT a modal takeover like the search editor: anything that
    // isn't a text key falls through to the global keymap, where — as before —
    // it cancels the arm. The eight arm keys keep arming/committing for the
    // first `DOUBLE_TAP_WINDOW`; after that the field is live and they type.
    if let Some(a) = &app.armed {
        if let Some(act) = armed_key(k, a) {
            return Some(act);
        }
    }
    Some(match k.code {
        KeyCode::Char('/') => Action::SearchOpen,
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
        KeyCode::Char('s') => Action::BandCycle,
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
        // Shifted: the same four edits on the host's parent suffix (`x.y.z.com`
        // → `.z.com`). Listed after the Ctrl-C guard above, which only matches
        // lowercase, so a shifted key can't be mistaken for it.
        KeyCode::Char('E') => Action::RouteSuffix(Lane::Escape),
        KeyCode::Char('C') => Action::RouteSuffix(Lane::Corp),
        KeyCode::Char('B') => Action::RouteSuffix(Lane::Block),
        KeyCode::Char('D') => Action::UnrouteSuffix,
        KeyCode::Char('u') => Action::UseServer,
        KeyCode::Char('o') => Action::ToggleProxy,
        KeyCode::Enter => Action::Confirm,
        KeyCode::Esc => Action::Escape,
        _ => return None,
    })
}

/// The eight keys that arm a lane edit. Inside the double-tap window they keep
/// that meaning within the confirm bar rather than typing themselves — which is
/// what makes "press the same key twice" commit.
const ARM_KEYS: [char; 8] = ['e', 'c', 'b', 'd', 'E', 'C', 'B', 'D'];

/// Keys the armed confirm bar claims. `None` = not ours, fall through to the
/// global keymap (which cancels the arm, as it always did).
fn armed_key(k: KeyEvent, a: &Armed) -> Option<Action> {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    let edit = |e| Some(Action::ArmEdit(e));
    match k.code {
        // Ctrl-C cancels the pending edit rather than quitting — the same
        // precedence the search editor uses while it owns the keyboard.
        KeyCode::Char('c') if ctrl => Some(Action::Escape),
        KeyCode::Char('a') if ctrl => edit(Edit::Home),
        KeyCode::Char('e') if ctrl => edit(Edit::End),
        KeyCode::Char('w') if ctrl => edit(Edit::DropLabel),
        // Unambiguous edit intent: these do nothing useful while armed today
        // (Backspace/Delete/Home/End aren't bound at all; ←/→ only change focus
        // on the way to cancelling), so claiming them costs nothing.
        KeyCode::Backspace => edit(Edit::Backspace),
        KeyCode::Delete => edit(Edit::Delete),
        KeyCode::Left => edit(Edit::Cursor(-1)),
        KeyCode::Right => edit(Edit::Cursor(1)),
        KeyCode::Home => edit(Edit::Home),
        KeyCode::End => edit(Edit::End),
        // Inside the double-tap window the arm keys keep their meaning, so `e`,`e`
        // commits and `e`,`C` re-arms. Once it closes — or as soon as anything is
        // typed — the bar is a live editor and they type like any other key.
        KeyCode::Char(c) if !ctrl && !alt && (a.editing() || !ARM_KEYS.contains(&c)) => edit(Edit::Insert(c)),
        _ => None, // Enter/Esc and everything else: the global keymap handles it
    }
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
