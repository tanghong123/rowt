//! `rowt-lanes` — apply one lane-list edit, writing the files back.
//!
//!   rowt-lanes --config-dir DIR <escape|corp|block> add <entry>…
//!   rowt-lanes --config-dir DIR <escape|corp|block> rm <entry>…
//!   rowt-lanes --config-dir DIR <escape|corp|block> clear
//!   rowt-lanes --config-dir DIR <escape|corp|block> import <file>
//!   rowt-lanes --config-dir DIR <escape|corp|block> dump
//!
//! Prints the same lines the shell prints for these operations. What it
//! deliberately does not do is the shell's surrounding side effects — fetching a
//! `geosite:` rule-set, printing the "also covered by" hint, reloading the
//! router. Those are network and process work, not set logic.

use rowt_core::classify::Lane;
use rowt_core::lanes::{apply, dump, Lanes, Op};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

fn file_of(cfg: &Path, lane: Lane) -> PathBuf {
    cfg.join(match lane {
        Lane::Escape => "escape-domains.txt",
        Lane::Corp => "corp-domains.txt",
        Lane::Block => "block-domains.txt",
        Lane::Direct => "direct-domains.txt",
    })
}

fn run() -> Result<String, String> {
    let mut args = std::env::args().skip(1).peekable();
    let mut cfg: Option<PathBuf> = None;
    let mut rest: Vec<String> = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config-dir" => cfg = args.next().map(PathBuf::from),
            other => rest.push(other.to_string()),
        }
    }
    let cfg = cfg.ok_or("--config-dir is required")?;
    let lane = rest
        .first()
        .and_then(|s| Lane::parse(s))
        .filter(|l| *l != Lane::Direct)
        .ok_or("usage: rowt-lanes --config-dir DIR <escape|corp|block> <op> [args…]")?;
    let action = rest.get(1).cloned().unwrap_or_else(|| "list".into());
    let operands: Vec<String> = rest.iter().skip(2).cloned().collect();

    let lanes = Lanes {
        escape: read(&file_of(&cfg, Lane::Escape)),
        corp: read(&file_of(&cfg, Lane::Corp)),
        block: read(&file_of(&cfg, Lane::Block)),
    };

    if action == "dump" {
        let body = match lane {
            Lane::Escape => &lanes.escape,
            Lane::Corp => &lanes.corp,
            _ => &lanes.block,
        };
        return Ok(dump(body).join("\n"));
    }

    let op = match action.as_str() {
        "add" => Op::Add(operands),
        "rm" | "remove" => Op::Rm(operands),
        "clear" => Op::Clear,
        "import" => {
            let f = operands.first().ok_or("import needs a file")?;
            Op::Import(read(Path::new(f)).lines().map(|s| s.to_string()).collect())
        }
        other => return Err(format!("unknown action: {other}")),
    };

    let edit = apply(&lanes, lane, &op);
    for l in [Lane::Escape, Lane::Corp, Lane::Block] {
        let before = match l {
            Lane::Escape => &lanes.escape,
            Lane::Corp => &lanes.corp,
            _ => &lanes.block,
        };
        let after = match l {
            Lane::Escape => &edit.lanes.escape,
            Lane::Corp => &edit.lanes.corp,
            _ => &edit.lanes.block,
        };
        if before != after {
            std::fs::write(file_of(&cfg, l), after).map_err(|e| format!("write: {e}"))?;
        }
    }
    Ok(edit.messages.join("\n"))
}

fn main() -> ExitCode {
    match run() {
        Ok(s) => {
            if !s.is_empty() {
                println!("{s}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("rowt-lanes: {e}");
            ExitCode::FAILURE
        }
    }
}
