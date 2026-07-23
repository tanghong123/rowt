#!/usr/bin/env python3
"""Reconcile the corp lane's auto-synced CIDR block toward a *superset* of the
live tunnel routes, with the fewest possible rewrites (each rewrite costs a
sing-box reload).

Three inputs, one CIDR per line (IPv4; blanks/comments ignored):

  --active      A: CIDRs routed by the currently-UP tunnels (netstat, now)
  --handadded   H: CIDRs the user typed into corp-domains.txt by hand
  --block       B: CIDRs currently in the auto-managed sync block

The corp lane only has to CONTAIN every live route (a superset is fine — an
over-broad hand-added 11.0.0.0/8 already covers a live 11.122.0.0/15). So:

  * If every a in A is covered by H ∪ B (CIDR containment) -> print NOCHANGE.
    Nothing is rewritten, nothing reloads. Stale CIDRs kept from a now-down
    tunnel stay put (still needed in-office), even if they overlap nothing.

  * Otherwise a live route is uncovered, so rewrite to the minimal new block:
        B' = { c in B : c is disjoint from A }        # keep non-colliding stale
             ∪ { a in A : a not covered by H }         # add what H doesn't cover
    A block CIDR that COLLIDES with (overlaps) any live route is dropped WHOLE
    (never shrunk). Hand-added CIDRs (H) are never touched — they live outside
    the managed block. Live routes already covered by H aren't re-added (minimal).

stdout: first line CHANGE or NOCHANGE; on CHANGE, the sorted B' CIDRs follow.
Stdlib only; nothing is written to disk here.
"""

from __future__ import annotations

import argparse
import ipaddress


def _load(path: str | None) -> list[ipaddress.IPv4Network]:
    if not path:
        return []
    out: list[ipaddress.IPv4Network] = []
    seen: set[str] = set()
    with open(path, encoding="utf-8", errors="replace") as fh:
        for raw in fh:
            line = raw.strip()
            if not line or line.startswith("#"):
                continue
            try:
                net = ipaddress.ip_network(line, strict=False)
            except ValueError:
                continue
            if net.version != 4:
                continue
            key = str(net)
            if key not in seen:
                seen.add(key)
                out.append(net)
    return out


def _covered_by(a: ipaddress.IPv4Network, pool) -> bool:
    """True if a is entirely contained in the union of `pool` (collapsed)."""
    return any(a.subnet_of(n) for n in pool)


def main() -> int:
    ap = argparse.ArgumentParser(description="corp-lane superset reconcile")
    ap.add_argument("--active", help="live tunnel routes (A)")
    ap.add_argument("--handadded", help="hand-typed corp CIDRs (H)")
    ap.add_argument("--block", help="current auto-managed block CIDRs (B)")
    ap.add_argument(
        "--private",
        help="private/overlay ranges the router already defaults to unbound (P): "
        "routes inside them are never mirrored, and are pruned from the block",
    )
    args = ap.parse_args()

    A = _load(args.active)
    H = _load(args.handadded)
    B = _load(args.block)
    P = list(ipaddress.collapse_addresses(_load(args.private)))

    # Ranges the router already sends unbound by default (RFC1918/CGNAT/…) don't
    # belong in the sync block: skip such live routes, and drop them from B below.
    A = [a for a in A if not _covered_by(a, P)]
    B = [c for c in B if not _covered_by(c, P)]

    cover = list(ipaddress.collapse_addresses(H + B))
    if all(_covered_by(a, cover) for a in A):
        print("NOCHANGE")
        return 0

    hcover = list(ipaddress.collapse_addresses(H)) if H else []
    keep = [c for c in B if not any(c.overlaps(a) for a in A)]
    add = [a for a in A if not _covered_by(a, hcover)]

    merged: dict[str, ipaddress.IPv4Network] = {}
    for net in keep + add:
        merged[str(net)] = net
    result = sorted(
        merged.values(), key=lambda n: (int(n.network_address), n.prefixlen)
    )

    print("CHANGE")
    for net in result:
        print(str(net))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
