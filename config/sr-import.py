#!/usr/bin/env python3
"""Extract servers, subscriptions and proxy rules from a Shadowrocket install.

Reads Shadowrocket's binary server store (NSKeyedArchiver plist) and its
Surge-style rule config, and emits a JSON summary on stdout:

    {
      "subscriptions": [{"url": "...", "info": "...", "title": "..."}],
      "servers":       [ <sing-box VLESS outbound>, ... ],   # VLESS only
      "proxy_domains": ["google.com", ...],                   # [Rule] ...,PROXY
      "skipped":       {"AnyTLS": 19, "Shadowsocks": 44}      # not importable
    }

Nothing is written and no credentials are printed to stderr. Stdlib only.
"""

from __future__ import annotations

import argparse
import glob
import json
import os
import plistlib
import re
import sys
from collections import Counter
from plistlib import UID

HOME = os.path.expanduser("~")
GC = f"{HOME}/Library/Group Containers/group.com.liguangming.Shadowrocket"
ICLOUD = (
    f"{HOME}/Library/Mobile Documents/iCloud~com~liguangming~Shadowrocket/Documents"
)

STORE_CANDIDATES = [f"{GC}/ServerManager", f"{ICLOUD}/shadowrocket.v2.model"]
CONF_GLOBS = [
    f"{HOME}/Library/Mobile Documents/com~apple~CloudDocs/**/default.conf",
    f"{ICLOUD}/**/*.conf",
    f"{GC}/*.conf",
]
RESERVED = {"escape", "auto", "direct", "corp", "block", "in", "local", "dns-out"}


def _find(paths: list[str]) -> str | None:
    for p in paths:
        if os.path.isfile(p):
            return p
    return None


def _find_conf() -> str | None:
    for g in CONF_GLOBS:
        hits = [h for h in glob.glob(g, recursive=True) if os.path.isfile(h)]
        if hits:
            return max(hits, key=os.path.getmtime)
    return None


def _sanitize(name: str, index: int, used: set[str]) -> str:
    base = (
        re.sub(r"[^A-Za-z0-9._-]+", "-", (name or "").strip()).strip("-")
        or f"sr-{index}"
    )
    if base in RESERVED:
        base = f"{base}-{index}"
    tag, n = base, 2
    while tag in used:
        tag = f"{base}-{n}"
        n += 1
    used.add(tag)
    return tag


def _load_servers(store: str) -> list[dict]:
    objs = plistlib.load(open(store, "rb"))["$objects"]

    def d(x):  # dereference a CF$UID, unwrapping NSString/NSMutableString objects
        v = objs[x.data] if isinstance(x, UID) else x
        if isinstance(v, dict) and "NS.string" in v:  # NSString wrapper -> inner str
            return v["NS.string"]
        return v

    out = []
    for o in objs:
        if isinstance(o, dict) and "type" in o and "host" in o:
            out.append({k: d(v) for k, v in o.items()})
    return out


def _to_vless(s: dict, tag: str) -> dict | None:
    host, port = s.get("host"), s.get("port")
    # Shadowrocket stores the real VLESS id in its generic `password` field (the
    # same slot AnyTLS/SS use); the `uuid` field is Shadowrocket's own internal
    # config id, NOT the VLESS user id. Prefer password, fall back to uuid.
    vid = s.get("password") or s.get("uuid")
    if not host or not vid:
        return None
    out: dict = {
        "type": "vless",
        "tag": tag,
        "server": str(host),
        "server_port": int(port or 443),
        "uuid": str(vid),
    }
    if str(s.get("xtls")) == "2":
        out["flow"] = "xtls-rprx-vision"
    sni = str(s.get("peer") or host)
    pbk = s.get("publicKey")
    if pbk:  # reality
        tls = {
            "enabled": True,
            "server_name": sni,
            "utls": {"enabled": True, "fingerprint": "chrome"},
            "reality": {
                "enabled": True,
                "public_key": str(pbk),
                "short_id": str(s.get("shortId") or ""),
            },
        }
        out["tls"] = tls
    elif s.get("tls"):
        out["tls"] = {
            "enabled": True,
            "server_name": sni,
            "utls": {"enabled": True, "fingerprint": "chrome"},
        }
    obfs = str(s.get("obfs") or "none").lower()
    if obfs in ("ws", "websocket"):
        out["transport"] = {
            "type": "ws",
            "path": str(s.get("obfsParam") or s.get("pluginParam") or "/"),
        }
    elif obfs == "grpc":
        out["transport"] = {
            "type": "grpc",
            "service_name": str(s.get("obfsParam") or ""),
        }
    return out


