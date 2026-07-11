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
/// How long an optimistic proxy toggle is shown before deferring to the real
/// polled state (long enough for `rowt proxy` + the ~2s state re-read to land).
pub const PROXY_OPTIMISTIC_TTL: Duration = Duration::from_secs(6);
/// Clear a selection (which freezes the strip / holds a row) after this much
/// input inactivity, so the view resumes live scrolling if the operator walks away.
pub const SELECTION_IDLE_TIMEOUT: Duration = Duration::from_secs(15);

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
    SelectServer(usize), // click a server chip: focus the strip + select it in place
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
    // Frozen scroll position of the strip's ring, in cells (matches the marquee's
    // own offset). Captured at the exact instant the first ←/→ freezes the scroll
    // — so the display doesn't jump; a partial chip may sit before the selection —
    // then nudged just enough to keep the selection visible as it moves.
    pub strip_off: usize,
    pub strip_w: u16,          // server-strip viewport width, fed back from the renderer
    pub strip_render_off: usize, // the marquee offset the renderer last drew (freeze to it → no jump)

    // Control layer: the armed-but-uncommitted lane edit, and the batched-reload
    // deadline (7s after the last committed edit).
    pub armed: Option<Armed>,
    pub pending_reload: Option<Instant>,
    // Optimistic system-proxy state: shown immediately on a toggle so the change
    // feels instant, then cleared once the real (polled) state confirms it — or a
    // timeout reverts the display if the underlying `rowt proxy` command failed.
    pub proxy_optimistic: Option<(String, Instant)>,

    pub help: bool,
    pub started: Instant, // wall-clock start, for time-based pulse + marquees
    pub hover: Option<(u16, u16)>, // last mouse position (for hover highlights)
    pub last_input: Instant,       // last action (for the selection idle timeout)
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
            strip_off: 0,
            strip_w: 0,
            strip_render_off: 0,
            armed: None,
            pending_reload: None,
            proxy_optimistic: None,
            help: false,
            started: Instant::now(),
            hover: None,
            last_input: Instant::now(),
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
        // Clear the optimistic proxy state once the real state confirms it, or
        // give up after a few seconds (a failed toggle then visibly reverts).
        if let Some((want, at)) = &self.proxy_optimistic {
            if self.snap.identity.proxy == *want || at.elapsed() >= PROXY_OPTIMISTIC_TTL {
                self.proxy_optimistic = None;
            }
        }
        self.expire_idle_selection(Instant::now());
    }

    /// Drop any active selection (row lock / frozen strip) after
    /// `SELECTION_IDLE_TIMEOUT` of input inactivity, so the panes resume live
    /// scrolling/updating. `now` is a parameter for testability.
    pub fn expire_idle_selection(&mut self, now: Instant) {
        let has_selection = self.conn_key.is_some() || self.err_key.is_some() || self.strip_sel.is_some();
        if has_selection && now.duration_since(self.last_input) >= SELECTION_IDLE_TIMEOUT {
            self.conn_key = None;
            self.err_key = None;
            self.strip_sel = None;
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
        // Any real action counts as activity for the selection idle timeout
        // (mouse-move/hover doesn't go through here, so it won't keep it alive).
        self.last_input = Instant::now();
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
                    self.set_focus(Focus::Conn);
                }
            }
            FocusRight => {
                if self.focus == Focus::Health {
                    self.strip_move(1);
                } else if self.side_by_side {
                    self.set_focus(Focus::Err);
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
            FocusConn => self.set_focus(Focus::Conn),
            FocusErr => self.set_focus(Focus::Err),
            ScrollConn(d) => {
                self.set_focus(Focus::Conn);
                self.scroll_list(Focus::Conn, d);
            }
            ScrollErr(d) => {
                self.set_focus(Focus::Err);
                self.scroll_list(Focus::Err, d);
            }
            SelectConn(i) => {
                self.set_focus(Focus::Conn);
                self.set_conn_index(i.min(self.conn_len().saturating_sub(1)));
            }
            SelectErr(i) => {
                self.set_focus(Focus::Err);
                self.set_err_index(i.min(self.err_len().saturating_sub(1)));
            }
            SelectServer(i) => self.select_server(i),
        }
    }

    /// Click a server chip: focus the strip and select that chip *in place* — the
    /// strip freezes exactly where it is (the clicked chip doesn't move).
    fn select_server(&mut self, i: usize) {
        if i >= self.snap.chips.len() {
            return;
        }
        self.set_focus(Focus::Health);
        // Freeze the ring at the exact rendered offset only if it wasn't already
        // frozen, so re-clicking another visible chip keeps the display put.
        if self.strip_sel.is_none() {
            let (_, _, span) = self.strip_layout();
            self.strip_off = self.strip_render_off % span.max(1);
        }
        self.strip_sel = Some(i);
    }

    /// Move focus, forgetting the selection in the pane we're leaving so that
    /// re-focusing a pane always starts fresh (no restored highlight). A caller
    /// that wants a selection in the destination (fall-through, click) sets it
    /// explicitly *after* this.
    fn set_focus(&mut self, f: Focus) {
        if self.focus == f {
            return;
        }
        match self.focus {
            Focus::Conn => self.conn_key = None,
            Focus::Err => self.err_key = None,
            Focus::Health => self.strip_sel = None,
        }
        self.focus = f;
    }

    fn cycle_focus(&mut self, d: i8) {
        // conns → errors → server health → conns (Tab); reverse on Shift+Tab.
        let next = match (self.focus, d >= 0) {
            (Focus::Conn, true) => Focus::Err,
            (Focus::Err, true) => Focus::Health,
            (Focus::Health, true) => Focus::Conn,
            (Focus::Conn, false) => Focus::Health,
            (Focus::Health, false) => Focus::Err,
            (Focus::Err, false) => Focus::Conn,
        };
        self.set_focus(next);
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

    /// The system-proxy state to display: the optimistic target if a toggle is
    /// pending confirmation, otherwise the real polled state.
    pub fn proxy_display(&self) -> String {
        match &self.proxy_optimistic {
            Some((v, _)) => v.clone(),
            None => self.snap.identity.proxy.clone(),
        }
    }

    fn toggle_proxy(&mut self) {
        let on = self.proxy_display() == "on";
        let target = if on { "off" } else { "on" };
        self.source.set_proxy(!on);
        // Flip the display immediately (optimistic); on_frame reconciles it with
        // the real state once the command lands, or reverts it after a timeout.
        self.proxy_optimistic = Some((target.to_string(), Instant::now()));
        self.notify(format!("system proxy → {target}"));
    }

    fn strip_move(&mut self, d: i32) {
        let n = self.snap.chips.len();
        if n == 0 {
            return;
        }
        match self.strip_sel {
            // First ←/→ freezes the marquee at the EXACT offset the renderer last
            // drew (fed back as `strip_render_off`), so the frozen view is the
            // snapshot on screen — a partial chip may remain at the left edge, and
            // nothing jumps. Then select the first fully-visible chip.
            None => {
                let (starts, widths, span) = self.strip_layout();
                self.strip_off = self.strip_render_off % span.max(1);
                self.strip_sel = Some(self.first_fully_visible(&starts, &widths, span, self.strip_off));
            }
            // Subsequent moves wrap around the ends; scroll the frozen ring (in the
            // move direction) just enough to keep the selection fully visible.
            Some(i) => {
                let ni = (i as i32 + d).rem_euclid(n as i32) as usize;
                self.strip_sel = Some(ni);
                self.reveal_strip(ni, d);
            }
        }
    }

    /// Cell width of server chip `i`, matching `draw_chips`'s cell buffer exactly
    /// (char counts: name + ` ` + `NNN ms`, plus `▶ ` for the active one).
    fn chip_w(&self, i: usize) -> u16 {
        match self.snap.chips.get(i) {
            Some(c) => c.name.chars().count() as u16 + 7 + if c.active { 2 } else { 0 },
            None => 0,
        }
    }

    /// The strip's ring layout: each chip's start cell, its width, and the total
    /// ring span (chips + 3-cell separators + a 3-cell trailing gap), mirroring the
    /// marquee cell buffer in `draw_chips`.
    fn strip_layout(&self) -> (Vec<usize>, Vec<u16>, usize) {
        let n = self.snap.chips.len();
        let mut starts = Vec::with_capacity(n);
        let mut widths = Vec::with_capacity(n);
        let mut cells = 0usize;
        for i in 0..n {
            if i > 0 {
                cells += 3;
            }
            starts.push(cells);
            let w = self.chip_w(i);
            widths.push(w);
            cells += w as usize;
        }
        (starts, widths, cells + 3)
    }

    /// Display column of chip `i`'s left edge within the window at offset `off`.
    fn chip_col(&self, starts: &[usize], span: usize, off: usize, i: usize) -> usize {
        (starts[i] + span - off % span) % span
    }

    /// The first chip fully inside the viewport at offset `off` (what the first
    /// ←/→ selects); falls back to chip 0 if none fits.
    fn first_fully_visible(&self, starts: &[usize], widths: &[u16], span: usize, off: usize) -> usize {
        let mut best: Option<(usize, usize)> = None; // (left column, index)
        for (i, &wd) in widths.iter().enumerate() {
            let col = self.chip_col(starts, span, off, i);
            if col + wd as usize <= self.strip_w as usize && best.is_none_or(|(c, _)| col < c) {
                best = Some((col, i));
            }
        }
        best.map(|(_, i)| i).unwrap_or(0)
    }

    /// Scroll the frozen ring one cell at a time in the move direction `d` until
    /// the selected chip is fully visible (bounded by the ring span).
    fn reveal_strip(&mut self, si: usize, d: i32) {
        let (starts, widths, span) = self.strip_layout();
        if span == 0 || self.strip_w == 0 {
            return;
        }
        for _ in 0..span {
            let col = self.chip_col(&starts, span, self.strip_off, si);
            if col + widths[si] as usize <= self.strip_w as usize {
                return;
            }
            self.strip_off = if d >= 0 { (self.strip_off + 1) % span } else { (self.strip_off + span - 1) % span };
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
                    self.set_focus(Focus::Conn);
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
                        self.set_focus(Focus::Err);
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
                    self.set_focus(Focus::Conn);
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
        self.set_focus(Focus::Health);
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
