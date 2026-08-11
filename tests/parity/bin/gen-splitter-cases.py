#!/usr/bin/env python3
"""Generate sing-box log streams for `parity splitter-diff`.

The splitter is the one ported piece that cannot be gated through a command.
It is a daemon `up` starts inside a process substitution, it outlives the CLI,
and the sandbox's fake sing-box exits without printing anything — so no cli-diff
case has ever caused it to write a single line. Feeding both implementations the
same stream directly is deterministic where catching them through `up` would be
a race: each runs to EOF and then exits, and what they wrote is on disk.

The stream matters more than it looks. Every line sing-box emits goes through
here on its way to host.log, so a splitter that mangles, drops or duplicates a
line corrupts the record `rowt <lane> errors` reads — and the fail-safe (write
the raw line through) means the damage is silent.

Line shapes are crossed deliberately around the regex's edges: the tag decides
the lane, the port must be digits, the timestamp is optional, ANSI has to come
off first, and anything that does not match must survive VERBATIM.

Usage: gen-splitter-cases.py <workdir> [count]   -> prints the case count
"""

from __future__ import annotations

import random
import sys
from pathlib import Path

TAGS = ["block", "direct", "corp", "escape", "auto", "vps-jp", "", "a]b", "sel[x"]
DOMAINS = [
    "example.com", "a.example.com", "192.0.2.1", "[2001:db8::1]",
    "xn--fiq228c.example", "中文.example", "host with space",
    "trailing.example.", "", "a:b.example",
]
PORTS = ["443", "80", "0", "65535", "abc", "", "44a3", "443 "]
TYPES = ["direct", "vless", "shadowsocks", "block", "", "http_2"]
REASONS = [
    "connection refused",
    "dial tcp: lookup failed",
    "context canceled",
    "rejected by rule",
    "",
    "reason with\ttab",
]

# Lines the regex must NOT match, which therefore have to reach host.log byte
# for byte. The last two are near-misses on purpose.
NOISE = [
    "INFO[0001] router started",
    "+0800 2026-08-11 09:00:00 DEBUG something happened",
    "",
    "   ",
    "open connection to example.com using outbound/direct[escape]: no port",
    "open connection to example.com:443 using outbound[escape]: no slash-type",
    "some prefix open connection to but nothing after",
    "中文 log line",
]

TS = ["2026-08-11 09:00:00", "2026-01-02 23:59:59", None]


def conn_line(r: random.Random) -> str:
    dom, port = r.choice(DOMAINS), r.choice(PORTS)
    typ, tag, reason = r.choice(TYPES), r.choice(TAGS), r.choice(REASONS)
    ts = r.choice(TS)
    head = f"{ts} ERROR " if ts else "ERROR "
    line = f"{head}open connection to {dom}:{port} using outbound/{typ}[{tag}]: {reason}"
    if r.random() < 0.25:
        # sing-box colours its level when stdout is a tty, and something in the
        # chain can keep the escapes. They come off before matching, and the
        # line written to host.log keeps them.
        line = f"\x1b[31m{line}\x1b[0m"
    return line


def main() -> int:
    work = Path(sys.argv[1])
    count = int(sys.argv[2]) if len(sys.argv) > 2 else 120
    rng = random.Random(0x511772)
    work.mkdir(parents=True, exist_ok=True)

    n = 0
    for i in range(count):
        d = work / f"case{i:04d}"
        d.mkdir(parents=True, exist_ok=True)
        lines = [
            conn_line(rng) if rng.random() < 0.65 else rng.choice(NOISE)
            for _ in range(rng.randint(0, 14))
        ]
        body = "\n".join(lines)
        # The three stream endings, because they are three different questions:
        # a normal EOF, a last line with NO newline (Python writes it without
        # one, and anything that adds one has changed the file), and CRLF, which
        # Python keeps as part of the line and Rust's `.lines()` strips.
        roll = rng.random()
        if roll < 0.7 and lines:
            body += "\n"
        elif roll < 0.85:
            body = "\r\n".join(lines) + ("\r\n" if lines else "")
        (d / "stdin").write_text(body, encoding="utf-8")
        n += 1

    print(n)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
