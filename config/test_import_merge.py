#!/usr/bin/env python3
"""Tests for import-merge.py (accumulate a client's extract into a review file,
dedup vs pool + file, tag _source, prune already-applied). Synthetic data only.

Run: python3 config/test_import_merge.py
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile

_HERE = os.path.dirname(os.path.abspath(__file__))
_SCRIPT = f"{_HERE}/import-merge.py"

FAILS: list[str] = []


def _tmp(text: str) -> str:
    f = tempfile.NamedTemporaryFile("w", suffix=".json", delete=False)
    f.write(text)
    f.close()
    return f.name


def run(into: dict | None, add: dict, source="src1", pool=None, pool_subs=None) -> dict:
    """Invoke import-merge and return the resulting accumulation dict."""
    files = []
    into_path = _tmp(json.dumps(into)) if into is not None else _tmp("")
    add_path = _tmp(json.dumps(add))
    files += [into_path, add_path]
    args = [
        sys.executable,
        _SCRIPT,
        "--into",
        into_path,
        "--add",
        add_path,
        "--source",
        source,
    ]
    for p in pool or []:
        pp = _tmp(json.dumps(p))
        files.append(pp)
        args += ["--pool", pp]
    for lines in pool_subs or []:
        sp = tempfile.NamedTemporaryFile("w", suffix=".txt", delete=False)
        sp.write("\n".join(lines) + "\n")
        sp.close()
        files.append(sp.name)
        args += ["--pool-subs", sp.name]
    subprocess.run(args, capture_output=True, text=True, check=True)
    out = json.loads(open(into_path).read())
    for f in files:
        os.unlink(f)
    return out


def srv(server, uuid, tag="t", typ="vless", port=443):
    return {
        "type": typ,
        "server": server,
        "server_port": port,
        "uuid": uuid,
        "tag": tag,
    }


def sub(url):
    return {"url": url}


def check(name, got, want):
    if got != want:
        FAILS.append(f"{name}: got {got!r}, want {want!r}")


def main() -> int:
    # 1. fresh accumulate: two servers + one sub, all tagged _source.
    r = run(
        None,
        {
            "servers": [srv("a.com", "u1"), srv("b.com", "u2")],
            "subscriptions": [sub("https://x/s")],
        },
    )
    check("fresh-servers", len(r["servers"]), 2)
    check("fresh-source", {s["_source"] for s in r["servers"]}, {"src1"})
    check("fresh-sub-source", r["subscriptions"][0]["_source"], "src1")

    # 2. re-add the same (different name) → deduped by identity, nothing added.
    r2 = run(
        r,
        {"servers": [srv("a.com", "u1", tag="A-renamed"), srv("b.com", "u2")]},
        source="src2",
    )
    check("dedup-vs-file", len(r2["servers"]), 2)

    # 3. accumulate a genuinely new server from a 2nd source → appended + tagged.
    r3 = run(r, {"servers": [srv("c.com", "u3")]}, source="src2")
    check("append-new", len(r3["servers"]), 3)
    check(
        "append-source",
        [s for s in r3["servers"] if s["server"] == "c.com"][0]["_source"],
        "src2",
    )

    # 4. dedup vs the pool: c.com already in pool → not added.
    r4 = run(
        r, {"servers": [srv("c.com", "u3")]}, source="src2", pool=[[srv("c.com", "u3")]]
    )
    check("dedup-vs-pool", [s["server"] for s in r4["servers"]], ["a.com", "b.com"])

    # 5. prune accumulation of entries now in the pool (already applied).
    r5 = run(r, {"servers": []}, source="src2", pool=[[srv("a.com", "u1")]])
    check("prune-applied", [s["server"] for s in r5["servers"]], ["b.com"])

    # 6. subscription dedup by normalized URL (a display-only name= is ignored).
    base = {"servers": [], "subscriptions": [sub("https://x/s?token=1")]}
    r6 = run(
        base,
        {"servers": [], "subscriptions": [sub("https://x/s?token=1&name=foo")]},
        source="src2",
    )
    check("sub-normalized-dedup", len(r6["subscriptions"]), 1)

    if FAILS:
        print("FAIL")
        for f in FAILS:
            print("  " + f)
        return 1
    print("ok — import-merge")
    return 0


if __name__ == "__main__":
    sys.exit(main())
