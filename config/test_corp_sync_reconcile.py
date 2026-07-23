#!/usr/bin/env python3
"""Tests for corp-sync-reconcile.py (superset / minimal-reload CIDR reconcile).

Run: python3 config/test_corp_sync_reconcile.py
"""

from __future__ import annotations

import importlib.util
import os
import subprocess
import sys
import tempfile

_here = os.path.dirname(os.path.abspath(__file__))
_script = f"{_here}/corp-sync-reconcile.py"

# import to keep a module ref (ensures it stays importable / py_compile-clean)
_spec = importlib.util.spec_from_file_location("reconcile", _script)
_mod = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_mod)

FAILS: list[str] = []


def run(active, handadded, block, private=()):
    """Invoke the reconcile CLI; return (status, [cidrs])."""
    files = []

    def mk(lines):
        f = tempfile.NamedTemporaryFile("w", suffix=".txt", delete=False)
        f.write("\n".join(lines) + ("\n" if lines else ""))
        f.close()
        files.append(f.name)
        return f.name

    args = [
        sys.executable,
        _script,
        "--active",
        mk(active),
        "--handadded",
        mk(handadded),
        "--block",
        mk(block),
        "--private",
        mk(list(private)),
    ]
    out = subprocess.run(args, capture_output=True, text=True, check=True).stdout
    for fn in files:
        os.unlink(fn)
    lines = out.strip().splitlines()
    return (lines[0] if lines else ""), lines[1:]


def check(name, got, want):
    if got != want:
        FAILS.append(f"{name}: got {got!r}, want {want!r}")


PRIV = [
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "100.64.0.0/10",
    "169.254.0.0/16",
]


def main() -> int:
    # 1. active covered by a broad hand-added CIDR -> no reload.
    s, _ = run(["11.122.0.0/15"], ["11.0.0.0/8"], [])
    check("broad-cover", s, "NOCHANGE")

    # 2. reconnect whose routes are already persisted in the block -> no reload.
    s, _ = run(["30.100.0.0/16"], [], ["30.100.0.0/16", "6.0.0.0/12"])
    check("persisted", s, "NOCHANGE")

    # 3. a new uncovered live route -> add it, keep disjoint stale.
    s, body = run(["47.88.0.0/16"], [], ["6.0.0.0/12"])
    check("add-new", (s, body), ("CHANGE", ["6.0.0.0/12", "47.88.0.0/16"]))

    # 4. collision: stale 30.0.0.0/9 overlaps a live route -> dropped WHOLE.
    s, body = run(["30.100.0.0/16", "30.200.0.0/16"], [], ["30.0.0.0/9", "6.0.0.0/12"])
    check(
        "collision-whole-drop",
        (s, body),
        ("CHANGE", ["6.0.0.0/12", "30.100.0.0/16", "30.200.0.0/16"]),
    )

    # 5. all tunnels down (no active) -> persist, no reload.
    s, _ = run([], [], ["30.0.0.0/8", "6.0.0.0/12"])
    check("all-down-persist", s, "NOCHANGE")

    # 6. private-range live routes are never mirrored; private cruft pruned.
    s, body = run(
        ["10.5.0.0/16", "47.88.0.0/16", "100.64.75.0/24"],
        [],
        ["192.168.9.0/24", "30.1.0.0/16"],
        PRIV,
    )
    check("private-filter", (s, body), ("CHANGE", ["30.1.0.0/16", "47.88.0.0/16"]))

    # 7. only private live routes, no public -> nothing to mirror.
    s, _ = run(["10.5.0.0/16", "192.168.1.0/24"], [], [], PRIV)
    check("all-private", s, "NOCHANGE")

    if FAILS:
        print("FAIL")
        for f in FAILS:
            print("  " + f)
        return 1
    print("ok — corp-sync-reconcile")
    return 0


if __name__ == "__main__":
    sys.exit(main())
