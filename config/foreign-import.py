#!/usr/bin/env python3
"""Import servers from other proxy clients into rowt's review format.

    foreign-import.py --from clash-verge|v2box|flclash [--path DIR|FILE]

Emits the same review JSON as `sr-import.py` on stdout:

    {"servers": [<sing-box outbound>...],
     "subscriptions": [{"url": "...", "name": "..."}...],
     "proxy_domains": [],
     "skipped": {"<protocol>": <count>}}

Share links are turned into outbounds by the sibling `vless-parse.py`, so only
the protocols rowt speaks — VLESS / VMess / AnyTLS / hysteria2 — are kept; other
protocols (trojan, ss, tuic, wireguard, …) are counted under "skipped".

Sources:
  clash-verge  Clash Verge Rev profiles dir (Clash YAML; needs `yq`). Remote
               profiles contribute their subscription URL; local profiles
               contribute their `proxies:` as servers.
  v2box        V2Box's Core Data DB.sqlite (share links in ZURL).
  flclash      FlClash profiles dir (Clash YAML; needs `yq`).

Credentials never touch the repo — the caller writes the review file under
~/.config/rowt/ (chmod 600).
"""

from __future__ import annotations

import argparse
import base64
import importlib.util
import json
import shutil
import sqlite3
import subprocess
import sys
from collections import Counter
from pathlib import Path
from urllib.parse import parse_qsl, quote, urlencode, urlsplit, urlunsplit

# Reuse the share-link parser (hyphenated filename → importlib).
_HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("vless_parse", _HERE / "vless-parse.py")
assert _spec and _spec.loader
vp = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(vp)

HOME = Path.home()

CLASH_VERGE_DIRS = [
    HOME / "Library/Application Support/io.github.clash-verge-rev.clash-verge-rev",
    HOME / "Library/Application Support/io.github.clash-verge-rev.clash-verge",
    HOME / "Library/Application Support/io.github.zzzgydi.clash-verge",
    HOME / ".local/share/clash-verge",
    HOME / ".config/clash-verge",
]
FLCLASH_DIRS = [
    HOME / "Library/Application Support/FlClash",
    HOME / "Library/Application Support/com.follow.clash",
    HOME / ".config/FlClash",
    HOME / ".config/flclash",
]
V2BOX_DBS = [
    HOME / "Library/Group Containers/group.hossin.asaadi.V2Box/DB.sqlite",
]


def _yq_json(path: Path) -> object:
    """Load a YAML file as JSON via `yq` (PyYAML isn't a dependency)."""
    if not shutil.which("yq"):
        raise RuntimeError(
            "`yq` is required to read Clash YAML — install it: brew install yq"
        )
    r = subprocess.run(
        ["yq", "-o=json", ".", str(path)], capture_output=True, text=True
    )
    if r.returncode != 0:
        raise RuntimeError(f"yq failed on {path.name}: {r.stderr.strip()}")
    return json.loads(r.stdout or "null")


