//! Real adapters over the six data sources (README "Data provenance"). Filled
//! in task 5; for now this is a thin placeholder that falls back to fixtures so
//! the binary always has something to show.

use crate::model::{Snapshot, Window};
use crate::source::{FixtureSource, Source};

pub struct LiveSource {
    fallback: FixtureSource,
}

impl LiveSource {
    pub fn new() -> Self {
        LiveSource { fallback: FixtureSource::new() }
    }
}

impl Default for LiveSource {
    fn default() -> Self {
        Self::new()
    }
}

impl Source for LiveSource {
    fn label(&self) -> &str {
        "live"
    }

    fn poll(&mut self, window: Window) -> Snapshot {
        // TODO(task 5): clash API + config/state/logs/system; fall back per-field.
        self.fallback.poll(window)
    }
}
