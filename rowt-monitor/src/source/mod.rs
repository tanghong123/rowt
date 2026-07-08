//! Data sources sit behind one trait so the UI can run against fixtures first
//! and real adapters later. A source is polled once per 2s tick with the
//! currently-selected error window (it scopes the log re-aggregation).

use crate::model::{Lane, Snapshot, Window};

pub mod fixtures;
pub mod parse;
pub mod live;

pub trait Source {
    /// Produce one observation. `window` scopes the errors/blocked aggregation;
    /// `lane` (the active lane filter) scopes both the connections list and the
    /// errors pane (None = all lanes).
    fn poll(&mut self, window: Window, lane: Option<Lane>) -> Snapshot;

    /// Force an immediate server re-probe (and reset the periodic timer). No-op
    /// for sources without an active prober (e.g. fixtures).
    fn force_probe(&self) {}

    /// Short human label for what's being observed (shown nowhere critical;
    /// useful for a future status line / --help). Fixtures say so plainly.
    fn label(&self) -> &str {
        "live"
    }
}

pub use fixtures::FixtureSource;
pub use live::LiveSource;
