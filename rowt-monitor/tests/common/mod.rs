//! Test support shared by the integration tests (`mod common;`).
#![allow(dead_code)] // each test crate uses its own subset

use std::sync::{Arc, Mutex};

use rowt_monitor::model::{Lane, Server, Snapshot, Window, AUTO_GROUP};
use rowt_monitor::source::{FixtureSource, History, Source};

/// Which selection the recording source reports.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    /// The still fixture as shipped: `JP-Tokyo` pinned.
    Manual,
    /// Auto server selection with urltest's live pick; `None` = not resolved yet.
    Auto(Option<&'static str>),
}

/// The still fixture, switchable into auto mode mid-test — the mode is a shared
/// handle, so a test can play "rowt wrote the new selection" — recording every
/// `use_server` call (and a batched reload as `"reload"`), the control layer's
/// observable effects. `busy` plays "a router restart is still in flight".
pub struct Recording {
    inner: FixtureSource,
    pub mode: Arc<Mutex<Mode>>,
    pub calls: Arc<Mutex<Vec<String>>>,
    pub busy: Arc<Mutex<bool>>,
}

impl Recording {
    pub fn new(mode: Mode) -> Self {
        Recording {
            inner: FixtureSource::still(),
            mode: Arc::new(Mutex::new(mode)),
            calls: Arc::new(Mutex::new(Vec::new())),
            busy: Arc::new(Mutex::new(false)),
        }
    }
}

impl Source for Recording {
    fn poll(&mut self, window: Window, lane: Option<Lane>) -> Snapshot {
        let mut s = self.inner.poll(window, lane);
        if let Mode::Auto(pick) = *self.mode.lock().unwrap() {
            // What `LiveSource` reports in auto mode: the state names the group,
            // urltest's pick is the active chip — first, and held there even with
            // no probe reading yet — and the header names it.
            s.active_server = AUTO_GROUP.to_string();
            s.auto_now = pick.map(str::to_string);
            for c in &mut s.chips {
                c.active = Some(c.name.as_str()) == pick;
            }
            if let Some(p) = pick {
                if !s.chips.iter().any(|c| c.name == p) {
                    s.chips.push(Server { name: p.to_string(), ms: None, active: true });
                }
            }
            s.chips.sort_by_key(|c| !c.active);
            s.identity.server_name = pick.unwrap_or(AUTO_GROUP).to_string();
        }
        s
    }

    fn history(&mut self, spans: [i64; 4], lane: Option<Lane>) -> History {
        self.inner.history(spans, lane)
    }

    fn use_server(&self, tag: &str) {
        self.calls.lock().unwrap().push(tag.to_string());
    }

    fn reload_router(&self) {
        self.calls.lock().unwrap().push("reload".to_string());
    }

    fn restart_in_flight(&self) -> bool {
        *self.busy.lock().unwrap()
    }

    fn label(&self) -> &str {
        "recording"
    }
}
