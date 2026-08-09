#!/usr/bin/env python3
"""Generate synthetic `scutil --dns` outputs for the net-detect differential gate.

Synthetic only — no real employer data. The point is COVERAGE of the shapes the
parser has to survive, not realism: a search domain before its if_index (the
ordering bug the Python's own fixture guards), sub-interfaces that must not read
as physical, nameservers just inside and just outside each internal range,
duplicate domains across resolvers, and malformed lines that must be ignored
rather than crash either side.
"""
from __future__ import annotations
import os, random, sys

DEVS = ["en0", "en1", "en12", "utun4", "utun0", "bridge0", "en0:1", "lo0", "ppp0"]
DOMS = ["corp.example", "hq.corp.example", "vpn.corp.example", "local",
        "254.169.in-addr.arpa", "8.b.d.0.1.0.0.2.ip6.arpa", "tail1234.ts.net",
        "search.tailscale", "sub.corp.example", "EXAMPLE.COM", "a.b.c.d.example"]
# Deliberately straddling every boundary in the internal-NS list.
NS = ["10.0.0.53", "9.255.255.255", "11.0.0.1", "12.0.0.1", "30.0.0.1", "31.0.0.1",
      "6.0.0.1", "6.16.0.1", "100.64.0.1", "100.128.0.1", "169.254.1.1",
      "172.15.0.1", "172.16.0.1", "172.32.0.1", "192.168.1.1", "223.5.5.5",
      "fd00:1::1", "::1", "10.0.0.256", "10.0.0", "not.an.ip"]
JUNK = ["flags    : Request A records", "reach    : 0x00000002 (Reachable)",
        "order    : 100200", "options  : mdns", "", "  ",
        "domain :", "nameserver[] : 10.0.0.1", "if_index : (en0)",
        "search domain[x] : bogus.example"]

def block(r: random.Random, n: int) -> str:
    lines = [f"resolver #{n}"]
    body = []
    # search domains BEFORE if_index in some blocks, after in others.
    for _ in range(r.randint(0, 3)):
        d = r.choice(DOMS)
        idx = f"[{r.randint(0,3)}]" if r.random() < 0.6 else ""
        kind = "search domain" if r.random() < 0.5 else "domain"
        body.append(f"  {kind}{idx} : {d}")
    for _ in range(r.randint(0, 3)):
        idx = f"[{r.randint(0,3)}]" if r.random() < 0.7 else ""
        body.append(f"  nameserver{idx} : {r.choice(NS)}")
    if r.random() < 0.8:
        body.append(f"  if_index : {r.randint(1,30)} ({r.choice(DEVS)})")
    for _ in range(r.randint(0, 2)):
        body.append(f"  {r.choice(JUNK)}")
    r.shuffle(body)
    return "\n".join(lines + body)

def main() -> int:
    out, count = sys.argv[1], int(sys.argv[2]) if len(sys.argv) > 2 else 200
    r = random.Random(20260809)   # seeded: a failing case must be reproducible
    for i in range(count):
        d = os.path.join(out, f"case{i:04d}")
        os.makedirs(d, exist_ok=True)
        blocks = [block(r, n + 1) for n in range(r.randint(1, 6))]
        with open(os.path.join(d, "scutil"), "w", encoding="utf-8") as fh:
            fh.write("DNS configuration\n\n" + "\n\n".join(blocks) + "\n")
    print(count)
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
