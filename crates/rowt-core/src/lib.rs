//! `rowt-core` — the parts of rowt that are pure.
//!
//! Nothing here touches the network, the clock, or the OS. Values that require
//! any of those (the physical interface, the clash secret, a DNS answer) are
//! inputs, so the platform seam stays in the shell until Phase 5 of PORTING.md.
//!
//! Each module is held to the bash it replaces by a differential gate in
//! `tests/parity` rather than by inspection.

pub mod bplist;
pub mod classify;
pub mod foreign;
pub mod foreignio;
pub mod geosite;
pub mod importmerge;
pub mod laneerr;
pub mod lanes;
pub mod netdetect;
pub mod pycli;
pub mod pyjson;
pub mod pypath;
pub mod pyurl;
pub mod reconcile;
pub mod render;
pub mod sharelink;
pub mod srimport;
pub mod srio;
pub mod watch;

pub use classify::{classify, Classification, ClassifyInput, Lane};
pub use render::{
    geosites_of, group, parse_list, render_host, render_vm, Filter, Geo, HostInput, Lists,
};
