#!/usr/bin/env python3
"""Generate the classifier corpus from lane list files (PORTING.md §6.1).

The lane logs are NOT an oracle — they are error logs, so the domains in them
are whatever happened to fail. This generator produces the opposite: an
exhaustive, boring set of destinations derived from the rules themselves, where
every entry has a known-correct answer by construction.

For each domain suffix it emits the exact match plus the near-misses that a
suffix matcher gets wrong when it uses the wrong comparison:

    example.com          exact          -> must match
    sub.example.com      subdomain      -> must match
    xexample.com         no dot boundary-> must NOT match
    example.com.evil     suffix reused  -> must NOT match
    example.co           truncated      -> must NOT match

For each CIDR it emits the boundary addresses where an off-by-one lives:
network-1, network, network+1, broadcast-1, broadcast, broadcast+1. It also
emits the PRIVATE_CIDRS edges, which decide the corp fall-through.

Usage:
    gen-corpus.py [--lists DIR] [--out FILE]

Defaults to the synthetic fixture lists. Pointing --lists at a real config dir
is a LOCAL-ONLY operation: the output would carry employer-internal names and
must never be committed.
"""

from __future__ import annotations

import argparse
import ipaddress
import os
import sys

# The ranges bin/rowt treats as corp by fall-through (DESIGN.md §3).
PRIVATE_CIDRS = ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "100.64.0.0/10", "169.254.0.0/16"]

LANES = {
    "escape-domains.txt": "escape",
    "corp-domains.txt": "corp",
    "block-domains.txt": "block",
}


def read_list(path: str) -> list[str]:
    if not os.path.exists(path):
        return []
    out = []
    for raw in open(path, encoding="utf-8", errors="replace"):
        line = raw.strip()
        if not line or line.startswith("#") or line.startswith("geosite:"):
            continue
        out.append(line)
    return out


def domain_cases(suffix: str, lane: str) -> list[tuple[str, str]]:
    """(destination, why) pairs around one suffix."""
    head, _, rest = suffix.partition(".")
    truncated = suffix[:-1] if len(suffix) > 1 else suffix
    return [
        (suffix, f"{lane}: exact suffix"),
        (f"sub.{suffix}", f"{lane}: subdomain of the suffix"),
        (f"deep.sub.{suffix}", f"{lane}: deep subdomain"),
        (f"x{suffix}", "no match: no dot boundary before the suffix"),
        (f"{suffix}.example", "no match: suffix appears as a prefix"),
        (truncated, "no match: truncated suffix"),
        (f"{head}.example" if rest else f"{head}x.example", "no match: first label only"),
    ]


def in_private(addr: str) -> bool:
    ip = ipaddress.ip_address(addr)
    return any(ip in ipaddress.ip_network(c) for c in PRIVATE_CIDRS)


def cidr_cases(cidr: str, lane: str) -> list[tuple[str, str]]:
    try:
        net = ipaddress.ip_network(cidr, strict=False)
    except ValueError:
        return [(cidr, f"{lane}: unparseable CIDR (kept — bash may still accept it)")]
    first, last = int(net.network_address), int(net.broadcast_address)
    v4 = ipaddress.IPv4Address
    cases = [
        (str(v4(first - 1)), f"no match: one below {cidr}"),
        (str(v4(first)), f"{lane}: network address of {cidr}"),
        (str(v4(first + 1)), f"{lane}: first host of {cidr}"),
        (str(v4(last - 1)), f"{lane}: last host of {cidr}"),
        (str(v4(last)), f"{lane}: broadcast address of {cidr}"),
        (str(v4(last + 1)), f"no match: one above {cidr}"),
    ]
    # An address just outside a corp CIDR can still land in corp via the
    # RFC1918 fall-through — say so, or the case reads like a false positive.
    return [
        (a, w + " (still corp: inside the private fall-through)")
        if w.startswith("no match") and in_private(a)
        else (a, w)
        for a, w in cases
        if a
    ]


def main() -> int:
    ap = argparse.ArgumentParser()
    here = os.path.dirname(os.path.abspath(__file__))
    ap.add_argument("--lists", default=os.path.join(here, "..", "fixtures", "config"))
    ap.add_argument("--out", default="-")
    args = ap.parse_args()

    rows: list[tuple[str, str]] = []
    for fname, lane in LANES.items():
        for entry in read_list(os.path.join(args.lists, fname)):
            if "/" in entry:
                rows.extend(cidr_cases(entry, lane))
            else:
                rows.extend(domain_cases(entry, lane))

    for cidr in PRIVATE_CIDRS:
        rows.extend(cidr_cases(cidr, "corp (private fall-through)"))

    # Controls: nothing in any list should claim these.
    rows.extend(
        [
            ("unlisted.example", "no match: not in any list"),
            ("203.0.113.7", "no match: public address in no CIDR"),
            ("xn--80ak6aa92e.example", "no match: IDN punycode, not listed"),
        ]
    )

    seen, lines = set(), []
    for dest, why in rows:
        if dest in seen:
            continue
        seen.add(dest)
        lines.append(f"{dest}\t{why}")

    body = "\n".join(lines) + "\n"
    if args.out == "-":
        sys.stdout.write(body)
    else:
        with open(args.out, "w", encoding="utf-8") as fh:
            fh.write(body)
        print(f"wrote {len(lines)} cases to {args.out}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
