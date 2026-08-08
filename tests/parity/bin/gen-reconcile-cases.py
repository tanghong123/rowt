#!/usr/bin/env python3
"""Generate reconcile cases: hand-picked edges first, then randomized ones.

The reconcile is CIDR arithmetic — containment, overlap, collapsing — which is
exactly where a reimplementation goes subtly wrong on inputs nobody thought to
write down. So the differential gate feeds both implementations the same
generated cases rather than only the scenarios in the docstring.

Deterministic: seeded, so a failure is reproducible and the corpus doesn't
churn between runs.

Usage: gen-reconcile-cases.py OUTDIR [count]
"""

from __future__ import annotations

import ipaddress
import os
import random
import sys

# Ranges the corp lane actually deals with: RFC1918, CGNAT, and the
# public-looking /8s some corporate clouds route privately.
_BASES = ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "100.64.0.0/10", "11.0.0.0/8", "30.0.0.0/8"]
_PRIVATE = "10.0.0.0/8\n172.16.0.0/12\n192.168.0.0/16\n100.64.0.0/10\n169.254.0.0/16\n"

# The scenarios the docstring names, written out so a regression in any of them
# is legible rather than hiding among the random cases.
HANDPICKED = [
    # (active, handadded, block)
    ("11.122.0.0/15", "11.0.0.0/8", ""),                       # covered by a broad hand entry
    ("30.1.0.0/16", "", "12.0.0.0/8"),                         # uncovered -> rewrite
    ("30.1.0.0/16\n40.0.0.0/16", "", "30.0.0.0/8"),            # colliding block entry dropped whole
    ("10.1.0.0/16\n30.1.0.0/16", "", "10.9.0.0/16"),           # private skipped and pruned
    ("10.0.0.0/8", "10.0.0.0/9\n10.128.0.0/9", ""),            # halves collapse to cover
    ("", "", ""),                                               # nothing at all
    ("", "", "30.0.0.0/8"),                                     # stale block, no live routes
    ("30.0.0.0/8", "30.0.0.0/8", "30.0.0.0/8"),                 # identical everywhere
    ("30.1.2.3/16", "", ""),                                    # host bits set
    ("not-a-cidr\n30.1.0.0/16", "# comment\n", "\n"),           # junk tolerated
]


def rnd_net(rng: random.Random) -> str:
    base = ipaddress.ip_network(rng.choice(_BASES))
    prefix = rng.randint(base.prefixlen, min(base.prefixlen + 10, 30))
    span = int(base.broadcast_address) - int(base.network_address)
    addr = int(base.network_address) + (rng.randint(0, span) if span else 0)
    return str(ipaddress.ip_network((addr, prefix), strict=False))


def rnd_list(rng: random.Random, lo: int, hi: int) -> str:
    return "".join(f"{rnd_net(rng)}\n" for _ in range(rng.randint(lo, hi)))


def main() -> int:
    outdir = sys.argv[1]
    count = int(sys.argv[2]) if len(sys.argv) > 2 else 200
    rng = random.Random(20260809)
    os.makedirs(outdir, exist_ok=True)

    cases = [(a, h, b) for a, h, b in HANDPICKED]
    for _ in range(count):
        cases.append((rnd_list(rng, 0, 5), rnd_list(rng, 0, 3), rnd_list(rng, 0, 5)))

    for i, (a, h, b) in enumerate(cases):
        d = os.path.join(outdir, f"case{i:04d}")
        os.makedirs(d, exist_ok=True)
        for name, body in (("active", a), ("handadded", h), ("block", b), ("private", _PRIVATE)):
            with open(os.path.join(d, name), "w", encoding="utf-8") as fh:
                fh.write(body)
    print(len(cases))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
