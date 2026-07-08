//! Real adapters over the six data sources (README "Data provenance"):
//! clash API (`/traffic`, `/connections`, `/proxies`), `host.json`,
//! `state`/`servers.json`, `lane-*.log`, and host system facts.
//!
//! Every field degrades gracefully: if the clash API is unreachable the binary
//! still renders from config/state/logs; if there is no rowt config at all it
//! falls back to the demo fixture so `rowt-monitor` always shows *something*.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::model::*;
use crate::source::parse::{self, ErrAgg, RawConn};
use crate::source::{FixtureSource, Source};

pub struct LiveSource {
    cfg: PathBuf,
    clash_port: u16,
    proxy_port: u16,
    fallback: FixtureSource,

    // rate computation: previous cumulative byte counts per connection id
    prev: HashMap<String, (u64, u64)>,
    prev_at: Option<Instant>,

    // errors cache (re-aggregated on window change or every few seconds)
    err_cache: Option<(Window, Instant, ErrAgg)>,

    started: Instant,
    uptime_base: Option<u64>, // proxy process uptime (secs) sampled once
}

impl LiveSource {
    pub fn new() -> Self {
        let cfg = config_dir();
        let clash_port = env_port("ROWT_CLASH_PORT", 9090);
        let proxy_port = env_port("ROWT_PORT", 7890);
        let uptime_base = proxy_uptime_secs();
        LiveSource {
            cfg,
            clash_port,
            proxy_port,
            fallback: FixtureSource::new(),
            prev: HashMap::new(),
            prev_at: None,
            err_cache: None,
            started: Instant::now(),
            uptime_base,
        }
    }

    fn clash_secret(&self) -> Option<String> {
        let text = std::fs::read_to_string(self.cfg.join("state")).ok()?;
        parse::parse_state(&text).get("clash_secret").cloned()
    }

