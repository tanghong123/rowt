#!/usr/bin/env python3
"""Parse VLESS / AnyTLS share links (or a subscription) into sing-box outbounds.

    vless-parse.py '<vless://|anytls://...>' [--tag TAG]   # one link -> one object
    vless-parse.py --multi < links.txt                     # many links -> array
    vless-parse.py --sub '<subscription-url>'              # fetch+decode -> array
    vless-parse.py --combine < array.json                  # dedupe + uniquify tags

Emits sing-box outbound(s) on stdout. Supported protocols: VLESS (incl. Reality)
and AnyTLS. In --multi/--sub each outbound gets a unique tag from the link's
#name (sanitized), falling back to "server-N". Stdlib only — no dependencies.
Credentials never touch the repo; the caller stores output under
~/.config/rowt/.
"""

from __future__ import annotations

import argparse
import base64
import json
import re
import sys
from urllib.parse import parse_qs, unquote, urlsplit
from urllib.request import Request, urlopen

RESERVED = {"escape", "auto", "direct", "corp", "block", "in", "local", "dns-out"}


def _first(qs: dict[str, list[str]], key: str, default: str = "") -> str:
    vals = qs.get(key)
    return unquote(vals[0]) if vals else default


def parse_vless(link: str, tag: str = "escape") -> dict:
    """Turn a vless:// URI into a sing-box VLESS outbound dict."""
    u = urlsplit(link)
    uuid = unquote(u.username or "")
    server = u.hostname or ""
    port = u.port or 443
    if not uuid or not server:
        raise ValueError("vless link missing uuid or host")

    qs = parse_qs(u.query)
    security = _first(qs, "security", "none").lower()
    net = _first(qs, "type", "tcp").lower()
    flow = _first(qs, "flow")
    sni = _first(qs, "sni") or _first(qs, "peer") or server
    fp = _first(qs, "fp", "chrome")
    alpn = _first(qs, "alpn")

    out: dict = {
        "type": "vless",
        "tag": tag,
        "server": server,
        "server_port": int(port),
        "uuid": uuid,
    }
    if flow:
        out["flow"] = flow

    if security in ("tls", "reality", "xtls"):
        tls: dict = {"enabled": True, "server_name": sni}
        if alpn:
            tls["alpn"] = [a for a in alpn.split(",") if a]
        tls["utls"] = {"enabled": True, "fingerprint": fp}
        if security == "reality":
            pbk = _first(qs, "pbk")
            if not pbk:
                raise ValueError("reality link missing pbk (public key)")
            tls["reality"] = {
                "enabled": True,
                "public_key": pbk,
                "short_id": _first(qs, "sid"),
            }
        out["tls"] = tls

    if net in ("ws", "websocket"):
        transport: dict = {"type": "ws", "path": _first(qs, "path", "/")}
        host = _first(qs, "host")
        if host:
            transport["headers"] = {"Host": host}
        out["transport"] = transport
    elif net == "grpc":
        out["transport"] = {"type": "grpc", "service_name": _first(qs, "serviceName")}
    elif net in ("http", "h2"):
        transport = {"type": "http", "path": _first(qs, "path", "/")}
        host = _first(qs, "host")
        if host:
            transport["host"] = [h for h in host.split(",") if h]
        out["transport"] = transport

    return out


def parse_anytls(link: str, tag: str = "escape") -> dict:
    """Turn an anytls:// URI into a sing-box AnyTLS outbound dict."""
    u = urlsplit(link)
    password = unquote(u.username or "")
    server = u.hostname or ""
    port = u.port or 443
    if not password or not server:
        raise ValueError("anytls link missing password or host")

    qs = parse_qs(u.query)
    sni = _first(qs, "sni") or _first(qs, "peer") or server
    insecure = _first(qs, "insecure", "0") in ("1", "true", "True")
    alpn = _first(qs, "alpn")

    tls: dict = {
        "enabled": True,
        "server_name": sni,
        "insecure": insecure,
        "utls": {"enabled": True, "fingerprint": _first(qs, "fp", "chrome")},
    }
    if alpn:
        tls["alpn"] = [a for a in alpn.split(",") if a]
    return {
        "type": "anytls",
        "tag": tag,
        "server": server,
        "server_port": int(port),
        "password": password,
        "tls": tls,
    }