def clash_proxy_to_link(p: dict) -> str | None:
    """A Clash/Mihomo proxy dict → a share-link URI, or None if rowt can't use it."""
    t = str(p.get("type", "")).lower()
    name = str(p.get("name", "")).strip()
    server = str(p.get("server", ""))
    port = p.get("port", "")
    frag = "#" + quote(name) if name else ""

    def q_str(q: dict) -> str:
        return urlencode({k: v for k, v in q.items() if v not in (None, "")})

    if t == "vless":
        net = str(p.get("network", "tcp")).lower()
        ro = p.get("reality-opts") or {}
        q: dict = {
            "type": net,
            "security": "reality" if ro else ("tls" if p.get("tls") else "none"),
            "sni": p.get("servername") or p.get("sni"),
            "fp": p.get("client-fingerprint"),
            "flow": p.get("flow"),
            "pbk": ro.get("public-key"),
            "sid": ro.get("short-id"),
        }
        if p.get("skip-cert-verify"):
            q["insecure"] = "1"
        if net in ("ws", "websocket"):
            ws = p.get("ws-opts") or {}
            q["path"] = ws.get("path", "/")
            hdr = ws.get("headers") or {}
            q["host"] = hdr.get("Host") or hdr.get("host")
        elif net == "grpc":
            q["serviceName"] = (p.get("grpc-opts") or {}).get("grpc-service-name")
        return (
            f"vless://{quote(str(p.get('uuid', '')))}@{server}:{port}?{q_str(q)}{frag}"
        )

    if t == "vmess":
        net = str(p.get("network", "tcp")).lower()
        j: dict = {
            "v": "2",
            "ps": name,
            "add": server,
            "port": str(port),
            "id": p.get("uuid", ""),
            "aid": str(p.get("alterId", p.get("alter-id", 0)) or 0),
            "scy": p.get("cipher", "auto") or "auto",
            "net": net,
        }
        if net in ("ws", "websocket"):
            ws = p.get("ws-opts") or {}
            j["path"] = ws.get("path", "/")
            hdr = ws.get("headers") or {}
            j["host"] = hdr.get("Host") or hdr.get("host") or ""
        elif net == "grpc":
            j["path"] = (p.get("grpc-opts") or {}).get("grpc-service-name", "")
        if p.get("tls"):
            j["tls"] = "tls"
            j["sni"] = p.get("servername") or p.get("sni") or ""
        return "vmess://" + base64.b64encode(json.dumps(j).encode()).decode()

    if t == "hysteria2":
        pw = p.get("password") or p.get("auth") or ""
        q = {"sni": p.get("sni") or p.get("servername")}
        if p.get("skip-cert-verify"):
            q["insecure"] = "1"
        if p.get("obfs"):
            q["obfs"] = p.get("obfs")
            q["obfs-password"] = p.get("obfs-password", "")
        return f"hysteria2://{quote(str(pw))}@{server}:{port}?{q_str(q)}{frag}"

    if t == "anytls":
        pw = p.get("password") or ""
        q = {"sni": p.get("sni") or p.get("servername")}
        if p.get("skip-cert-verify"):
            q["insecure"] = "1"
        return f"anytls://{quote(str(pw))}@{server}:{port}?{q_str(q)}{frag}"

    return None  # trojan / ss / ssr / tuic / wireguard / … — rowt can't use these


def _links_from_clash_proxies(proxies: list, skipped: Counter) -> list[str]:
    links: list[str] = []
    for p in proxies or []:
        if not isinstance(p, dict):
            continue
        link = clash_proxy_to_link(p)
        if link:
            links.append(link)
        else:
            skipped[str(p.get("type", "unknown")).lower()] += 1
    return links


def _first_existing(paths: list[Path]) -> Path | None:
    return next((p for p in paths if p.exists()), None)


def import_clash_dir(root: Path, skipped: Counter) -> tuple[list[str], list[dict]]:
    """A Clash Verge / FlClash profiles dir → (share links, subscription dicts)."""
    links: list[str] = []
    subs: list[dict] = []
    index = root / "profiles.yaml"
    prof_dir = root / "profiles"
    if index.exists() and prof_dir.is_dir():
        items = (_yq_json(index) or {}).get("items") or []
        for it in items:
            if not isinstance(it, dict):
                continue
            typ = str(it.get("type", "")).lower()
            if typ == "remote" and it.get("url"):
                subs.append({"url": it["url"], "name": it.get("name") or it.get("uid")})
            elif typ == "local" and it.get("file"):
                f = prof_dir / it["file"]
                if f.exists():
                    doc = _yq_json(f) or {}
                    links += _links_from_clash_proxies(
                        doc.get("proxies") or [], skipped
                    )
    else:
        # No index (FlClash-style): scan any YAML that carries a proxies: list.
        for f in sorted(root.rglob("*.y*ml")):
            try:
                doc = _yq_json(f)
            except Exception:  # noqa: BLE001
                continue
            if isinstance(doc, dict) and doc.get("proxies"):
                links += _links_from_clash_proxies(doc["proxies"], skipped)
    return links, subs


