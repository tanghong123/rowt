//! UI-local application state and the update logic. Everything here is
//! operator-local (README "State (ui-local)"); none of it is ever sent upstream.

use crate::model::{Conn, Lane, Snapshot, Window};
use crate::source::Source;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Conn,
    Err,
}

/// An in-progress app-level drag selection (single row). Rendered as a reversed
/// highlight; the covered glyphs are copied on mouse-up. This is the secondary
/// copy path (primary is `y` yank) — it exists because enabling mouse tracking
/// disables the terminal's native drag-select.
#[derive(Clone, Copy, Debug)]
pub struct Drag {
    pub row: u16,
    pub c0: u16,
    pub c1: u16,
}

impl Drag {
    pub fn lo(&self) -> u16 {
        self.c0.min(self.c1)
    }
    pub fn hi(&self) -> u16 {
        self.c0.max(self.c1)
    }
}

/// One interaction, decoded from a key or mouse event (see `input.rs`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Quit,
    Up,
    Down,
    FocusLeft,
    FocusRight,
    CycleFocus,
    CycleFocusBack,
    LaneCycle,
    LaneSet(Option<Lane>),
    WindowCycle,
    WindowStep(i8),
    WindowSet(Window),
    Yank,
    TogglePause,
    ToggleHelp,
    // Mouse:
    FocusConn,
    FocusErr,
    ScrollConn(i8),
    ScrollErr(i8),
    SelectConn(usize), // absolute index in the filtered view
    SelectErr(usize),
}

pub struct App {
    pub source: Box<dyn Source>,
    pub snap: Snapshot,

    pub paused: bool,
    pub lane_filter: Option<Lane>,
    pub window: Window,

    pub focus: Focus,
    pub conn_sel: usize,
    pub conn_scroll: usize,
    pub err_sel: usize,
    pub err_scroll: usize,

    pub help: bool,
    pub marquee: usize,
    pub should_quit: bool,
    pub last_yank: Option<String>,
    pub drag: Option<Drag>,

    // Last-drawn list geometry, fed back by the renderer so movement/scroll can
    // clamp correctly (they need the visible heights, known only at draw time).
    pub conn_h: usize,
    pub err_h: usize,
    pub side_by_side: bool,
}

impl App {
    pub fn new(mut source: Box<dyn Source>) -> Self {
        let window = Window::M10;
        let snap = source.poll(window);
        App {
            source,
            snap,
            paused: false,
            lane_filter: None,
            window,
            focus: Focus::Conn,
            conn_sel: 0,
            conn_scroll: 0,
            err_sel: 0,
            err_scroll: 0,
            help: false,
            marquee: 0,
            should_quit: false,
            last_yank: None,
            drag: None,
            conn_h: 1,
            err_h: 1,
            side_by_side: true,
        }
    }

    /// Data tick: re-poll unless paused.
    pub fn tick(&mut self) {
        if !self.paused {
            self.snap = self.source.poll(self.window);
            self.clamp_selection();
        }
    }

    /// Animation tick (faster than the data tick): advance marquees.
    pub fn anim(&mut self) {
        self.marquee = self.marquee.wrapping_add(1);
    }

    /// Connections visible in the table: block lane is always excluded, plus the
    /// active lane filter (README: filter scopes the connections table only).
    pub fn conns_view(&self) -> Vec<&Conn> {
        self.snap
            .conns
            .iter()
            .filter(|c| c.lane != Lane::Block)
            .filter(|c| self.lane_filter.is_none_or(|l| c.lane == l))
            .collect()
    }

    fn conn_len(&self) -> usize {
        self.conns_view().len()
    }
    fn err_len(&self) -> usize {
        self.snap.errors.len()
    }

