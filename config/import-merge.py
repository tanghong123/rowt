#!/usr/bin/env python3
"""Merge one VPN client's extracted servers/subscriptions into an ACCUMULATION
review file, so `rowt server import --output <file> --from <src>` can be run once
per client to build a single review across several clients — without duplicates.

For each server/subscription in the new extract it is KEPT only if it isn't:
  * already in the pool (servers.json / manual.json / subs.txt), or
  * already accumulated in <file>.
Every kept entry gets a `_source` attribute (the client name) so the human editing
the file — and rowt at apply time — knows where it came from (rowt strips
`_source` before it reaches sing-box). The accumulation is also PRUNED of entries
that are now in the pool (i.e. already applied), so re-running stays clean.

  --into FILE       accumulation review file (created if missing; '-' = stdout)
  --add FILE        one client's raw extract (importer JSON; '-' = stdin)
  --source NAME     tag kept entries with this source (e.g. shadowrocket)
  --pool FILE       existing pool servers (servers.json/manual.json); repeatable
  --pool-subs FILE  existing subs list (subs.txt); repeatable

Servers are identified by endpoint identity (sibling vless-parse.py `key_of`),
subscriptions by normalized URL. Stdlib only (+ vless-parse.py). Summary -> stderr.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import sys
from pathlib import Path
from urllib.parse import parse_qsl, urlencode, urlsplit, urlunsplit

_HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("vless_parse", _HERE / "vless-parse.py")
vp = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(vp)


def norm_sub(url: str) -> str:
    """Normalize a subscription URL for dup detection: drop a display-only `name`
    query param and a trailing slash, lowercase scheme/host (matches the importers)."""
    try:
        u = urlsplit(url.strip())
    except ValueError:
        return url.strip()
    q = [
        (k, v)
        for k, v in parse_qsl(u.query, keep_blank_values=True)
        if k.lower() != "name"
    ]
    return urlunsplit(
        (u.scheme.lower(), u.netloc.lower(), u.path.rstrip("/"), urlencode(q), "")
    )


def _load(path: str):
    try:
        return json.loads(Path(path).read_text())
    except (OSError, ValueError):
        return None


def _sub_url(s) -> str:
    return s.get("url", "") if isinstance(s, dict) else ""


def main() -> int:
    ap = argparse.ArgumentParser(
        description="accumulate one client's extract into a review file"
    )
    ap.add_argument("--into", required=True)
    ap.add_argument("--add", required=True)
    ap.add_argument("--source", default="")
    ap.add_argument("--pool", action="append", default=[])
    ap.add_argument("--pool-subs", action="append", default=[])
    args = ap.parse_args()

    add = json.load(sys.stdin) if args.add == "-" else _load(args.add)
    if not isinstance(add, dict):
        print("error: --add is not a review JSON object", file=sys.stderr)
        return 1

    acc = None if args.into == "-" else _load(args.into)
    if not isinstance(acc, dict):
        acc = {}
    for k in ("servers", "subscriptions", "proxy_domains"):
        if not isinstance(acc.get(k), list):
            acc[k] = []
    if not isinstance(acc.get("skipped"), dict):
        acc["skipped"] = {}

    # Pool identity: servers by key, subscriptions by normalized URL.
    pool_keys: set[str] = set()
    for p in args.pool:
        arr = _load(p)
        for o in arr if isinstance(arr, list) else []:
            if isinstance(o, dict):
                pool_keys.add(vp.key_of(o))
    pool_subs: set[str] = set()
    for p in args.pool_subs:
        try:
            for line in Path(p).read_text().splitlines():
                line = line.strip()
                if line and not line.startswith("#"):
                    pool_subs.add(norm_sub(line))
        except OSError:
            pass

    # Prune the accumulation of entries that are already in the pool (applied).
    acc["servers"] = [
        o
        for o in acc["servers"]
        if isinstance(o, dict) and vp.key_of(o) not in pool_keys
    ]
    acc["subscriptions"] = [
        s
        for s in acc["subscriptions"]
        if _sub_url(s) and norm_sub(_sub_url(s)) not in pool_subs
    ]

    have_keys = pool_keys | {vp.key_of(o) for o in acc["servers"]}
    have_subs = pool_subs | {norm_sub(_sub_url(s)) for s in acc["subscriptions"]}

    added_s = dup_s = added_u = dup_u = 0
    for o in add.get("servers") or []:
        if not isinstance(o, dict):
            continue
        k = vp.key_of(o)
        if k in have_keys:
            dup_s += 1
            continue
        have_keys.add(k)
        acc["servers"].append({**o, "_source": args.source} if args.source else o)
        added_s += 1
    for s in add.get("subscriptions") or []:
        url = _sub_url(s)
        if not url:
            continue
        n = norm_sub(url)
        if n in have_subs:
            dup_u += 1
            continue
        have_subs.add(n)
        acc["subscriptions"].append({**s, "_source": args.source} if args.source else s)
        added_u += 1
    have_dom = set(acc["proxy_domains"])
    for d in add.get("proxy_domains") or []:
        if d not in have_dom:
            have_dom.add(d)
            acc["proxy_domains"].append(d)
    for kk, vv in (add.get("skipped") or {}).items():
        try:
            acc["skipped"][kk] = int(acc["skipped"].get(kk, 0)) + int(vv)
        except (TypeError, ValueError):
            pass

    text = json.dumps(acc, indent=2, ensure_ascii=False) + "\n"
    if args.into == "-":
        sys.stdout.write(text)
    else:
        Path(args.into).write_text(text)

    src = args.source or "source"
    msg = f"{src}: +{added_s} server(s), +{added_u} subscription(s)"
    if dup_s or dup_u:
        msg += f"  (skipped {dup_s} dup server(s), {dup_u} dup sub(s))"
    print(msg, file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
