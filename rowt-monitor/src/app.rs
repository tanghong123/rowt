//! UI-local application state and the update logic. Everything here is
//! operator-local (README "State (ui-local)"); none of it is ever sent upstream.

use std::time::{Duration, Instant};

use crate::model::{Conn, Lane, Snapshot, Window};
use crate::source::Source;

/// After the last committed lane edit, wait this long before issuing the single
/// batched router reload (CONTROLS.md §4.3).
pub const RELOAD_DEBOUNCE: Duration = Duration::from_secs(7);
/// An armed (not-yet-committed) lane edit auto-cancels after this long (§4.2).
pub const ARM_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Conn,
    Err,
    Health,
}

/// A lane edit that's been armed by a first keypress and awaits confirmation
/// (§4.2). `u`/`o` skip this and apply immediately.
#[derive(Clone, Debug)]
pub struct Armed {
    pub domain: String,
    /// `Some(lane)` = route into that lane (`e`/`c`/`b`); `None` = unroute (`d`).
    pub lane: Option<Lane>,
    pub key: char, // the key that armed it; pressing it again commits
    pub at: Instant,
}

impl Armed {
    /// Footer preview label, e.g. `x.com → escape` or `x.com → direct`.
    pub fn label(&self) -> String {
        let dest = self.lane.map(Lane::label).unwrap_or("direct");
        format!("{} → {}", self.domain, dest)
    }
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
    ForceProbe,
    // Control layer (CONTROLS.md):
    Route(Lane),  // e/c/b — arm routing the selected domain into a lane
    Unroute,      // d — arm removing the selected domain (→ direct)
    Confirm,      // Enter — commit the armed edit
    Escape,       // Esc — cancel arm / clear selection / clear lane filter (in that order)
    UseServer,    // u — switch to the selected server (immediate)
    ToggleProxy,  // o — toggle the macOS system proxy (immediate)
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

    // Deferred/locked selection (CONTROLS.md §5.2): a focused pane starts with NO
    // active selection; the first ↑/↓ locks onto a row *by key* (domain), so the
    // acted-on domain can't shift under the 2s re-sort. `None` = nothing selected.
    pub conn_key: Option<String>,
    pub err_key: Option<String>,

    // Server strip (§5.4): `None` = marqueeing, no selection. Set on first ←/→.
    pub strip_sel: Option<usize>,
    // Index of the leftmost visible chip, fed back by draw_chips each frame. While
    // marqueeing it's the first visible chip (what the first ←/→ selects); while a
    // chip is selected it's the frozen ring window's left edge (the display wraps
    // past the last chip back to the first, so the row is always filled).
    pub strip_page: usize,
    pub strip_w: u16, // server-strip viewport width, fed back for circular paging

    // Control layer: the armed-but-uncommitted lane edit, and the batched-reload
    // deadline (7s after the last committed edit).
    pub armed: Option<Armed>,
    pub pending_reload: Option<Instant>,

