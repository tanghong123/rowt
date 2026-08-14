//! UI-local application state and the update logic. Everything here is
//! operator-local (README "State (ui-local)"); none of it is ever sent upstream.

use std::time::{Duration, Instant};

use std::collections::HashSet;

use regex::RegexBuilder;

use crate::model::{Conn, ConnRow, ConnView, ErrRow, Lane, MetricsBand, Snapshot, Window};
use crate::source::History;
use crate::source::Source;

/// After the last committed lane edit, wait this long before issuing the single
/// batched router reload (CONTROLS.md §4.3).
pub const RELOAD_DEBOUNCE: Duration = Duration::from_secs(7);
/// An armed (not-yet-committed) lane edit auto-cancels after this long IDLE
/// (§4.2) — measured from the last keypress, not from arming, so it never
/// expires mid-edit while someone is typing. Cancelling is exactly what `Esc`
/// does, and just as silent.
///
/// One value covers both phases. The bar is a live editable field within half a
/// second, so an untouched arm and a half-typed one look the same to the
/// operator and it would be strange for them to vanish on different schedules.
/// Ten seconds is long enough to pause and think about a domain, short enough
/// that a mis-press clears itself before you notice it.
pub const ARM_TIMEOUT: Duration = Duration::from_secs(10);
/// How long after arming the bar stays a plain confirmation — arm keys keep
/// their meaning, no cursor — before it becomes a live editor.
///
/// The two behaviours are mutually exclusive on purpose: a key cannot both
/// commit and type. Splitting them in TIME rather than by a mode flag means the
/// bar never has to advertise which one is active, so its hint is one fixed
/// string and nothing in it can move (see `draw_footer`).
///
/// 500ms matches the usual double-click threshold. Too short and a deliberate
/// second press lands as a keystroke; the cost of too long is only that the
/// cursor appears late, so err upward if it ever feels tight.
pub const DOUBLE_TAP_WINDOW: Duration = Duration::from_millis(500);
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
///
/// `domain` doubles as a one-line editor buffer: the confirm bar is editable, so
/// the entry that gets written need not be the one the keypress proposed. Until
/// the buffer is touched (`edited`) the bar keeps every armed behaviour it had
/// before — the arming key still double-taps, the other control keys still
/// re-arm — so editing is strictly additive.
#[derive(Clone, Debug)]
pub struct Armed {
    pub domain: String,
    /// `Some(lane)` = route into that lane (`e`/`c`/`b`); `None` = unroute (`d`).
    pub lane: Option<Lane>,
    pub key: char, // the key that armed it; pressing it again commits
    pub at: Instant,
    /// Block-cursor char index into `domain` (0..=len). Only drawn once `edited`.
    pub cursor: usize,
    /// Has the operator touched the buffer? Gates both the double-tap shortcut
    /// and whether letters type instead of re-arming.
    pub edited: bool,
}

impl Armed {
    /// Footer preview label, e.g. `x.com → escape` or `x.com → direct`.
    pub fn label(&self) -> String {
        let dest = self.lane.map(Lane::label).unwrap_or("direct");
        format!("{} → {}", self.domain, dest)
    }
    /// Is the bar a live editor yet? Before this the arm keys still commit and
    /// re-arm, and no cursor is drawn. Typing anything opens it immediately —
    /// the operator has said what they want, so there is nothing left to wait for.
    pub fn editing(&self) -> bool {
        self.edited || self.at.elapsed() >= DOUBLE_TAP_WINDOW
    }
    /// The entry as it would be written. Only the ends are trimmed — NOT
    /// interior whitespace, even though bash `edit_list` would strip that with
    /// `tr -d '[:space:]'`. Silently closing up `a b.com` into `ab.com` would
    /// apply something other than what the bar previewed, so `commit_armed`
    /// refuses instead.
    pub fn entry(&self) -> String {
        self.domain.trim().to_string()
    }
}