    pub fn update(&mut self, a: Action) {
        use Action::*;
        match a {
            Quit => self.should_quit = true,
            ToggleHelp => self.help = !self.help,
            TogglePause => self.paused = !self.paused,
            Up => self.move_sel(-1),
            Down => self.move_sel(1),
            FocusLeft => {
                if self.side_by_side {
                    self.focus = Focus::Conn;
                }
            }
            FocusRight => {
                if self.side_by_side {
                    self.focus = Focus::Err;
                }
            }
            CycleFocus | CycleFocusBack => {
                self.focus = match self.focus {
                    Focus::Conn => Focus::Err,
                    Focus::Err => Focus::Conn,
                };
            }
            LaneCycle => {
                self.lane_filter = match self.lane_filter {
                    None => Some(Lane::Escape),
                    Some(Lane::Escape) => Some(Lane::Corp),
                    Some(Lane::Corp) => Some(Lane::Direct),
                    Some(Lane::Direct) => None,
                    Some(Lane::Block) => None,
                };
                self.conn_sel = 0;
                self.conn_scroll = 0;
            }
            LaneSet(l) => {
                self.lane_filter = l;
                self.conn_sel = 0;
                self.conn_scroll = 0;
            }
            WindowCycle => {
                let i = (self.window.index() + 1) % Window::ALL.len();
                self.window = Window::ALL[i];
                self.tick_window();
            }
            WindowStep(d) => {
                let n = Window::ALL.len() as i32;
                let i = (self.window.index() as i32 + d as i32).rem_euclid(n);
                self.window = Window::ALL[i as usize];
                self.tick_window();
            }
            WindowSet(win) => {
                self.window = win;
                self.tick_window();
            }
            Yank => self.yank(),
            FocusConn => self.focus = Focus::Conn,
            FocusErr => self.focus = Focus::Err,
            ScrollConn(d) => {
                self.focus = Focus::Conn;
                self.scroll_list(Focus::Conn, d);
            }
            ScrollErr(d) => {
                self.focus = Focus::Err;
                self.scroll_list(Focus::Err, d);
            }
            SelectConn(i) => {
                self.focus = Focus::Conn;
                self.conn_sel = i.min(self.conn_len().saturating_sub(1));
                self.ensure_visible(Focus::Conn);
            }
            SelectErr(i) => {
                self.focus = Focus::Err;
                self.err_sel = i.min(self.err_len().saturating_sub(1));
                self.ensure_visible(Focus::Err);
            }
        }
    }

    /// Re-aggregate the errors pane for the new window immediately.
    fn tick_window(&mut self) {
        self.snap = self.source.poll(self.window);
        self.err_sel = self.err_sel.min(self.err_len().saturating_sub(1));
        self.ensure_visible(Focus::Err);
    }

    fn move_sel(&mut self, d: i32) {
        match self.focus {
            Focus::Conn => {
                let len = self.conn_len();
                if len == 0 {
                    return;
                }
                let cur = self.conn_sel as i32;
                // Stacked fall-through: past the bottom -> focus errors' top.
                if !self.side_by_side && d > 0 && cur + d > len as i32 - 1 {
                    self.focus = Focus::Err;
                    self.err_sel = 0;
                    self.ensure_visible(Focus::Err);
                    return;
                }
                self.conn_sel = (cur + d).clamp(0, len as i32 - 1) as usize;
                self.ensure_visible(Focus::Conn);
            }
            Focus::Err => {
                let len = self.err_len();
                if len == 0 {
                    return;
                }
                let cur = self.err_sel as i32;
                if !self.side_by_side && d < 0 && cur + d < 0 {
                    // Fall back up into the connections list (select its bottom).
                    self.focus = Focus::Conn;
                    self.conn_sel = self.conn_len().saturating_sub(1);
                    self.ensure_visible(Focus::Conn);
                    return;
                }
                self.err_sel = (cur + d).clamp(0, len as i32 - 1) as usize;
                self.ensure_visible(Focus::Err);
            }
        }
    }

    fn scroll_list(&mut self, which: Focus, d: i8) {
        match which {
            Focus::Conn => {
                let len = self.conn_len();
                self.conn_sel = (self.conn_sel as i32 + d as i32).clamp(0, len.saturating_sub(1) as i32) as usize;
                self.ensure_visible(Focus::Conn);
            }
            Focus::Err => {
                let len = self.err_len();
                self.err_sel = (self.err_sel as i32 + d as i32).clamp(0, len.saturating_sub(1) as i32) as usize;
                self.ensure_visible(Focus::Err);
            }
        }
    }

    fn ensure_visible(&mut self, which: Focus) {
        match which {
            Focus::Conn => {
                let (sel, h) = (self.conn_sel, self.conn_h.max(1));
                if sel < self.conn_scroll {
                    self.conn_scroll = sel;
                } else if sel >= self.conn_scroll + h {
                    self.conn_scroll = sel + 1 - h;
                }
            }
            Focus::Err => {
                let (sel, h) = (self.err_sel, self.err_h.max(1));
                if sel < self.err_scroll {
                    self.err_scroll = sel;
                } else if sel >= self.err_scroll + h {
                    self.err_scroll = sel + 1 - h;
                }
            }
        }
    }

    fn clamp_selection(&mut self) {
        self.conn_sel = self.conn_sel.min(self.conn_len().saturating_sub(1));
        self.err_sel = self.err_sel.min(self.err_len().saturating_sub(1));
    }

    /// The key field `y` copies for the focused list.
    pub fn yank_target(&self) -> Option<String> {
        match self.focus {
            Focus::Conn => self.conns_view().get(self.conn_sel).map(|c| c.key()),
            Focus::Err => self.snap.errors.get(self.err_sel).map(|e| e.domain.clone()),
        }
    }

    fn yank(&mut self) {
        if let Some(s) = self.yank_target() {
            crate::clipboard::copy(&s);
            self.last_yank = Some(s);
        }
    }
}
