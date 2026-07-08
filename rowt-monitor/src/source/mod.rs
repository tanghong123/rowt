//! Data sources sit behind one trait so the UI can run against fixtures first
//! and real adapters later. A source is polled once per 2s tick with the
//! currently-selected error window (it scopes the log re-aggregation).

use crate::model::{Snapshot, Window};

pub mod fixtures;
pub mod parse;
pub mod live;

pub trait Source {
    /// Produce one observation. `window` scopes the errors/blocked aggregation.
    fn poll(&mut self, window: Window) -> Snapshot;

    /// Short human label for what's being observed (shown nowhere critical;
    /// useful for a future status line / --help). Fixtures say so plainly.
    fn label(&self) -> &str {
        "live"
    }
}

pub use fixtures::FixtureSource;
pub use live::LiveSource;