def import_v2box(db_path: Path, skipped: Counter) -> tuple[list[str], list[dict]]:
    """V2Box's Core Data store → (share links, subscription dicts)."""
    links: list[str] = []
    subs: list[dict] = []
    con = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        rows = con.execute(
            "SELECT ZTYPE, ZURL, ZSUBSCRIBE FROM ZCDV2RAYITEM"
        ).fetchall()
    finally:
        con.close()
    for ztype, zurl, zsub in rows:
        url = (zurl or "").strip()
        if url.startswith(
            (
                "vless://",
                "vmess://",
                "anytls://",
                "hysteria2://",
                "hy2://",
                "trojan://",
                "ss://",
            )
        ):
            if url.startswith(("trojan://", "ss://")):
                skipped[url.split("://", 1)[0]] += 1
            else:
                links.append(url)
        else:
            skipped[str(ztype or "unknown").lower()] += 1
        sub = (zsub or "").strip()
        if sub.startswith(("http://", "https://")):
            subs.append({"url": sub, "name": None})
    return links, subs


def load_existing_keys(paths: list[str]) -> dict[str, str]:
    """Identity key -> tag for every server already in the pool (servers.json /
    manual.json), so we can skip re-importing a server you already have."""
    known: dict[str, str] = {}
    for p in paths:
        try:
            arr = json.loads(Path(p).read_text())
        except Exception:  # noqa: BLE001
            continue
        for o in arr if isinstance(arr, list) else []:
            if isinstance(o, dict):
                known.setdefault(vp.key_of(o), o.get("tag") or "?")
    return known


def norm_sub(url: str) -> str:
    """Normalize a subscription URL for dup detection: drop a display-only `name`
    query param (clients add it) and a trailing slash, lowercase scheme/host."""
    try:
        u = urlsplit(url.strip())
    except Exception:  # noqa: BLE001
        return url.strip()
    q = [
        (k, v)
        for k, v in parse_qsl(u.query, keep_blank_values=True)
        if k.lower() != "name"
    ]
    return urlunsplit(
        (u.scheme.lower(), u.netloc.lower(), u.path.rstrip("/"), urlencode(q), "")
    )


def load_existing_subs(paths: list[str]) -> set[str]:
    """Normalized set of subscription URLs already saved (one per line in subs.txt)."""
    known: set[str] = set()
    for p in paths:
        try:
            lines = Path(p).read_text().splitlines()
        except Exception:  # noqa: BLE001
            continue
        for line in lines:
            line = line.strip()
            if line and not line.startswith("#"):
                known.add(norm_sub(line))
    return known


def build_review(
    links: list[str],
    subs: list[dict],
    skipped: Counter,
    existing: dict[str, str] | None = None,
    existing_subs: set[str] | None = None,
) -> dict:
    parsed: list[dict] = []
    if links:
        try:
            parsed = vp.parse_many(links)
        except ValueError as e:  # "no usable links" → leave servers empty
            print(f"note: {e}", file=sys.stderr)
    # Drop servers already in the pool (by identity, even when the name differs) —
    # warn, don't import — and drop within-source duplicates too.
    existing = existing or {}
    seen_keys: dict[str, str] = {}
    servers: list[dict] = []
    for o in parsed:
        k = vp.key_of(o)
        prior = existing.get(k) or seen_keys.get(k)
        if prior:
            print(
                f"note: skipping '{o.get('tag')}' — already in your pool as '{prior}' ({o.get('server')}:{o.get('server_port')})",
                file=sys.stderr,
            )
            skipped["duplicate"] += 1
            continue
        seen_keys[k] = o.get("tag") or "?"
        servers.append(o)
    # Subscriptions: drop ones already saved (matched by normalized URL, ignoring a
    # display-only name= param) — warn, don't re-add — plus within-source dups.
    existing_subs = existing_subs or set()
    seen_subs: set[str] = set()
    uniq_subs: list[dict] = []
    for s in subs:
        n = norm_sub(s["url"])
        if n in existing_subs:
            print(
                f"note: subscription already saved — skipping {s['url']}",
                file=sys.stderr,
            )
            skipped["duplicate-subscription"] += 1
            continue
        if n in seen_subs:
            continue
        seen_subs.add(n)
        uniq_subs.append(s)
    return {
        "servers": servers,
        "subscriptions": uniq_subs,
        "proxy_domains": [],
        "skipped": dict(skipped),
    }


