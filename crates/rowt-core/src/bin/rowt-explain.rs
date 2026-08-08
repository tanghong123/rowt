//! `rowt-explain` — prints which lane a destination takes, and why.
//!
//!   rowt-explain --config-dir DIR <dest>
//!   rowt-explain --config-dir DIR --corpus FILE     # one verdict per line, TSV
//!
//! Output matches the first two lines of `rowt explain`. The optional note and
//! live-probe lines the shell adds are not reproduced: the first depends on
//! which rule-sets happen to be cached, the second dials the network.
//!
//! No DNS resolution happens here — that is a platform call. Pass `--ip` when
//! the caller already has an answer.

use rowt_core::classify::{classify, ClassifyInput, Lane};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string())
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

fn sget(state: &str, key: &str) -> String {
    state
        .lines()
        .filter_map(|l| l.strip_prefix(&format!("{key}=")))
        .next_back()
        .unwrap_or("")
        .to_string()
}

fn run() -> Result<String, String> {
    let mut args = std::env::args().skip(1);
    let (mut cfg, mut dest, mut corpus, mut ip) = (None::<PathBuf>, None::<String>, None::<PathBuf>, String::new());
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config-dir" => cfg = args.next().map(PathBuf::from),
            "--corpus" => corpus = args.next().map(PathBuf::from),
            "--ip" => ip = args.next().unwrap_or_default(),
            other if other.starts_with("--") => return Err(format!("unknown flag: {other}")),
            other => dest = Some(other.to_string()),
        }
    }
    let cfg = cfg.ok_or("--config-dir is required")?;

    let escape = read(&cfg.join("escape-domains.txt"));
    let corp = read(&cfg.join("corp-domains.txt"));
    let block = read(&cfg.join("block-domains.txt"));
    let state = read(&cfg.join("state"));
    let mode = {
        let m = sget(&state, "mode");
        if m.is_empty() { "host".into() } else { m }
    };
    let private_cidrs: Vec<String> =
        ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "100.64.0.0/10", "169.254.0.0/16"]
            .iter()
            .map(|s| s.to_string())
            .collect();
    let final_route = Lane::parse(&env_or("ROWT_FINAL", "direct")).ok_or("bad ROWT_FINAL")?;
    let private_default = env_or("ROWT_PRIVATE_DEFAULT", "corp");

    let input = ClassifyInput {
        escape_list: &escape,
        corp_list: &corp,
        block_list: &block,
        private_cidrs: &private_cidrs,
        private_default: &private_default,
        final_route,
        local_mode: mode == "local",
        resolved_ip: &ip,
    };

    if let Some(path) = corpus {
        // TSV mirroring `parity corpus`: destination, lane, reason.
        let body = read(&path);
        let mut out = String::new();
        for line in body.lines() {
            let d = line.split('\t').next().unwrap_or("");
            if d.is_empty() {
                continue;
            }
            let c = classify(d, &input);
            out.push_str(&format!("{}\t{}\t{}\n", d, c.lane.upper(), c.why));
        }
        return Ok(out.trim_end_matches('\n').to_string());
    }

    let dest = dest.ok_or("usage: rowt-explain --config-dir DIR <dest>")?;
    Ok(classify(&dest, &input).render())
}

fn main() -> ExitCode {
    match run() {
        Ok(s) => {
            println!("{s}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("rowt-explain: {e}");
            ExitCode::FAILURE
        }
    }
}