    fn clash_get(&self, path: &str) -> Option<Value> {
        let url = format!("http://127.0.0.1:{}{}", self.clash_port, path);
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_millis(400))
            .build();
        let mut req = agent.get(&url);
        if let Some(s) = self.clash_secret() {
            req = req.set("Authorization", &format!("Bearer {}", s));
        }
        req.call().ok()?.into_json().ok()
    }

    /// Read a single sample from the streaming `/traffic` endpoint.
    fn clash_traffic(&self) -> Option<(f64, f64)> {
        let url = format!("http://127.0.0.1:{}/traffic", self.clash_port);
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_millis(400))
            .build();
        let mut req = agent.get(&url);
        if let Some(s) = self.clash_secret() {
            req = req.set("Authorization", &format!("Bearer {}", s));
        }
        let resp = req.call().ok()?;
        let mut line = String::new();
        BufReader::new(resp.into_reader()).read_line(&mut line).ok()?;
        let v: Value = serde_json::from_str(line.trim()).ok()?;
        Some((v.get("up")?.as_f64()?, v.get("down")?.as_f64()?))
    }

    fn escape_tags(&self, host: &Value) -> HashSet<String> {
        let mut tags: HashSet<String> = HashSet::new();
        tags.insert("escape".to_string());
        if let Some(obs) = host.get("outbounds").and_then(|o| o.as_array()) {
            for o in obs {
                if o.get("tag").and_then(|t| t.as_str()) == Some("escape") {
                    if let Some(m) = o.get("outbounds").and_then(|m| m.as_array()) {
                        tags.extend(m.iter().filter_map(|s| s.as_str().map(str::to_string)));
                    }
                }
            }
        }
        tags
    }

    /// Compute per-connection byte rates from the delta since the last poll.
    fn rates(&mut self, raw: &[RawConn]) -> HashMap<String, (f64, f64)> {
        let now = Instant::now();
        let dt = self.prev_at.map(|t| now.duration_since(t).as_secs_f64()).unwrap_or(0.0);
        let mut out = HashMap::new();
        let mut cur = HashMap::new();
        for c in raw {
            cur.insert(c.id.clone(), (c.up, c.down));
            if dt > 0.05 {
                if let Some(&(pu, pd)) = self.prev.get(&c.id) {
                    let u = c.up.saturating_sub(pu) as f64 / dt;
                    let d = c.down.saturating_sub(pd) as f64 / dt;
                    out.insert(c.id.clone(), (u, d));
                }
            }
        }
        self.prev = cur;
        self.prev_at = Some(now);
        out
    }

    fn read_errors(&self, window: Window) -> ErrAgg {
        let mut events = Vec::new();
        for (file, is_block) in [
            ("lane-escape.log", false),
            ("lane-corp.log", false),
            ("lane-direct.log", false),
            ("lane-block.log", true),
        ] {
            if let Some(text) = tail_read(&self.cfg.join("log").join(file), 512 * 1024) {
                events.extend(parse::parse_lane_log(&text, is_block));
            }
        }
        parse::aggregate_errors(&events, parse::window_secs(window))
    }

    fn errors(&mut self, window: Window) -> ErrAgg {
        let fresh = match &self.err_cache {
            Some((w, at, _)) => *w != window || at.elapsed() > Duration::from_secs(6),
            None => true,
        };
        if fresh {
            let agg = self.read_errors(window);
            self.err_cache = Some((window, Instant::now(), agg));
        }
        self.err_cache.as_ref().unwrap().2.clone()
    }

    fn servers(&self, host: &Value, state: &HashMap<String, String>) -> (u32, u32, u32, String, Vec<Server>, Option<u32>) {
        let selected = state.get("selected").cloned().unwrap_or_default();
        let members: Vec<String> = self
            .escape_tags(host)
            .into_iter()
            .filter(|t| t != "escape")
            .collect();
        // latencies from /proxies (clash): proxies[tag].history[-1].delay
        let proxies = self.clash_get("/proxies");
        let delay_of = |tag: &str| -> Option<u32> {
            let p = proxies.as_ref()?.get("proxies")?.get(tag)?;
            let h = p.get("history")?.as_array()?;
            let last = h.last()?;
            let d = last.get("delay")?.as_u64()? as u32;
            if d == 0 {
                None
            } else {
                Some(d)
            }
        };
        let mut up = 0u32;
        let mut down = 0u32;
        let mut chips = Vec::new();
        let mut active_ms = None;
        for tag in &members {
            match delay_of(tag) {
                Some(ms) => {
                    up += 1;
                    if *tag == selected {
                        active_ms = Some(ms);
                    } else {
                        chips.push(Server { name: tag.clone(), ms });
                    }
                }
                None => down += 1,
            }
        }
        chips.sort_by_key(|c| c.ms);
        let total = members.len() as u32;
        (total, up, down, selected, chips, active_ms)
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
        // No rowt config at all -> demo fixture so we always render something.
        let host_text = std::fs::read_to_string(self.cfg.join("host.json"));
        let Ok(host_text) = host_text else {
            return self.fallback.poll(window);
        };
        let host: Value = serde_json::from_str(&host_text).unwrap_or(Value::Null);
        let config_ok = host.is_object();
        let state = std::fs::read_to_string(self.cfg.join("state"))
            .map(|t| parse::parse_state(&t))
            .unwrap_or_default();
        let escape = self.escape_tags(&host);

        // Connections + throughput from clash (empty if unreachable).
        let conns_json = self.clash_get("/connections");
        let router_up = conns_json.is_some();
        let raw = conns_json.as_ref().map(parse::parse_connections).unwrap_or_default();
        let rates = self.rates(&raw);
        let (conns, lanes, mut all) = parse::build_conn_rows(&raw, &rates, &escape);
        if let Some((u, d)) = self.clash_traffic() {
            all.up = u;
            all.down = d;
        }

        let (transient, persistent, blocked, errors) = self.errors(window);
        let (total, up, down, active, chips, active_ms) = self.servers(&host, &state);

        let iface = host_bind_iface(&host).unwrap_or_else(|| "—".to_string());
        let mode = format!("{} · {}", state.get("mode").map(String::as_str).unwrap_or("host"), iface);
        let uptime = self
            .uptime_base
            .map(|b| fmt_uptime(b + self.started.elapsed().as_secs()))
            .unwrap_or_else(|| "—".to_string());
        let router = if router_up {
            format!("running · :{}", self.proxy_port)
        } else {
            "down".to_string()
        };
        let proxy = if router_up {
            format!("on · {}", iface)
        } else {
            "off".to_string()
        };

        Snapshot {
            identity: Identity {
                mode,
                uptime,
                server_name: if active.is_empty() { "—".into() } else { active.clone() },
                server_ms: active_ms.unwrap_or(0),
                router,
                proxy,
                config: if config_ok { "host.json OK".into() } else { "host.json ERR".into() },
            },
            all,
            lanes,
            conns,
            transient,
            persistent,
            blocked,
            errors,
            servers_total: total,
            servers_up: up,
            servers_down: down,
            active_server: active,
            chips,
        }
    }
}