def parse_link(link: str, tag: str = "escape") -> dict:
    """Dispatch a share link to the right protocol parser."""
    if link.startswith("vless://"):
        return parse_vless(link, tag)
    if link.startswith("anytls://"):
        return parse_anytls(link, tag)
    raise ValueError("unsupported protocol")


def _tag_for(link: str, index: int, used: set[str]) -> str:
    """Derive a unique, filesystem/API-safe tag from a link's #name."""
    name = unquote(urlsplit(link).fragment).strip()
    base = re.sub(r"[^A-Za-z0-9._-]+", "-", name).strip("-") or f"server-{index}"
    if base in RESERVED:
        base = f"{base}-{index}"
    tag, n = base, 2
    while tag in used:
        tag = f"{base}-{n}"
        n += 1
    used.add(tag)
    return tag


def parse_many(links: list[str]) -> list[dict]:
    """Parse a list of share links; skip (with a warning) anything unusable."""
    out: list[dict] = []
    used: set[str] = set(RESERVED)
    for i, raw in enumerate(links, 1):
        link = raw.strip()
        if not link or link.startswith("#"):
            continue
        if not (link.startswith("vless://") or link.startswith("anytls://")):
            proto = link.split("://", 1)[0] if "://" in link else "?"
            print(f"warning: skipping unsupported link ({proto}://)", file=sys.stderr)
            continue
        try:
            out.append(parse_link(link, _tag_for(link, i, used)))
        except ValueError as e:
            print(f"warning: skipping a link ({e})", file=sys.stderr)
    if not out:
        raise ValueError("no usable vless:// / anytls:// links found")
    return out


def combine(outbounds: list[dict]) -> list[dict]:
    """Dedupe outbounds by (type, server, port, secret) and uniquify tags."""
    used: set[str] = set(RESERVED)
    seen: set[str] = set()
    result: list[dict] = []
    for i, o in enumerate(outbounds, 1):
        secret = o.get("uuid") or o.get("password") or ""
        key = f"{o.get('type')}:{o.get('server')}:{o.get('server_port')}:{secret}"
        if key in seen:
            continue
        seen.add(key)
        base = o.get("tag") or f"server-{i}"
        if base in RESERVED:
            base = f"{base}-{i}"
        tag, n = base, 2
        while tag in used:
            tag = f"{base}-{n}"
            n += 1
        used.add(tag)
        o["tag"] = tag
        result.append(o)
    return result


def fetch_subscription(url: str) -> list[str]:
    """Fetch a subscription URL and return its share links.

    Handles the common v2ray format (base64-encoded, newline-separated links)
    as well as a plain-text list of links.
    """
    req = Request(url, headers={"User-Agent": "rowt/1.0"})
    with urlopen(req, timeout=20) as r:  # noqa: S310 (user-supplied sub URL)
        body = r.read().decode("utf-8", "replace").strip()
    if "://" not in body:
        # assume base64 (may be url-safe, may lack padding)
        pad = "=" * (-len(body) % 4)
        try:
            body = base64.b64decode(
                body.replace("-", "+").replace("_", "/") + pad
            ).decode("utf-8", "replace")
        except Exception as e:  # noqa: BLE001
            raise ValueError(f"could not decode subscription body: {e}") from e
    if "://" not in body:
        raise ValueError(
            "subscription did not yield any share links (Clash/JSON not supported)"
        )
    return body.splitlines()


def main() -> int:
    ap = argparse.ArgumentParser(description="share link(s) -> sing-box outbound JSON")
    ap.add_argument("link", nargs="?", help="a vless:// / anytls:// share link")
    ap.add_argument("--tag", default="escape", help="outbound tag in single mode")
    ap.add_argument(
        "--multi", action="store_true", help="read links from stdin -> array"
    )
    ap.add_argument("--sub", metavar="URL", help="fetch a subscription URL -> array")
    ap.add_argument(
        "--combine", action="store_true", help="dedupe an array read from stdin"
    )
    args = ap.parse_args()

    try:
        if args.combine:
            result: object = combine(json.load(sys.stdin))
        elif args.sub:
            result = parse_many(fetch_subscription(args.sub))
        elif args.multi:
            result = parse_many(sys.stdin.read().splitlines())
        elif args.link:
            result = parse_link(args.link.strip(), args.tag)
        else:
            ap.error("provide a link, --multi, --sub, or --combine")
            return 2
    except ValueError as e:
        print(f"error: {e}", file=sys.stderr)
        return 1

    json.dump(result, sys.stdout, indent=2)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
