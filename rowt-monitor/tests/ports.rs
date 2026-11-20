//! The port scope (`P`): the connections pane shows one port's traffic, its
//! header rates and history included, while the errors pane stays whole.

use rowt_monitor::app::{Action, App};
use rowt_monitor::model::{Lane, Snapshot, Window};
use rowt_monitor::source::{FixtureSource, History, Source};
use rowt_monitor::{render_app_text, render_text};

const GOLDEN_VM: &str = "renders/rowt-monitor-port-vm-150x30.txt";

fn app() -> App {
    std::env::set_var("ROWT_MONITOR_NO_CLIPBOARD", "1");
    App::new(Box::new(FixtureSource::still()))
}

fn line_with<'a>(frame: &'a str, needle: &str) -> &'a str {
    frame.lines().find(|l| l.contains(needle)).unwrap_or_else(|| panic!("no line with {needle:?} in:\n{frame}"))
}

#[test]
fn p_cycles_main_then_each_named_port_then_all() {
    let mut a = app();
    assert_eq!(a.snap.ports, vec!["main", "vm"]);
    assert_eq!(a.port_scope, None);
    a.update(Action::PortCycle);
    assert_eq!(a.port_scope.as_deref(), Some("main"));
    a.update(Action::PortCycle);
    assert_eq!(a.port_scope.as_deref(), Some("vm"));
    a.update(Action::PortCycle);
    assert_eq!(a.port_scope, None, "past the last port it shows every port again");
}

#[test]
fn scoped_to_vm_the_pane_counts_only_vm() {
    let mut a = app();
    a.update(Action::PortCycle);
    a.update(Action::PortCycle);
    let frame = render_app_text(&a, 150, 30);
    // Pinned frame, so a layout change to the scoped view is a deliberate one.
    // Regenerate with ROWT_MONITOR_BLESS=1 and read the diff.
    if std::env::var_os("ROWT_MONITOR_BLESS").is_some() {
        std::fs::write(GOLDEN_VM, &frame).unwrap();
    }
    let golden = std::fs::read_to_string(GOLDEN_VM).expect("the vm golden (bless it once)");
    assert_eq!(frame, golden);

    assert!(line_with(&frame, "live connections").contains("live connections · port vm"));
    assert!(line_with(&frame, "errors & blocked").contains("errors & blocked · all ports"));
    // Only vm's hosts, and the header counts only them.
    assert!(frame.contains("mirrors.debian.org"));
    assert!(frame.contains("api.anthropic.com"));
    assert!(!frame.contains("i.ytimg.com"), "main's traffic is not vm's");
    assert!(!frame.contains("jira.corp.example.com"));
    let all = line_with(&frame, "│ all ");
    assert!(all.contains("↑ 401 KB/s"), "vm's rates, not the whole router's: {all:?}");
    // The errors pane is not scoped: its rows are all still there.
    assert!(frame.contains("vpn.corp.example.com"));
}

#[test]
fn scoped_history_shows_only_that_ports_domains() {
    let mut a = app();
    a.update(Action::PortCycle);
    a.update(Action::PortCycle);
    a.update(Action::ConnViewCycle); // ↑ upload history
    let hosts: Vec<&str> = a.rows.iter().map(|r| r.host.as_str()).collect();
    assert!(hosts.contains(&"mirrors.debian.org"));
    assert!(!hosts.contains(&"netflix.com"), "a main-only dormant domain: {hosts:?}");
    assert!(!hosts.contains(&"www.youtube.com"), "{hosts:?}");
}

#[test]
fn unscoped_after_a_full_cycle_is_the_golden_frame_again() {
    let mut a = app();
    for _ in 0..3 {
        a.update(Action::PortCycle);
    }
    assert_eq!(render_app_text(&a, 150, 30), render_text(150, 30));
}

#[test]
fn esc_clears_the_port_scope_after_the_lane_filter() {
    let mut a = app();
    a.update(Action::PortCycle);
    a.update(Action::PortCycle);
    a.update(Action::LaneSet(Some(Lane::Direct)));
    a.update(Action::Escape);
    assert_eq!(a.lane_filter, None, "the lane filter goes first");
    assert_eq!(a.port_scope.as_deref(), Some("vm"));
    a.update(Action::Escape);
    assert_eq!(a.port_scope, None);
}

/// The fixture as a router with no named port.
struct MainOnly(FixtureSource);

impl Source for MainOnly {
    fn poll(&mut self, window: Window, lane: Option<Lane>) -> Snapshot {
        let mut s = self.0.poll(window, lane);
        s.ports = vec!["main".to_string()];
        s
    }
    fn history(&mut self, spans: [i64; 4], lane: Option<Lane>) -> History {
        self.0.history(spans, lane)
    }
    fn set_port(&mut self, port: Option<&str>) {
        self.0.set_port(port)
    }
}

#[test]
fn with_only_main_p_says_how_to_add_a_port() {
    let mut a = App::new(Box::new(MainOnly(FixtureSource::still())));
    a.update(Action::PortCycle);
    assert_eq!(a.port_scope, None);
    let toast = a.toast.as_ref().map(|(m, _)| m.as_str()).unwrap_or("");
    assert!(toast.contains("rowt port add"), "{toast:?}");
}

/// The fixture as a router whose `vm` port was just removed.
struct VmRemoved(FixtureSource);

impl Source for VmRemoved {
    fn poll(&mut self, window: Window, lane: Option<Lane>) -> Snapshot {
        let mut s = self.0.poll(window, lane);
        s.ports.retain(|p| p != "vm");
        s
    }
    fn set_port(&mut self, port: Option<&str>) {
        self.0.set_port(port)
    }
}

#[test]
fn a_removed_port_drops_the_scope_on_the_next_tick() {
    let mut a = app();
    a.update(Action::PortCycle);
    a.update(Action::PortCycle);
    assert_eq!(a.port_scope.as_deref(), Some("vm"));
    a.source = Box::new(VmRemoved(FixtureSource::still()));
    a.source.set_port(Some("vm"));
    a.tick();
    assert_eq!(a.port_scope, None);
    let toast = a.toast.as_ref().map(|(m, _)| m.as_str()).unwrap_or("");
    assert!(toast.contains("port vm is gone"), "{toast:?}");
}
