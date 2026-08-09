//! The platform layer: everything rowt asks the operating system.
//!
//! `rowt-core` is pure by construction — the values that require the network,
//! the clock or the OS arrive as inputs. This crate is where those inputs come
//! from, and where the FSM's `Action`s are carried out.
//!
//! The macOS implementation is held to the shell not by inspection but by argv:
//! `parity platform-diff` runs both under the same recorder shims and requires
//! the invocations to match exactly. A cutover that changed *what* gets executed
//! would be a behavior change wearing a refactor's clothes.

use std::process::{Command, Stdio};

/// What rowt needs from the operating system.
pub trait Platform {
    /// The network service the physical interface belongs to ("Wi-Fi").
    fn active_service(&self) -> Option<String>;
    /// The physical interface with both an address and a gateway; None = offline.
    fn detect_iface(&self) -> Option<String>;
    /// Is any proxy protocol currently enabled for this service?
    fn proxy_any_on(&self, service: &str) -> bool;
    /// Are all three protocols pointed at 127.0.0.1:port?
    fn proxy_pointing_ok(&self, service: &str, port: u16) -> bool;
    /// Point all three protocols at 127.0.0.1:port and enable them.
    fn proxy_set(&self, service: &str, port: u16) -> Result<(), String>;
    /// Replace the bypass list.
    fn proxy_set_bypass(&self, service: &str, entries: &[String]) -> Result<(), String>;
    /// Turn the three protocols off. `passwordless` uses `sudo -n`, which is what
    /// the watchdog's captive and stale-proxy paths do.
    fn proxy_states_off(&self, service: &str, passwordless: bool) -> Result<(), String>;
    fn proxy_states_on(&self, service: &str, passwordless: bool) -> Result<(), String>;
    /// Monotonic-ish boot identity: a reboot changes it.
    fn boot_id(&self) -> Option<String>;
    /// True when the escape lane's own destinations answer on the physical NIC —
    /// i.e. there is no censorship here to tunnel around, and `local` mode is
    /// the right choice. Decides a mode, so it belongs to the platform, not the
    /// pure core: it dials the network over a named interface.
    fn direct_reaches_escape(&self, canaries: &[String], timeout: u32) -> bool;
}

fn out(cmd: &str, args: &[&str]) -> Option<String> {
    let o = Command::new(cmd).args(args).stderr(Stdio::null()).output().ok()?;
    Some(String::from_utf8_lossy(&o.stdout).into_owned())
}

fn run(cmd: &str, args: &[&str]) -> Result<(), String> {
    let st = Command::new(cmd)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("{cmd}: {e}"))?;
    if st.success() {
        Ok(())
    } else {
        Err(format!("{cmd} {}: exit {:?}", args.join(" "), st.code()))
    }
}

pub struct Mac;

/// The three protocols, in the order the shell touches them. Order is part of
/// the observable behavior: the argv trace is compared as a sequence.
const PROTOS: [(&str, &str); 3] = [
    ("-setsocksfirewallproxy", "-setsocksfirewallproxystate"),
    ("-setwebproxy", "-setwebproxystate"),
    ("-setsecurewebproxy", "-setsecurewebproxystate"),
];
const GETTERS: [&str; 3] = ["-getsocksfirewallproxy", "-getwebproxy", "-getsecurewebproxy"];

impl Mac {
    /// `sudo -n /usr/sbin/networksetup …` for the watchdog's paths, plain
    /// `sudo networksetup …` for the interactive ones — the shell distinguishes
    /// them and so must this, because the argv differs.
    fn sudo_networksetup(&self, passwordless: bool, args: &[&str]) -> Result<(), String> {
        let mut a: Vec<&str> = Vec::new();
        if passwordless {
            a.push("-n");
            a.push("/usr/sbin/networksetup");
        } else {
            a.push("networksetup");
        }
        a.extend_from_slice(args);
        run("sudo", &a)
    }

    /// `_iface_up`: an address AND a gateway.
    fn iface_up(&self, ifc: &str) -> bool {
        let addr = out("ipconfig", &["getifaddr", ifc]).unwrap_or_default();
        if addr.trim().is_empty() {
            return false;
        }
        let router = out("ipconfig", &["getoption", ifc, "router"]).unwrap_or_default();
        !router.trim().is_empty()
    }
}

impl Platform for Mac {
    fn active_service(&self) -> Option<String> {
        let dev = self.detect_iface()?;
        let body = out("networksetup", &["-listnetworkserviceorder"])?;
        // The shell's awk: remember the most recent "Hardware Port: X, Device: Y"
        // header, and print X when Y matches.
        let mut svc = String::new();
        for line in body.lines() {
            if let Some(i) = line.find("Hardware Port: ") {
                svc = line[i + "Hardware Port: ".len()..]
                    .split(',')
                    .next()
                    .unwrap_or("")
                    .to_string();
            }
            if line.contains(&format!("Device: {dev})")) {
                return Some(svc);
            }
        }
        None
    }

    fn detect_iface(&self) -> Option<String> {
        if let Ok(forced) = std::env::var("ROWT_IFACE") {
            if !forced.is_empty() {
                return Some(forced);
            }
        }
        // The default route's interface first, if it is an `en*` that is really up.
        if let Some(body) = out("route", &["-n", "get", "default"]) {
            for line in body.lines() {
                if let Some(rest) = line.trim().strip_prefix("interface:") {
                    let ifc = rest.trim().to_string();
                    if ifc.starts_with("en") && self.iface_up(&ifc) {
                        return Some(ifc);
                    }
                }
            }
        }
        // Otherwise the first `en*` hardware port that is up.
        let ports = out("networksetup", &["-listallhardwareports"])?;
        for line in ports.lines() {
            if let Some(rest) = line.trim().strip_prefix("Device:") {
                let ifc = rest.trim().to_string();
                if ifc.starts_with("en") && self.iface_up(&ifc) {
                    return Some(ifc);
                }
            }
        }
        None
    }