def detect() -> list[str]:
    """Which foreign clients have importable data on this machine (fast path checks)."""
    found: list[str] = []
    cv = _first_existing(CLASH_VERGE_DIRS)
    if cv and ((cv / "profiles.yaml").exists() or (cv / "profiles").is_dir()):
        found.append("clash-verge")
    if _first_existing(V2BOX_DBS):
        found.append("v2box")
    fc = _first_existing(FLCLASH_DIRS)
    if fc and next(fc.rglob("*.y*ml"), None):
        found.append("flclash")
    return found


def main() -> int:
    ap = argparse.ArgumentParser(description="import servers from another proxy client")
    ap.add_argument(
        "--from",
        dest="source",
        choices=["clash-verge", "v2box", "flclash"],
    )
    ap.add_argument(
        "--detect",
        action="store_true",
        help="print which of clash-verge/v2box/flclash have importable data",
    )
    ap.add_argument("--path", help="override the source location (dir or DB file)")
    ap.add_argument(
        "--existing",
        action="append",
        default=[],
        metavar="FILE",
        help="an existing pool (servers.json / manual.json); its servers are skipped as duplicates",
    )
    ap.add_argument(
        "--existing-subs",
        action="append",
        default=[],
        metavar="FILE",
        help="an existing subs list (subs.txt); URLs already saved are skipped",
    )
    args = ap.parse_args()

    if args.detect:
        for name in detect():
            print(name)
        return 0
    if not args.source:
        ap.error(
            "--from is required (one of clash-verge/v2box/flclash), or use --detect"
        )

    skipped: Counter = Counter()
    try:
        if args.source in ("clash-verge", "flclash"):
            cands = (
                [Path(args.path)]
                if args.path
                else (
                    CLASH_VERGE_DIRS if args.source == "clash-verge" else FLCLASH_DIRS
                )
            )
            root = _first_existing(cands)
            if not root:
                print(
                    f"error: no {args.source} data found (looked in: {', '.join(str(p) for p in cands)})",
                    file=sys.stderr,
                )
                return 1
            print(f"reading {args.source} from {root}", file=sys.stderr)
            links, subs = import_clash_dir(root, skipped)
        else:  # v2box
            cands = [Path(args.path)] if args.path else V2BOX_DBS
            db = _first_existing(cands)
            if not db:
                print(
                    f"error: no V2Box DB found (looked in: {', '.join(str(p) for p in cands)})",
                    file=sys.stderr,
                )
                return 1
            print(f"reading v2box from {db}", file=sys.stderr)
            links, subs = import_v2box(db, skipped)
    except RuntimeError as e:
        print(f"error: {e}", file=sys.stderr)
        return 1

    review = build_review(
        links,
        subs,
        skipped,
        load_existing_keys(args.existing),
        load_existing_subs(args.existing_subs),
    )
    if not review["servers"] and not review["subscriptions"]:
        print(
            "note: nothing new to import (already in your pool / subscriptions)",
            file=sys.stderr,
        )
    json.dump(review, sys.stdout, indent=2)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