def _to_anytls(s: dict, tag: str) -> dict | None:
    host, port, pw = s.get("host"), s.get("port"), s.get("password")
    if not host or not pw:
        return None
    sni = str(s.get("peer") or host)
    return {
        "type": "anytls",
        "tag": tag,
        "server": str(host),
        "server_port": int(port or 443),
        "password": str(pw),
        "tls": {
            "enabled": True,
            "server_name": sni,
            "insecure": bool(s.get("allowInsecure")),
            "utls": {"enabled": True, "fingerprint": "chrome"},
        },
    }


def _parse_rules(conf: str) -> list[str]:
    doms: list[str] = []
    in_rules = False
    for raw in open(conf, encoding="utf-8", errors="replace"):
        line = raw.strip()
        if line.startswith("["):
            in_rules = line.lower().startswith("[rule]")
            continue
        if not in_rules or not line or line.startswith("#"):
            continue
        parts = [p.strip() for p in line.split(",")]
        if len(parts) < 3:
            continue
        typ, value, action = parts[0].upper(), parts[1], parts[-1].upper()
        if action == "PROXY" and typ in ("DOMAIN-SUFFIX", "DOMAIN"):
            doms.append(value.lower())
    # dedupe, keep order
    seen: set[str] = set()
    return [x for x in doms if not (x in seen or seen.add(x))]


def main() -> int:
    ap = argparse.ArgumentParser(description="Shadowrocket -> rowt importer")
    ap.add_argument("--store", help="ServerManager / v2.model plist (auto-detected)")
    ap.add_argument("--conf", help="default.conf rule file (auto-detected)")
    args = ap.parse_args()

    store = args.store or _find(STORE_CANDIDATES)
    conf = args.conf or _find_conf()

    result: dict = {
        "subscriptions": [],
        "servers": [],
        "proxy_domains": [],
        "skipped": {},
    }

    if store and os.path.isfile(store):
        servers = _load_servers(store)
        used: set[str] = set(RESERVED)
        skipped: Counter = Counter()
        for i, s in enumerate(servers, 1):
            typ = str(s.get("type"))
            if typ == "Subscribe":
                url = str(s.get("host") or "")
                if url.startswith("http"):
                    result["subscriptions"].append(
                        {
                            "url": url,
                            "info": str(s.get("data") or ""),
                            "title": str(s.get("title") or ""),
                        }
                    )
            elif typ in ("VLESS", "AnyTLS"):
                tag = _sanitize(str(s.get("title") or s.get("host")), i, used)
                v = _to_vless(s, tag) if typ == "VLESS" else _to_anytls(s, tag)
                if v:
                    result["servers"].append(v)
                else:
                    skipped[typ] += 1
            else:
                skipped[typ] += 1
        result["skipped"] = dict(skipped)
    else:
        print("warning: no Shadowrocket server store found", file=sys.stderr)

    if conf and os.path.isfile(conf):
        result["proxy_domains"] = _parse_rules(conf)
    else:
        print("warning: no Shadowrocket .conf rule file found", file=sys.stderr)

    json.dump(result, sys.stdout, indent=2, ensure_ascii=False)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