/// One editing operation on the armed confirm bar.
///
/// Close to the search editor's key set, with two deliberate differences that
/// come from what a DOMAIN edit actually is: narrowing a hostname means dropping
/// labels off the FRONT (`i.ytimg.com` → `ytimg.com`), so the cursor starts at
/// the left and `^W` strips the leading label rather than killing backwards.
/// There is no kill-line: an empty field is a cancel, which `Esc` already says
/// more clearly.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Edit {
    Insert(char),
    Backspace,
    Delete,
    Cursor(i8),
    Home,
    End,
    /// `^W` — drop the leading dot-component.
    DropLabel,
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

/// Compiled host-search predicate. A case-insensitive, unanchored regex when the
/// pattern compiles; a case-insensitive literal-substring fallback when it
/// doesn't (so a mid-edit `foo(` still filters instead of erroring or freezing).
#[derive(Clone)]
pub enum Matcher {
    Regex(regex::Regex),
    Literal(String), // lower-cased needle
}

impl Matcher {
    /// Compile `pat` (empty → `None`, meaning no filtering).
    pub fn compile(pat: &str) -> Option<Matcher> {
        if pat.is_empty() {
            return None;
        }
        // Bound the compiled program so a pathological pattern can't blow memory;
        // on any build error fall back to a plain lower-cased substring search.
        Some(match RegexBuilder::new(pat).case_insensitive(true).size_limit(1 << 20).build() {
            Ok(re) => Matcher::Regex(re),
            Err(_) => Matcher::Literal(pat.to_lowercase()),
        })
    }
    pub fn is_match(&self, host: &str) -> bool {
        match self {
            Matcher::Regex(re) => re.is_match(host),
            Matcher::Literal(s) => host.to_lowercase().contains(s),
        }
    }
}

/// Host-name search state (`/`). While `editing`, the footer is a line editor and
/// the pane rows filter *incrementally* off `buf`; on Enter the pattern commits to
/// `committed` and persists. The effective predicate `matcher` reflects `buf` while
/// editing, else `committed`. Composed AND with the lane filter; never touches the
/// per-lane header aggregate. Applies to BOTH panes (one shared pattern).
#[derive(Clone, Default)]
pub struct Search {
    pub editing: bool,
    pub buf: String,             // editor buffer (meaningful while editing)
    pub cursor: usize,           // block-cursor char index into buf (0..=len)
    pub committed: String,       // last committed pattern ("" = inactive)
    pub matcher: Option<Matcher>, // effective predicate (buf while editing, else committed)
    pub changed_at: Option<Instant>, // when the committed pattern last changed (10s degrade)
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
    ConnViewCycle, // v — pan the connections pane: live → ↑ upload → ↓ download
    BandCycle,     // s — cycle the metrics-view timescale band (span)
    Yank,
    TogglePause,
    ToggleHelp,
    ForceProbe,
    // Control layer (CONTROLS.md):
    Route(Lane),  // e/c/b — arm routing the selected domain into a lane
    Unroute,      // d — arm removing the selected domain (→ direct)
    // E/C/B/D — the same four edits on the selected host's parent suffix
    // (`x.y.z.com` → `.z.com`), so one keystroke covers the whole service
    // instead of the one hostname that happened to show up in the pane.
    RouteSuffix(Lane),
    UnrouteSuffix,
    ArmEdit(Edit), // a text key while armed — edit the entry in the confirm bar
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
    // Host search (`/`): open the editor, edit the pattern, commit/cancel.
    SearchOpen,          // / — open (prefilled with the active pattern to edit it)
    SearchInput(char),   // a printable key while editing → insert at the cursor
    SearchBackspace,     // delete the char before the cursor
    SearchDelete,        // delete the char at the cursor
    SearchCursor(i8),    // ←/→ move the block cursor
    SearchHome,          // Home / Ctrl-A
    SearchEnd,           // End / Ctrl-E
    SearchKillLine,      // Ctrl-U — clear the buffer
    SearchKillWord,      // Ctrl-W — delete the word before the cursor
    SearchCommit,        // Enter — commit the pattern, leave the editor
    SearchCancel,        // Esc — cancel the edit, revert to the committed pattern
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
    pub strip_w: u16,          // the ring's viewport width, fed back from the renderer
    pub strip_render_off: usize, // the marquee offset the renderer last drew (freeze to it → no jump)
    // The chip the renderer pinned at the strip's left edge (the active server) —
    // it sits *outside* the scrolling ring, so it's always visible and never needs
    // revealing. `None` = nothing pinned (pool fits, or too narrow to pin).
    pub strip_pin: Option<usize>,
    // Server-strip marquee runs off a resettable baseline (offset at t0), not raw
    // elapsed time, so unfreezing resumes from the frozen offset instead of jumping
    // to where a free-running clock would be.
    pub marquee_off0: usize,
    pub marquee_t0: Instant,

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