    fn proxy_any_on(&self, service: &str) -> bool {
        // Short-circuits exactly like the shell's `||` chain, so an already-on
        // socks proxy means the other two are never queried.
        for g in GETTERS {
            let body = out("networksetup", &[g, service]).unwrap_or_default();
            if body.lines().any(|l| l.starts_with("Enabled: Yes")) {
                return true;
            }
        }
        false
    }

    fn proxy_pointing_ok(&self, service: &str, port: u16) -> bool {
        for g in GETTERS {
            let body = out("networksetup", &[g, service]).unwrap_or_default();
            let (mut e, mut s, mut p) = (String::new(), String::new(), String::new());
            for l in body.lines() {
                if let Some(v) = l.strip_prefix("Enabled:") {
                    e = v.trim().to_string();
                } else if let Some(v) = l.strip_prefix("Server:") {
                    s = v.trim().to_string();
                } else if let Some(v) = l.strip_prefix("Port:") {
                    p = v.trim().to_string();
                }
            }
            if !(e == "Yes" && s == "127.0.0.1" && p == port.to_string()) {
                return false;
            }
        }
        true
    }

    fn proxy_set(&self, service: &str, port: u16) -> Result<(), String> {
        let p = port.to_string();
        for (set, state) in PROTOS {
            self.sudo_networksetup(false, &[set, service, "127.0.0.1", &p])?;
            self.sudo_networksetup(false, &[state, service, "on"])?;
        }
        Ok(())
    }

    fn proxy_set_bypass(&self, service: &str, entries: &[String]) -> Result<(), String> {
        let mut args: Vec<&str> = vec!["-setproxybypassdomains", service];
        args.extend(entries.iter().map(|s| s.as_str()));
        self.sudo_networksetup(false, &args)
    }

    fn proxy_states_off(&self, service: &str, passwordless: bool) -> Result<(), String> {
        for (_, state) in PROTOS {
            // The shell tolerates failures on the second and third here, so a
            // partial result is not an error.
            let _ = self.sudo_networksetup(passwordless, &[state, service, "off"]);
        }
        Ok(())
    }

    fn proxy_states_on(&self, service: &str, passwordless: bool) -> Result<(), String> {
        for (_, state) in PROTOS {
            let _ = self.sudo_networksetup(passwordless, &[state, service, "on"]);
        }
        Ok(())
    }

    fn direct_reaches_escape(&self, canaries: &[String], timeout: u32) -> bool {
        // Bound to the interface and with the proxy bypassed, so it measures the
        // DIRECT path rather than whatever the router is doing. A 2xx/3xx means
        // TLS completed against the real host, which censorship cannot forge; one
        // strict hit is enough. Every failure mode — offline, canary down, flaky
        // link — reads as "keep the tunnel", the safe direction.
        let Some(ifc) = self.detect_iface() else { return false };
        let t = timeout.to_string();
        for url in canaries {
            let body = out(
                "curl",
                &["--interface", &ifc, "--noproxy", "*", "-sS", "-o", "/dev/null",
                  "-w", "%{http_code}", "-m", &t, url],
            )
            .unwrap_or_default();
            let code = body.trim();
            if code.starts_with('2') || code.starts_with('3') {
                return true;
            }
        }
        false
    }

    fn boot_id(&self) -> Option<String> {
        let body = out("sysctl", &["-n", "kern.boottime"])?;
        // `sed -n 's/[^0-9]*\([0-9][0-9]*\).*/\1/p'` — the first run of digits.
        let digits: String = body
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if digits.is_empty() {
            None
        } else {
            Some(digits)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_protocol_order_is_part_of_the_behavior() {
        // The argv trace is compared as a sequence, so this order is load-bearing.
        assert_eq!(PROTOS[0].1, "-setsocksfirewallproxystate");
        assert_eq!(PROTOS[2].0, "-setsecurewebproxy");
        assert_eq!(GETTERS[1], "-getwebproxy");
    }
}

/// Raw output of one `networksetup` proxy getter — the CLI formats it the way
/// the shell's awk does, so the parsing and the formatting stay separable.
pub fn read_proxy(service: &str, flag: &str) -> String {
    out("networksetup", &[flag, service]).unwrap_or_default()
}

/// The bypass list as the shell prints it: newlines squashed to spaces.
pub fn read_bypass(service: &str) -> String {
    let body = out("networksetup", &["-getproxybypassdomains", service]).unwrap_or_default();
    let mut s = body.replace('\n', " ");
    while s.ends_with("  ") {
        s.pop();
    }
    s
}

/// `_proxy_bypass_ok`: the configured list, sorted, equals what rowt wants.
pub fn bypass_ok(service: &str) -> bool {
    let body = out("networksetup", &["-getproxybypassdomains", service]).unwrap_or_default();
    let mut have: Vec<&str> = body.lines().filter(|l| !l.trim().is_empty()).collect();
    have.sort_unstable();
    let mut want: Vec<&str> = bypass_want().to_vec();
    want.sort_unstable();
    have == want
}

/// `_proxy_bypass_want` — one source of truth for what must never be proxied.
pub fn bypass_want() -> &'static [&'static str] {
    &[
        "*.local", "169.254/16", "127.0.0.1", "localhost", "*.arpa",
        "10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16",
        "captive.apple.com", "connectivitycheck.gstatic.com",
        "detectportal.firefox.com", "www.msftconnecttest.com",
    ]
}