// ---------------- helpers ----------------

fn config_dir() -> PathBuf {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(x).join("rowt")
    } else if let Some(h) = std::env::var_os("HOME") {
        PathBuf::from(h).join(".config").join("rowt")
    } else {
        PathBuf::from(".config/rowt")
    }
}

fn env_port(key: &str, default: u16) -> u16 {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn host_bind_iface(host: &Value) -> Option<String> {
    let obs = host.get("outbounds")?.as_array()?;
    for o in obs {
        if let Some(b) = o.get("bind_interface").and_then(|b| b.as_str()) {
            return Some(b.to_string());
        }
    }
    None
}

/// Read at most `cap` bytes from the end of a file (logs can be large; we only
/// need the recent tail for any window).
fn tail_read(path: &std::path::Path, cap: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(cap);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    // Drop a possibly-partial first line when we didn't start at the beginning.
    let mut s = String::from_utf8_lossy(&buf).into_owned();
    if start > 0 {
        if let Some(nl) = s.find('\n') {
            s = s[nl + 1..].to_string();
        }
    }
    Some(s)
}

fn fmt_uptime(secs: u64) -> String {
    let d = secs / 86400;
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    if d > 0 {
        format!("{}d {}h", d, h)
    } else if h > 0 {
        format!("{}h {}m", h, m)
    } else {
        format!("{}m", m)
    }
}

/// Best-effort proxy process uptime (seconds), via `ps` on the sing-box process.
fn proxy_uptime_secs() -> Option<u64> {
    let out = std::process::Command::new("pgrep").arg("-f").arg("sing-box").output().ok()?;
    let pid = String::from_utf8_lossy(&out.stdout).lines().next()?.trim().to_string();
    if pid.is_empty() {
        return None;
    }
    let out = std::process::Command::new("ps").args(["-o", "etime=", "-p", &pid]).output().ok()?;
    parse_etime(String::from_utf8_lossy(&out.stdout).trim())
}

/// Parse `ps` etime `[[dd-]hh:]mm:ss` into seconds.
fn parse_etime(s: &str) -> Option<u64> {
    let (days, hms) = match s.split_once('-') {
        Some((d, rest)) => (d.parse::<u64>().ok()?, rest),
        None => (0, s),
    };
    let parts: Vec<u64> = hms.split(':').map(|p| p.parse().unwrap_or(0)).collect();
    let (h, m, sec) = match parts.as_slice() {
        [h, m, s] => (*h, *m, *s),
        [m, s] => (0, *m, *s),
        _ => return None,
    };
    Some(days * 86400 + h * 3600 + m * 60 + sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etime_parse() {
        assert_eq!(parse_etime("05:10"), Some(310));
        assert_eq!(parse_etime("03:15:20"), Some(3 * 3600 + 15 * 60 + 20));
        assert_eq!(parse_etime("2-03:15:20"), Some(2 * 86400 + 3 * 3600 + 15 * 60 + 20));
    }

    #[test]
    fn uptime_format() {
        assert_eq!(fmt_uptime(3 * 3600 + 15 * 60), "3h 15m");
        assert_eq!(fmt_uptime(45 * 60), "45m");
        assert_eq!(fmt_uptime(2 * 86400 + 3 * 3600), "2d 3h");
    }
}