    // Connections pane view (`v` pans live → ↑ upload → ↓ download) + the metrics
    // timescale band (`w` when the pane is focused in a metrics view). `history` is
    // the per-domain byte history for the current band; `rows` is the unified
    // per-tick row list (live connections + top historical domains) rendered in
    // every view — see METRICS.md §5.
    pub conn_view: ConnView,
    pub band: MetricsBand,
    pub history: History,
    pub rows: Vec<ConnRow>,

    // Host-name search (`/`): a regex/literal filter on host rows, applied to both
    // panes and composed AND with the lane filter. Never touches the header agg.
    pub search: Search,
}

impl App {
    pub fn new(mut source: Box<dyn Source>) -> Self {
        let window = Window::M10;
        let snap = source.poll(window, None);
        let history = source.history(MetricsBand::Recent.spans(), None);
        let mut app = App {
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
            strip_pin: None,
            marquee_off0: 0,
            marquee_t0: Instant::now(),
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
            conn_view: ConnView::Live,
            band: MetricsBand::Recent,
            history,
            rows: Vec::new(),
            search: Search::default(),
        };
        app.rebuild_rows();
        app
    }

    /// Data tick: re-poll unless paused.
    pub fn tick(&mut self) {
        if !self.paused {
            self.snap = self.source.poll(self.window, self.lane_filter);
            self.refetch_history();
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
            self.clear_strip(); // resumes the marquee from the frozen offset
        }
    }

