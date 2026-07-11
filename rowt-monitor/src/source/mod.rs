//! Data sources sit behind one trait so the UI can run against fixtures first
//! and real adapters later. A source is polled once per 2s tick with the
//! currently-selected error window (it scopes the log re-aggregation).

use crate::model::{Lane, Snapshot, Window};

pub mod fixtures;
pub mod parse;
pub mod live;

/// Result of a control command (server switch / lane edit / proxy toggle),
/// surfaced back to the UI as a transient footer toast. Errors carry the CLI's
/// first stderr line so a failure (e.g. `proxy on` with the router down) is
/// visible rather than a silent no-op (CONTROLS.md §9.2).
#[derive(Clone, Debug)]
pub enum CtlOutcome {
    Ok(String),
    Err(String),
}

pub trait Source {
    /// Produce one observation. `window` scopes the errors/blocked aggregation;
    /// `lane` (the active lane filter) scopes both the connections list and the
    /// errors pane (None = all lanes).
    fn poll(&mut self, window: Window, lane: Option<Lane>) -> Snapshot;

    /// Force an immediate server re-probe (and reset the periodic timer). No-op
    /// for sources without an active prober (e.g. fixtures).
    fn force_probe(&self) {}

    // ---- control layer (CONTROLS.md) — default no-ops so fixtures stay inert.
    // Each spawns the corresponding `rowt` command off the UI thread and reports
    // its outcome via `drain_ctl`. Lane edits pass `--no-reload`; the reload is
    // batched by the UI and fired via `reload_router` on the debounce timer.

    /// Switch the active outbound server (`rowt use <tag>` — live, no reload).
    fn use_server(&self, _tag: &str) {}
    /// Route a domain into a lane (`rowt <lane> add <domain> --no-reload`).
    fn route_lane(&self, _domain: &str, _lane: Lane) {}
    /// Remove a domain from every editable lane → back to direct (`--no-reload`).
    fn unroute(&self, _domain: &str) {}
    /// Toggle the macOS system proxy (`rowt proxy on|off`).
    fn set_proxy(&self, _on: bool) {}
    /// Issue the single batched router reload after lane edits settle.
    fn reload_router(&self) {}
    /// Drain any completed control-command outcomes (for the footer toast).
    fn drain_ctl(&self) -> Vec<CtlOutcome> {
        Vec::new()
    }

    /// Short human label for what's being observed (shown nowhere critical;
    /// useful for a future status line / --help). Fixtures say so plainly.
    fn label(&self) -> &str {
        "live"
    }
}

pub use fixtures::FixtureSource;
pub use live::LiveSource;
