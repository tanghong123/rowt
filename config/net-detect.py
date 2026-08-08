#!/usr/bin/env python3
"""Discover the internal/corporate DNS domains the current network advertises.

Parses `scutil --dns` (macOS) and reports the search / match domains and the
private-range nameservers that a corp LAN or a connected corp VPN registers —
the raw material for suggesting corp-lane domain suffixes. This is a SUGGESTION
aid, not an auto-apply: a machine can legitimately see many internal domains and
only the human knows which belong in the corp lane.

Only observable when the signal is live — on the corp LAN, or with the corp VPN
up. At home with the VPN down there's nothing to see (that's what a persisted
per-corp store is for).

  --input FILE   read `scutil --dns` output from FILE instead of running it
                 (for tests). Default: run `scutil --dns`.

Emits JSON on stdout:
  {
    "internal_domains":  ["corp.example.com", "hq.corp.example", ...],  # de-duped
    "physical_search":   ["hq.corp.example"],   # search domains on the primary NIC
    "corp_nameservers":  ["30.1.2.3"]           # nameservers in private/corp space
  }
Stdlib only; reads nothing but the DNS config; writes nothing.
"""

from __future__ import annotations

import argparse
import ipaddress
import json
import re
import subprocess
import sys

# Nameserver IPs that indicate an internal resolver: RFC1918 + CGNAT + link-local,
# plus the public-looking /8s that some corporate clouds route privately (11/30/6).
_INTERNAL_NS = [
    ipaddress.ip_network(c)
    for c in (
        "10.0.0.0/8",
        "172.16.0.0/12",
        "192.168.0.0/16",
        "100.64.0.0/10",
        "169.254.0.0/16",
        "11.0.0.0/8",
        "30.0.0.0/8",
        "6.0.0.0/12",
    )
]


def _is_internal_ns(ip: str) -> bool:
    try:
        addr = ipaddress.ip_address(ip)
    except ValueError:
        return False
    return addr.version == 4 and any(addr in n for n in _INTERNAL_NS)


def _skip_domain(d: str) -> bool:
    d = d.lower()
    if not d or d == "local":
        return True
    if d.endswith(".arpa"):  # reverse-DNS zones
        return True
    if d.endswith(".ts.net") or "tailscale" in d:  # Tailscale MagicDNS
        return True
    return False


def _scutil_output(path: str | None) -> str:
    if path:
        with open(path, encoding="utf-8", errors="replace") as fh:
            return fh.read()
    try:
        return subprocess.run(
            ["scutil", "--dns"], capture_output=True, text=True, check=False
        ).stdout
    except (OSError, subprocess.SubprocessError):
        return ""


def parse(text: str) -> dict:
    internal: list[str] = []
    seen: set[str] = set()
    phys_search: list[str] = []
    ns: list[str] = []
    ns_seen: set[str] = set()

    # `scutil --dns` lists a resolver's `search domain` BEFORE its `if_index`, so
    # buffer each resolver block and resolve which NIC it's scoped to at flush.
    cur_search: list[str] = []  # search-domain entries in the current block
    cur_phys = False  # current block is scoped to a physical NIC (enN)

    def flush():
        if cur_phys:
            for d in cur_search:
                if d not in phys_search:
                    phys_search.append(d)

    for raw in text.splitlines():
        line = raw.strip()
        if line.startswith("resolver #"):
            flush()
            cur_search = []
            cur_phys = False
            continue
        m = re.match(r"if_index\s*:\s*\d+\s*\(([^)]+)\)", line)
        if m:
            cur_phys = bool(re.match(r"en\d+$", m.group(1)))
            continue
        m = re.match(r"(search )?domain(?:\[\d+\])?\s*:\s*(\S+)", line)
        if m:
            is_search = bool(m.group(1))
            dom = m.group(2).lower()
            if not _skip_domain(dom):
                if dom not in seen:
                    seen.add(dom)
                    internal.append(dom)
                if is_search:
                    cur_search.append(dom)
            continue
        m = re.match(r"nameserver(?:\[\d+\])?\s*:\s*(\S+)", line)
        if m and _is_internal_ns(m.group(1)) and m.group(1) not in ns_seen:
            ns_seen.add(m.group(1))
            ns.append(m.group(1))
    flush()

    return {
        "internal_domains": internal,
        "physical_search": phys_search,
        "corp_nameservers": ns,
    }


def main() -> int:
    ap = argparse.ArgumentParser(description="detect internal/corp DNS domains")
    ap.add_argument("--input", help="read scutil --dns output from FILE (for tests)")
    args = ap.parse_args()
    result = parse(_scutil_output(args.input))
    json.dump(result, sys.stdout, indent=2, ensure_ascii=False)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