    /// Re-resolve each locked selection to its row's current index (rows re-sort
    /// on the tick); drop the lock if the domain has left the list (§5.2).
    fn resolve_keys(&mut self) {
        if let Some(k) = self.conn_key.clone() {
            match self.conn_row_keys().iter().position(|c| *c == k) {
                Some(i) => {
                    self.conn_sel = i;
                    self.ensure_visible(Focus::Conn);
                }
                None => self.conn_key = None,
            }
        }
        if let Some(k) = self.err_key.clone() {
            // Position within the *filtered* errors view (drops the lock if the
            // domain has been filtered out or has left the window).
            let pos = self.errors_view().iter().position(|e| e.domain == k);
            match pos {
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
        self.conn_row_keys().len()
    }
    fn err_len(&self) -> usize {
        self.errors_view().len()
    }

    pub fn update(&mut self, a: Action) {
        use Action::*;
        // Any real action counts as activity for the selection idle timeout
        // (mouse-move/hover doesn't go through here, so it won't keep it alive).
        self.last_input = Instant::now();
        // Arm lifecycle: a control-key (re)arms or commits; Confirm commits; Esc
        // is handled below; ANY other key cancels a pending arm, then proceeds.
        match a {
            Route(_) | Unroute | RouteSuffix(_) | UnrouteSuffix | ArmEdit(_) | Confirm | Escape => {}
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
            Route(lane) => self.arm(Some(lane), false),
            Unroute => self.arm(None, false),
            RouteSuffix(lane) => self.arm(Some(lane), true),
            UnrouteSuffix => self.arm(None, true),
            ArmEdit(e) => self.arm_edit(e),
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
            WindowCycle => self.cycle_window(1),
            WindowStep(d) => self.cycle_window(d as i32),
            WindowSet(win) => {
                self.window = win;
                self.tick_window();
            }
            ConnViewCycle => self.pan_conn_view(),
            BandCycle => self.cycle_band(),
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
            SearchOpen => self.search_open(),
            SearchInput(c) => self.search_input(c),
            SearchBackspace => self.search_backspace(),
            SearchDelete => self.search_delete(),
            SearchCursor(d) => self.search_move_cursor(d as i32),
            SearchHome => self.search.cursor = 0,
            SearchEnd => self.search.cursor = self.search.buf.chars().count(),
            SearchKillLine => self.search_kill_line(),
            SearchKillWord => self.search_kill_word(),
            SearchCommit => self.search_commit(),
            SearchCancel => self.search_cancel(),
        }
    }

    // ---- host search (`/`) ---------------------------------------------------

    /// Open the editor, prefilled with the committed pattern so `/` doubles as
    /// "edit the current search". The cursor lands at the end.
    fn search_open(&mut self) {
        self.search.editing = true;
        self.search.buf = self.search.committed.clone();
        self.search.cursor = self.search.buf.chars().count();
        self.recompile_search();
    }

    /// Byte offset of char index `i` within the editor buffer (len if past the end).
    fn search_byte_at(&self, i: usize) -> usize {
        self.search.buf.char_indices().nth(i).map(|(b, _)| b).unwrap_or(self.search.buf.len())
    }

    fn search_input(&mut self, c: char) {
        let b = self.search_byte_at(self.search.cursor);
        self.search.buf.insert(b, c);
        self.search.cursor += 1;
        self.recompile_search();
        self.on_search_change();
    }

    fn search_backspace(&mut self) {
        if self.search.cursor == 0 {
            return;
        }
        let b = self.search_byte_at(self.search.cursor - 1);
        self.search.buf.remove(b);
        self.search.cursor -= 1;
        self.recompile_search();
        self.on_search_change();
    }

    fn search_delete(&mut self) {
        if self.search.cursor >= self.search.buf.chars().count() {
            return;
        }
        let b = self.search_byte_at(self.search.cursor);
        self.search.buf.remove(b);
        self.recompile_search();
        self.on_search_change();
    }

    fn search_move_cursor(&mut self, d: i32) {
        let len = self.search.buf.chars().count() as i32;
        self.search.cursor = (self.search.cursor as i32 + d).clamp(0, len) as usize;
    }

    fn search_kill_line(&mut self) {
        self.search.buf.clear();
        self.search.cursor = 0;
        self.recompile_search();
        self.on_search_change();
    }

    /// Ctrl-W: delete the whitespace-delimited word before the cursor.
    fn search_kill_word(&mut self) {
        let chars: Vec<char> = self.search.buf.chars().collect();
        let mut i = self.search.cursor;
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        let kept: String = chars[..i].iter().chain(chars[self.search.cursor..].iter()).collect();
        self.search.buf = kept;
        self.search.cursor = i;
        self.recompile_search();
        self.on_search_change();
    }

    fn search_commit(&mut self) {
        self.search.editing = false;
        self.search.committed = self.search.buf.clone();
        self.recompile_search(); // now sourced from `committed` (== buf)
        self.search.changed_at = Some(Instant::now());
        self.on_search_change();
    }

    /// Esc while editing: drop the edit and revert to the committed pattern.
    fn search_cancel(&mut self) {
        self.search.editing = false;
        self.search.buf.clear();
        self.search.cursor = 0;
        self.recompile_search(); // reverts to `committed`
        self.on_search_change();
    }

    /// Esc while a committed filter is active and nothing is focused: clear it.
    fn search_clear(&mut self) {
        self.search = Search::default();
        self.on_search_change();
    }

    /// Recompile the effective matcher from the buffer (while editing) or the
    /// committed pattern (otherwise). Empty → no filtering.
    fn recompile_search(&mut self) {
        let src = if self.search.editing { &self.search.buf } else { &self.search.committed };
        self.search.matcher = Matcher::compile(src);
    }

    /// A committed (persisted) search filter is in effect — drives the footer
    /// indicator and the Esc-to-clear path. False while the editor is open.
    pub fn search_committed(&self) -> bool {
        !self.search.editing && !self.search.committed.is_empty()
    }

    /// The effective pattern iff something is being filtered (for the empty-match
    /// placeholder), whether typed live or committed.
    pub fn search_pattern(&self) -> Option<&str> {
        self.search.matcher.as_ref().map(|_| if self.search.editing { self.search.buf.as_str() } else { self.search.committed.as_str() })
    }

    /// (matched, before-search) connection-row counts — powers the `n/m` indicator.
    /// Both sides are lane-filtered (search composes AND with the lane filter); the
    /// denominator is the pane's visible count *before* search narrows it.
    pub fn search_counts(&self) -> (usize, usize) {
        let base: Vec<&ConnRow> = self.rows.iter().filter(|r| self.lane_filter.is_none_or(|l| r.lane == l)).collect();
        let m = base.len();
        let n = match self.search.matcher.as_ref() {
            Some(mm) => base.iter().filter(|r| mm.is_match(&r.host)).count(),
            None => m,
        };
        (n, m)
    }

    /// The search filter changed: reset both panes' selection/scroll (the selected
    /// host may have just vanished) — same reset the lane filter does.
    fn on_search_change(&mut self) {
        self.conn_sel = 0;
        self.conn_scroll = 0;
        self.conn_key = None;
        self.err_sel = 0;
        self.err_scroll = 0;
        self.err_key = None;
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
            Focus::Health => self.clear_strip(),
        }
        self.focus = f;
    }

    /// Clear the server-strip selection and **resume the marquee from the frozen
    /// offset** — reset the marquee baseline to the current position and restart
    /// its clock, so unfreezing continues scrolling from there instead of jumping
    /// to where a free-running clock would be.
    fn clear_strip(&mut self) {
        if self.strip_sel.take().is_some() {
            self.marquee_off0 = self.strip_off;
            self.marquee_t0 = Instant::now();
        }
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
    ///
    /// `broaden` is the uppercase `E`/`C`/`B`/`D` variant: the edit targets the
    /// host's parent suffix instead of the host.
    ///
    /// Where there is nothing to broaden — an IP, or a host that already IS its
    /// registrable domain (`x.com`) — it stays inert and says so. It does NOT
    /// quietly fall back to the host: `E` promises a suffix entry, and writing
    /// the lowercase edit under an uppercase key would make the two forms
    /// indistinguishable in the lane file afterwards.
    fn arm(&mut self, lane: Option<Lane>, broaden: bool) {
        let Some(host) = self.selected_domain() else { return };
        let lower = match lane {
            Some(Lane::Escape) => 'e',
            Some(Lane::Corp) => 'c',
            Some(Lane::Block) => 'b',
            Some(Lane::Direct) | None => 'd',
        };
        let domain = match broaden {
            false => host,
            true => match crate::model::parent_suffix(&host) {
                Some(suffix) => suffix,
                None => {
                    self.notify(format!("⚠ {host} has no parent suffix — {lower} adds it as-is"));
                    return;
                }
            },
        };
        // The armed key doubles as the commit key, so the two forms must stay
        // distinguishable even when they name the same lane.
        let key = if broaden { lower.to_ascii_uppercase() } else { lower };
        if let Some(a) = &self.armed {
            if a.key == key && a.domain == domain && a.lane == lane {
                self.commit_armed();
                return;
            }
        }
        // The cursor starts at the LEFT. A proposed entry is nearly always too
        // specific rather than too short, so the first thing anyone does is trim
        // from the front — and appending to a hostname is the rare case.
        let cursor = 0;
        self.armed = Some(Armed { domain, lane, key, at: Instant::now(), cursor, edited: false });
    }

    /// Edit the armed entry in place. Any op enters edit mode (so letters type
    /// from then on instead of re-arming) and restarts the arm timeout, so the
    /// 5s window measures inactivity rather than total time spent typing.
    fn arm_edit(&mut self, op: Edit) {
        let Some(a) = self.armed.as_mut() else { return };
        a.edited = true;
        a.at = Instant::now();
        let byte_at = |s: &str, i: usize| s.char_indices().nth(i).map(|(b, _)| b).unwrap_or(s.len());
        match op {
            Edit::Insert(c) => {
                let b = byte_at(&a.domain, a.cursor);
                a.domain.insert(b, c);
                a.cursor += 1;
            }
            Edit::Backspace => {
                if a.cursor > 0 {
                    let b = byte_at(&a.domain, a.cursor - 1);
                    a.domain.remove(b);
                    a.cursor -= 1;
                }
            }
            Edit::Delete => {
                if a.cursor < a.domain.chars().count() {
                    let b = byte_at(&a.domain, a.cursor);
                    a.domain.remove(b);
                }
            }
            Edit::Cursor(d) => {
                let len = a.domain.chars().count() as i32;
                a.cursor = (a.cursor as i32 + d as i32).clamp(0, len) as usize;
            }
            Edit::Home => a.cursor = 0,
            Edit::End => a.cursor = a.domain.chars().count(),
            Edit::DropLabel => {
                // Strip the LEADING label — how you widen a hostname by hand:
                // `i.ytimg.com` → `ytimg.com` → `com`. Position-independent, so
                // it does the same thing wherever the cursor happens to be, and
                // a no-op once no dot is left rather than emptying the field.
                if let Some(i) = a.domain.find('.') {
                    a.domain = a.domain[i + 1..].to_string();
                }
                a.cursor = 0;
            }
        }
    }

    fn commit_armed(&mut self) {
        let Some(a) = self.armed.take() else { return };
        // An edited-to-empty buffer is a cancel, not a write: `rowt <lane> add ""`
        // would be a usage error and `rm ""` a silent no-op.
        let entry = a.entry();
        if entry.is_empty() {
            self.notify("⚠ empty entry — nothing applied".to_string());
            return;
        }
        // bash `edit_list` would strip interior whitespace and write a DIFFERENT
        // string than the bar showed. Refuse rather than mangle.
        if entry.chars().any(char::is_whitespace) {
            self.notify(format!("⚠ {entry:?} has a space — not applied"));
            return;
        }
        // An over-broad entry is refused outright, not confirmed: the bar has
        // already been showing it in red, so `↵` here is a second mistake, and
        // routing an entire TLD is silent and total once written.
        if let Some(why) = crate::model::entry_risk(&entry) {
            self.notify(format!("⚠ {entry} is {why} — not applied"));
            return;
        }
        match a.lane {
            Some(l) => self.source.route_lane(&entry, l),
            None => self.source.unroute(&entry),
        }
        // Batch the reload: (re)start the 7s debounce (CONTROLS.md §4.3).
        self.pending_reload = Some(Instant::now() + RELOAD_DEBOUNCE);
        let dest = a.lane.map(Lane::label).unwrap_or("direct");
        self.notify(format!("{entry} → {dest}"));
    }

    /// Esc priority: cancel an arm, else clear the focused selection, else clear
    /// the host search, else clear the lane filter.
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
                self.clear_strip();
                return;
            }
            _ => {}
        }
        if self.search_committed() {
            self.search_clear();
            return;
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

    /// Take the server strip's geometry back from the renderer after a draw. All
    /// three values describe what was *actually* drawn — the ring's viewport width,
    /// the marquee offset it was drawn at, and which chip (if any) was pinned
    /// outside the ring — so freezing and scrolling mirror the screen exactly.
    /// Always feed them together; feeding one without the others desyncs the mirror.
    pub fn feed_strip(&mut self, hit: &crate::ui::Hit) {
        self.strip_w = hit.strip_w;
        self.strip_render_off = hit.strip_render_off;
        self.strip_pin = hit.strip_pin;
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
    /// marquee cell buffer in `draw_chips`. The **pinned** chip is held outside the
    /// ring by the renderer, so its start is `None` and it contributes no cells.
    fn strip_layout(&self) -> (Vec<Option<usize>>, Vec<u16>, usize) {
        let n = self.snap.chips.len();
        let mut starts = Vec::with_capacity(n);
        let mut widths = Vec::with_capacity(n);
        let mut cells = 0usize;
        for i in 0..n {
            let w = self.chip_w(i);
            widths.push(w);
            if self.strip_pin == Some(i) {
                starts.push(None);
                continue;
            }
            if cells > 0 {
                cells += 3;
            }
            starts.push(Some(cells));
            cells += w as usize;
        }
        (starts, widths, cells + 3)
    }

    /// Display column of chip `i`'s left edge within the ring window at offset
    /// `off`; `None` for the pinned chip, which isn't in the ring at all.
    fn chip_col(&self, starts: &[Option<usize>], span: usize, off: usize, i: usize) -> Option<usize> {
        starts[i].map(|st| (st + span - off % span) % span)
    }

    /// The first chip fully inside the viewport at offset `off` (what the first
    /// ←/→ selects); falls back to chip 0 if none fits. The pinned chip wins
    /// outright — it's held at the strip's left edge, so it's both always fully
    /// visible and the leftmost one on screen.
    fn first_fully_visible(&self, starts: &[Option<usize>], widths: &[u16], span: usize, off: usize) -> usize {
        if let Some(p) = self.strip_pin.filter(|&p| p < widths.len()) {
            return p;
        }
        let mut best: Option<(usize, usize)> = None; // (left column, index)
        for (i, &wd) in widths.iter().enumerate() {
            let Some(col) = self.chip_col(starts, span, off, i) else { continue };
            if col + wd as usize <= self.strip_w as usize && best.is_none_or(|(c, _)| col < c) {
                best = Some((col, i));
            }
        }
        best.map(|(_, i)| i).unwrap_or(0)
    }

    /// Scroll the frozen ring one cell at a time in the move direction `d` until
    /// the selected chip is fully visible (bounded by the ring span). The pinned
    /// chip never scrolls — it's already on screen — so it's a no-op there.
    fn reveal_strip(&mut self, si: usize, d: i32) {
        let (starts, widths, span) = self.strip_layout();
        if span == 0 || self.strip_w == 0 || starts[si].is_none() {
            return;
        }
        for _ in 0..span {
            match self.chip_col(&starts, span, self.strip_off, si) {
                Some(col) if col + widths[si] as usize <= self.strip_w as usize => return,
                _ => {}
            }
            self.strip_off = if d >= 0 { (self.strip_off + 1) % span } else { (self.strip_off + span - 1) % span };
        }
    }

    /// `w` / `[` / `]` — cycle the errors pane's rolling window. Global (focus-
    /// independent), like `f`/`v`. `d` is the step (+1 = cycle).
    fn cycle_window(&mut self, d: i32) {
        let n = Window::ALL.len() as i32;
        let i = (self.window.index() as i32 + d).rem_euclid(n);
        self.window = Window::ALL[i as usize];
        self.tick_window();
    }

    /// `s` — the metrics-view **span** (band) key, global. From the Live view it
    /// first engages the span view (▲ upload, matching `v`'s live→↑ ordering) so
    /// the bands become visible; once flipped it cycles the band (recent→days→year).
    fn cycle_band(&mut self) {
        if self.conn_view.is_live() {
            self.conn_view = ConnView::Up;
            self.ensure_visible(Focus::Conn);
            return;
        }
        let i = (self.band.index() + 1) % MetricsBand::ALL.len();
        self.band = MetricsBand::ALL[i];
        self.refetch_history();
        self.clamp_selection();
    }

    /// `v`: pan the connections pane across live / ↑ upload / ↓ download. The row
    /// list and order don't change (both directions are already loaded), so the
    /// selection stays put — only the visible columns change.
    fn pan_conn_view(&mut self) {
        self.conn_view = self.conn_view.pan_next();
        self.ensure_visible(Focus::Conn);
    }

    /// Re-query the per-domain history for the current band (ALL lanes — the lane
    /// filter scopes only the visible list, not the header) and rebuild the rows.
    fn refetch_history(&mut self) {
        self.history = self.source.history(self.band.spans(), None);
        self.rebuild_rows();
    }

    fn rebuild_rows(&mut self) {
        self.rows = self.build_conn_rows();
    }

    /// The lane- and search-filtered subset of `rows` — the detail list actually
    /// shown (and navigated). The full `rows` still back the per-lane header
    /// aggregate, so neither filter touches the header totals.
    pub fn visible_rows(&self) -> Vec<&ConnRow> {
        let m = self.search.matcher.as_ref();
        self.rows
            .iter()
            .filter(|r| self.lane_filter.is_none_or(|l| r.lane == l))
            .filter(|r| m.is_none_or(|mm| mm.is_match(&r.host)))
            .collect()
    }

    /// The search-filtered errors rows — the errors *detail* list. The per-category
    /// header counts (transient/persistent/blocked) come from the whole snapshot and
    /// are never filtered, mirroring the connections header aggregate.
    pub fn errors_view(&self) -> Vec<&ErrRow> {
        let m = self.search.matcher.as_ref();
        self.snap.errors.iter().filter(|e| m.is_none_or(|mm| mm.is_match(&e.domain))).collect()
    }

    /// The unified connections list: live connections (with their history looked
    /// up) plus top historical domains not currently connected, one row per host.
    /// Live rows sort first by throughput; dormant/historical rows follow, ranked
    /// by history over the band's widest column. Rendered greyed when `conns == 0`.
    fn build_conn_rows(&self) -> Vec<ConnRow> {
        let mut rows: Vec<ConnRow> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        // Live + this-session-dormant connections (ALL lanes — the lane filter
        // scopes only the visible detail list, never the per-lane header aggregate),
        // in the source's own order (live-by-throughput then session-dormant), so
        // the established connections ordering is preserved across the `v` pan.
        for c in self.snap.conns.iter().filter(|c| c.lane != Lane::Block) {
            let (hu, hd) = self.history.get(&c.host).map(|(_, u, d)| (*u, *d)).unwrap_or_default();
            rows.push(ConnRow {
                host: c.host.clone(),
                port: c.port,
                lane: c.lane,
                conns: c.conns,
                live_up: c.up,
                live_down: c.down,
                rule: c.rule.clone(),
                hist_up: hu,
                hist_down: hd,
            });
            seen.insert(c.host.clone());
        }
        // Historical domains not currently connected (already lane-scoped by the
        // query) — appended below the live list, ranked by history over the widest
        // column; host breaks ties so the order is deterministic (HashMap iteration
        // isn't). Rendered greyed (conns == 0).
        let mut extra: Vec<ConnRow> = self
            .history
            .iter()
            .filter(|(host, _)| !seen.contains(*host))
            .map(|(host, (lane_s, hu, hd))| ConnRow {
                host: host.clone(),
                port: 0,
                lane: Lane::from_label(lane_s),
                conns: 0,
                live_up: 0.0,
                live_down: 0.0,
                rule: String::new(),
                hist_up: *hu,
                hist_down: *hd,
            })
            .collect();
        extra.sort_by(|a, b| {
            (b.hist_down[3] + b.hist_up[3]).cmp(&(a.hist_down[3] + a.hist_up[3])).then_with(|| a.host.cmp(&b.host))
        });
        rows.extend(extra);
        rows
    }

    /// The domains of the connections pane's *visible* (lane-filtered) rows — the
    /// shared selection / route / yank machinery keys off these.
    pub fn conn_row_keys(&self) -> Vec<String> {
        self.visible_rows().iter().map(|r| r.key()).collect()
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
        self.rebuild_rows(); // rows are lane-independent; just refresh from the new poll
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
        match self.conn_row_keys().get(i).cloned() {
            Some(k) => {
                self.conn_key = Some(k);
                self.conn_sel = i;
                self.ensure_visible(Focus::Conn);
            }
            None => self.conn_key = None,
        }
    }

    fn set_err_index(&mut self, i: usize) {
        let dom = self.errors_view().get(i).map(|e| e.domain.clone());
        match dom {
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
            Focus::Conn => self.conn_row_keys().get(self.conn_sel).cloned(),
            Focus::Err => self.errors_view().get(self.err_sel).map(|e| e.domain.clone()),
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