    pub help: bool,
    pub started: Instant, // wall-clock start, for time-based pulse + marquees
    pub should_quit: bool,
    pub last_yank: Option<String>,
    pub toast: Option<(String, Instant)>, // transient footer message (auto-clears)
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
        let snap = source.poll(window, None);
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
            conn_key: None,
            err_key: None,
            strip_sel: None,
            strip_page: 0,
            strip_w: 0,
            armed: None,
            pending_reload: None,
            help: false,
            started: Instant::now(),
            should_quit: false,
            last_yank: None,
            toast: None,
            drag: None,
            conn_h: 1,
            err_h: 1,
            side_by_side: true,
        }
    }

    /// Data tick: re-poll unless paused.
    pub fn tick(&mut self) {
        if !self.paused {
            self.snap = self.source.poll(self.window, self.lane_filter);
            self.resolve_keys();
            self.clamp_selection();
        }
    }

    /// Per-frame housekeeping (called every ~70ms from the event loop, not just
    /// on the 2s data tick): deliver control-command outcomes as toasts, fire the
    /// debounced router reload, and auto-cancel a stale arm.
    pub fn on_frame(&mut self) {
        for o in self.source.drain_ctl() {
            match o {
                crate::source::CtlOutcome::Ok(m) => self.notify(m),
                crate::source::CtlOutcome::Err(m) => self.notify(format!("⚠ {m}")),
            }
        }
        if let Some(t) = self.pending_reload {
            if Instant::now() >= t {
                self.pending_reload = None;
                self.source.reload_router();
                self.notify("reloading router…".to_string());
            }
        }
        if let Some(a) = &self.armed {
            if a.at.elapsed() >= ARM_TIMEOUT {
                self.armed = None;
            }
        }
    }

    /// Re-resolve each locked selection to its row's current index (rows re-sort
    /// on the tick); drop the lock if the domain has left the list (§5.2).
    fn resolve_keys(&mut self) {
        if let Some(k) = self.conn_key.clone() {
            match self.conns_view().iter().position(|c| c.key() == k) {
                Some(i) => {
                    self.conn_sel = i;
                    self.ensure_visible(Focus::Conn);
                }
                None => self.conn_key = None,
            }
        }
        if let Some(k) = self.err_key.clone() {
            match self.snap.errors.iter().position(|e| e.domain == k) {
                Some(i) => {
                    self.err_sel = i;
                    self.ensure_visible(Focus::Err);
                }
                None => self.err_key = None,
            }
        }
    }

    /// Re-poll now with the current window + lane filter (immediate feedback on
    /// a window/lane change; cheap — errors re-aggregate in memory).
    fn repoll(&mut self) {
        self.snap = self.source.poll(self.window, self.lane_filter);
    }

    /// Show a transient footer message (auto-clears — see `draw_footer`).
    pub fn notify(&mut self, msg: String) {
        self.toast = Some((msg, Instant::now()));
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
        // Arm lifecycle: a control-key (re)arms or commits; Confirm commits; Esc
        // is handled below; ANY other key cancels a pending arm, then proceeds.
        match a {
            Route(_) | Unroute | Confirm | Escape => {}
            _ => self.armed = None,
        }
        match a {
            Quit => self.should_quit = true,
            ToggleHelp => self.help = !self.help,
            TogglePause => self.paused = !self.paused,
            ForceProbe => {
                self.source.force_probe();
                self.notify("re-probing servers…".to_string());
            }
            Route(lane) => self.arm(Some(lane)),
            Unroute => self.arm(None),
            Confirm => self.commit_armed(),
            Escape => self.handle_escape(),
            UseServer => self.use_selected_server(),
            ToggleProxy => self.toggle_proxy(),
            Up => self.move_sel(-1),
            Down => self.move_sel(1),
            FocusLeft => {
                if self.focus == Focus::Health {
                    self.strip_move(-1);
                } else if self.side_by_side {
                    self.focus = Focus::Conn;
                }
            }
            FocusRight => {
                if self.focus == Focus::Health {
                    self.strip_move(1);
                } else if self.side_by_side {
                    self.focus = Focus::Err;
                }
            }
            CycleFocus => self.cycle_focus(1),
            CycleFocusBack => self.cycle_focus(-1),
            LaneCycle => {
                self.lane_filter = match self.lane_filter {
                    None => Some(Lane::Escape),
                    Some(Lane::Escape) => Some(Lane::Corp),
                    Some(Lane::Corp) => Some(Lane::Direct),
                    Some(Lane::Direct) => None,
                    Some(Lane::Block) => None,
                };
                self.on_lane_change();
            }
            LaneSet(l) => {
                self.lane_filter = l;
                self.on_lane_change();
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
                self.set_conn_index(i.min(self.conn_len().saturating_sub(1)));
            }
            SelectErr(i) => {
                self.focus = Focus::Err;
                self.set_err_index(i.min(self.err_len().saturating_sub(1)));
            }
        }
    }

    fn cycle_focus(&mut self, d: i8) {
        // conns → errors → server health → conns (Tab); reverse on Shift+Tab.
        self.focus = match (self.focus, d >= 0) {
            (Focus::Conn, true) => Focus::Err,
            (Focus::Err, true) => Focus::Health,
            (Focus::Health, true) => Focus::Conn,
            (Focus::Conn, false) => Focus::Health,
            (Focus::Health, false) => Focus::Err,
            (Focus::Err, false) => Focus::Conn,
        };
    }

    /// Arm a lane edit on the focused pane's locked domain (or commit if the same
    /// edit is already armed — the double-tap path). Inert with no selection.
    fn arm(&mut self, lane: Option<Lane>) {
        let Some(domain) = self.selected_domain() else { return };
        let key = match lane {
            Some(Lane::Escape) => 'e',
            Some(Lane::Corp) => 'c',
            Some(Lane::Block) => 'b',
            Some(Lane::Direct) | None => 'd',
        };
        if let Some(a) = &self.armed {
            if a.key == key && a.domain == domain && a.lane == lane {
                self.commit_armed();
                return;
            }
        }
        self.armed = Some(Armed { domain, lane, key, at: Instant::now() });
    }

    fn commit_armed(&mut self) {
        let Some(a) = self.armed.take() else { return };
        match a.lane {
            Some(l) => self.source.route_lane(&a.domain, l),
            None => self.source.unroute(&a.domain),
        }
        // Batch the reload: (re)start the 7s debounce (CONTROLS.md §4.3).
        self.pending_reload = Some(Instant::now() + RELOAD_DEBOUNCE);
        self.notify(a.label());
    }

    /// Esc priority: cancel an arm, else clear the focused selection, else clear
    /// the lane filter (the shipped Esc behavior).
    fn handle_escape(&mut self) {
        if self.armed.take().is_some() {
            return;
        }
        match self.focus {
            Focus::Conn if self.conn_key.is_some() => {
                self.conn_key = None;
                return;
            }
            Focus::Err if self.err_key.is_some() => {
                self.err_key = None;
                return;
            }
            Focus::Health if self.strip_sel.is_some() => {
                self.strip_sel = None;
                return;
            }
            _ => {}
        }
        if self.lane_filter.is_some() {
            self.lane_filter = None;
            self.on_lane_change();
        }
    }

    fn use_selected_server(&mut self) {
        if self.focus != Focus::Health {
            return;
        }
        if let Some(s) = self.strip_sel.and_then(|i| self.snap.chips.get(i)) {
            if s.active {
                self.notify(format!("{} is already active", s.name));
            } else {
                self.source.use_server(&s.name);
                self.notify(format!("switching → {}", s.name));
            }
        }
    }

    fn toggle_proxy(&mut self) {
        let on = self.snap.identity.proxy == "on";
        self.source.set_proxy(!on);
        self.notify(if on { "system proxy → off".into() } else { "system proxy → on".into() });
    }

    fn strip_move(&mut self, d: i32) {
        let n = self.snap.chips.len();
        if n == 0 {
            return;
        }
        match self.strip_sel {
            // First ←/→ selects the first VISIBLE chip and anchors the page there,
            // so the strip does not jump — the chip stays where it was on screen.
            None => {
                let f = self.strip_page.min(n - 1);
                self.strip_sel = Some(f);
                self.strip_page = f;
            }
            // Subsequent moves wrap around the ends; scroll the frozen ring window
            // (in the move direction) just enough to keep the selection visible.
            Some(i) => {
                let ni = (i as i32 + d).rem_euclid(n as i32) as usize;
                self.strip_sel = Some(ni);
                self.reveal_strip(ni, d);
            }
        }
    }

    /// Display width of server chip `i`, matching `draw_chips`'s segments exactly
    /// (name + ` ` + `NNN ms`, plus `▶ ` for the active one).
    fn chip_w(&self, i: usize) -> u16 {
        use crate::paint::dw;
        match self.snap.chips.get(i) {
            Some(c) => dw(&c.name) + 7 + if c.active { dw("▶ ") } else { 0 },
            None => 0,
        }
    }

    /// The chips `draw_chips` would render (circularly) starting at `page`, in
    /// order, until the viewport width is used up — its 3-cell separators match.
    fn strip_visible_from(&self, page: usize) -> Vec<usize> {
        let n = self.snap.chips.len();
        let mut out = Vec::new();
        if n == 0 || self.strip_w == 0 {
            return out;
        }
        let mut col = 0u16;
        let mut i = page % n;
        for drawn in 0..n {
            let sep = if drawn > 0 { 3 } else { 0 };
            if col + sep + self.chip_w(i) > self.strip_w {
                break;
            }
            col += sep + self.chip_w(i);
            out.push(i);
            i = (i + 1) % n;
        }
        out
    }

    /// Scroll the ring window one chip at a time in the move direction `d` until
    /// the selected chip `si` is visible (bounded by the pool size).
    fn reveal_strip(&mut self, si: usize, d: i32) {
        let n = self.snap.chips.len();
        if n == 0 || self.strip_w == 0 {
            return;
        }
        for _ in 0..n {
            if self.strip_visible_from(self.strip_page).contains(&si) {
                return;
            }
            self.strip_page = if d >= 0 { (self.strip_page + 1) % n } else { (self.strip_page + n - 1) % n };
        }
    }

    /// Re-aggregate the errors pane for the new window immediately.
    fn tick_window(&mut self) {
        self.repoll();
        self.err_sel = self.err_sel.min(self.err_len().saturating_sub(1));
        self.ensure_visible(Focus::Err);
    }

    /// The lane filter changed: reset both panes' selection and re-poll so the
    /// errors pane (now scoped to the filter) updates immediately.
    fn on_lane_change(&mut self) {
        self.conn_sel = 0;
        self.conn_scroll = 0;
        self.conn_key = None;
        self.repoll();
        self.err_sel = 0;
        self.err_scroll = 0;
        self.err_key = None;
    }

    fn move_sel(&mut self, d: i32) {
        match self.focus {
            // ↑ leaves the server strip back to the connections pane (§5.3).
            Focus::Health => {
                if d < 0 {
                    self.focus = Focus::Conn;
                    self.strip_sel = None;
                }
            }
            Focus::Conn => {
                let len = self.conn_len();
                if len == 0 {
                    if d > 0 {
                        self.enter_health();
                    }
                    return;
                }
                // First ↑/↓ just *activates* a selection (§5.2), locking row 0.
                if self.conn_key.is_none() {
                    self.set_conn_index(0);
                    return;
                }
                let cur = self.conn_sel as i32;
                if d > 0 && cur >= len as i32 - 1 {
                    // Past the bottom: stacked → errors' top; else → server strip.
                    if !self.side_by_side {
                        self.focus = Focus::Err;
                        self.set_err_index(0);
                    } else {
                        self.enter_health();
                    }
                    return;
                }
                self.set_conn_index((cur + d).clamp(0, len as i32 - 1) as usize);
            }
            Focus::Err => {
                let len = self.err_len();
                if len == 0 {
                    if d > 0 {
                        self.enter_health();
                    }
                    return;
                }
                if self.err_key.is_none() {
                    self.set_err_index(0);
                    return;
                }
                let cur = self.err_sel as i32;
                if !self.side_by_side && d < 0 && cur == 0 {
                    // Fall back up into the connections list (select its bottom).
                    self.focus = Focus::Conn;
                    self.set_conn_index(self.conn_len().saturating_sub(1));
                    return;
                }
                if d > 0 && cur >= len as i32 - 1 {
                    self.enter_health();
                    return;
                }
                self.set_err_index((cur + d).clamp(0, len as i32 - 1) as usize);
            }
        }
    }

    /// Move focus to the server strip without selecting a chip (keeps marqueeing).
    fn enter_health(&mut self) {
        self.focus = Focus::Health;
        self.strip_sel = None;
    }

    /// Lock the connections selection onto row `i` (by key), or clear if absent.
    fn set_conn_index(&mut self, i: usize) {
        match self.conns_view().get(i).map(|c| c.key()) {
            Some(k) => {
                self.conn_key = Some(k);
                self.conn_sel = i;
                self.ensure_visible(Focus::Conn);
            }
            None => self.conn_key = None,
        }
    }

    fn set_err_index(&mut self, i: usize) {
        match self.snap.errors.get(i).map(|e| e.domain.clone()) {
            Some(k) => {
                self.err_key = Some(k);
                self.err_sel = i;
                self.ensure_visible(Focus::Err);
            }
            None => self.err_key = None,
        }
    }

    fn scroll_list(&mut self, which: Focus, d: i8) {
        match which {
            Focus::Conn => {
                let i = (self.conn_sel as i32 + d as i32).clamp(0, self.conn_len().saturating_sub(1) as i32) as usize;
                self.set_conn_index(i);
            }
            Focus::Err => {
                let i = (self.err_sel as i32 + d as i32).clamp(0, self.err_len().saturating_sub(1) as i32) as usize;
                self.set_err_index(i);
            }
            Focus::Health => {}
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
            Focus::Health => {}
        }
    }

    fn clamp_selection(&mut self) {
        self.conn_sel = self.conn_sel.min(self.conn_len().saturating_sub(1));
        self.err_sel = self.err_sel.min(self.err_len().saturating_sub(1));
    }

    pub fn conn_active(&self) -> bool {
        self.conn_key.is_some()
    }
    pub fn err_active(&self) -> bool {
        self.err_key.is_some()
    }

    /// The focused pane's locked domain, if any (drives the contextual controls).
    pub fn selected_domain(&self) -> Option<String> {
        match self.focus {
            Focus::Conn => self.conn_key.clone(),
            Focus::Err => self.err_key.clone(),
            Focus::Health => None,
        }
    }

    /// The key field `y` copies for the focused list (the row under the cursor,
    /// whether or not it's locked — copying is harmless).
    pub fn yank_target(&self) -> Option<String> {
        match self.focus {
            Focus::Conn => self.conns_view().get(self.conn_sel).map(|c| c.key()),
            Focus::Err => self.snap.errors.get(self.err_sel).map(|e| e.domain.clone()),
            Focus::Health => self.strip_sel.and_then(|i| self.snap.chips.get(i)).map(|s| s.name.clone()),
        }
    }

    fn yank(&mut self) {
        if let Some(s) = self.yank_target() {
            crate::clipboard::copy(&s);
            self.notify(format!("copied {}", s));
            self.last_yank = Some(s);
        }
    }
}
