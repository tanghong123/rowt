#!/usr/bin/env python3
"""Parse VLESS / AnyTLS share links (or a subscription) into sing-box outbounds.

    vless-parse.py '<vless://|anytls://...>' [--tag TAG]   # one link -> one object
    vless-parse.py --multi < links.txt                     # many links -> array
    vless-parse.py --sub '<subscription-url>'              # fetch+decode -> array
    vless-parse.py --combine < array.json                  # dedupe + uniquify tags

Emits sing-box outbound(s) on stdout. Supported protocols: VLESS (incl. Reality),
VMess, AnyTLS, and hysteria2 (incl. Salamander obfs). In --multi/--sub each outbound
gets a unique tag from the link's #name (sanitized), falling back to "server-N".
Stdlib only — no dependencies. Credentials never touch the repo; the caller
stores output under ~/.config/rowt/.
"""

from __future__ import annotations

import argparse
import base64
import json
import os
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


def parse_hysteria2(link: str, tag: str = "escape") -> dict:
    """Turn a hysteria2:// (or hy2://) URI into a sing-box hysteria2 outbound.

    hysteria2 is QUIC-based, so TLS is always on. The auth string lives in the
    URI userinfo. Port-hopping (mport) is intentionally not handled.
    """
    u = urlsplit(link)
    password = unquote(u.username or "")
    server = u.hostname or ""
    port = u.port or 443
    if not password or not server:
        raise ValueError("hysteria2 link missing password or host")

    qs = parse_qs(u.query)
    sni = _first(qs, "sni") or _first(qs, "peer") or server
    insecure = _first(qs, "insecure", "0") in ("1", "true", "True")
    alpn = _first(qs, "alpn")

    tls: dict = {"enabled": True, "server_name": sni, "insecure": insecure}
    if alpn:
        tls["alpn"] = [a for a in alpn.split(",") if a]

    out: dict = {
        "type": "hysteria2",
        "tag": tag,
        "server": server,
        "server_port": int(port),
        "password": password,
        "tls": tls,
    }

    up = _first(qs, "upmbps")
    down = _first(qs, "downmbps")
    if up.isdigit():
        out["up_mbps"] = int(up)
    if down.isdigit():
        out["down_mbps"] = int(down)

    if _first(qs, "obfs"):
        out["obfs"] = {"type": "salamander", "password": _first(qs, "obfs-password")}

    return out


def parse_vmess(link: str, tag: str = "escape") -> dict:
    """Turn a vmess:// share link into a sing-box VMess outbound dict.

    Handles the dominant v2rayN format — `vmess://` + base64(JSON) — covering the
    tcp / ws / grpc / http(h2) transports and TLS. kcp/quic transports (which
    sing-box's vmess outbound doesn't speak) are rejected, as is the older
    `security:uuid@host` non-JSON body.
    """
    body = link[len("vmess://") :].strip()
    pad = "=" * (-len(body) % 4)
    try:
        raw = base64.b64decode(body.replace("-", "+").replace("_", "/") + pad)
        cfg = json.loads(raw.decode("utf-8", "replace"))
    except Exception as e:  # noqa: BLE001
        raise ValueError(f"vmess link is not base64-JSON ({e})") from e
    if not isinstance(cfg, dict):
        raise ValueError("vmess link is not a JSON object")

    def s(key: str, default: str = "") -> str:
        v = cfg.get(key)
        return str(v).strip() if v is not None else default

    server = s("add")
    uuid = s("id")
    if not server or not uuid:
        raise ValueError("vmess link missing id or host")

    net = (s("net", "tcp") or "tcp").lower()
    host = s("host")
    path = s("path", "/")

    out: dict = {
        "type": "vmess",
        "tag": tag,
        "server": server,
        "server_port": int(cfg.get("port", 443)),
        "uuid": uuid,
        "security": s("scy", "auto") or "auto",
        "alter_id": int(cfg.get("aid", 0) or 0),
    }

    if net in ("ws", "websocket"):
        transport: dict = {"type": "ws", "path": path or "/"}
        if host:
            transport["headers"] = {"Host": host}
        out["transport"] = transport
    elif net == "grpc":
        out["transport"] = {"type": "grpc", "service_name": path}
    elif net in ("http", "h2"):
        transport = {"type": "http", "path": path or "/"}
        if host:
            transport["host"] = [h for h in host.split(",") if h]
        out["transport"] = transport
    elif net in ("kcp", "quic"):
        raise ValueError(f"vmess {net} transport unsupported by sing-box")
    # net == "tcp": plain (http-header obfs isn't supported by sing-box, ignored).

    if s("tls").lower() in ("tls", "reality", "xtls", "1", "true"):
        tls: dict = {"enabled": True, "server_name": s("sni") or host or server}
        alpn = s("alpn")
        if alpn:
            tls["alpn"] = [a for a in alpn.split(",") if a]
        fp = s("fp")
        if fp:
            tls["utls"] = {"enabled": True, "fingerprint": fp}
        out["tls"] = tls

    return out


def parse_link(link: str, tag: str = "escape") -> dict:
    """Dispatch a share link to the right protocol parser."""
    if link.startswith("vless://"):
        return parse_vless(link, tag)
    if link.startswith("vmess://"):
        return parse_vmess(link, tag)
    if link.startswith("anytls://"):
        return parse_anytls(link, tag)
    if link.startswith(("hysteria2://", "hy2://")):
        return parse_hysteria2(link, tag)
    raise ValueError("unsupported protocol")


def _link_name(link: str) -> str:
    """The human name of a share link: the URI `#fragment` for vless/anytls/hy2,
    or the `ps` field for vmess:// (whose name lives inside the base64 JSON)."""
    if link.startswith("vmess://"):
        body = link[len("vmess://") :].strip()
        pad = "=" * (-len(body) % 4)
        try:
            cfg = json.loads(
                base64.b64decode(body.replace("-", "+").replace("_", "/") + pad).decode(
                    "utf-8", "replace"
                )
            )
        except Exception:  # noqa: BLE001
            return ""
        return str(cfg.get("ps", "")).strip() if isinstance(cfg, dict) else ""
    return unquote(urlsplit(link).fragment).strip()


def _tag_for(link: str, index: int, used: set[str]) -> str:
    """Derive a unique, filesystem/API-safe tag from a link's name."""
    name = _link_name(link)
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
        if "://" not in link:
            continue  # subscription header lines (e.g. "REMARKS=...") — skip quietly
        if not link.startswith(
            ("vless://", "vmess://", "anytls://", "hysteria2://", "hy2://")
        ):
            proto = link.split("://", 1)[0]
            print(f"warning: skipping unsupported link ({proto}://)", file=sys.stderr)
            continue
        try:
            out.append(parse_link(link, _tag_for(link, i, used)))
        except ValueError as e:
            print(f"warning: skipping a link ({e})", file=sys.stderr)
    if not out:
        raise ValueError(
            "no usable vless:// / vmess:// / anytls:// / hysteria2:// links found"
        )
    return out


def key_of(o: dict) -> str:
    """Identity of an outbound — the same endpoint regardless of its display name.
    Two outbounds with this key equal are the same server (dedup unit)."""
    secret = o.get("uuid") or o.get("password") or ""
    return f"{o.get('type')}:{o.get('server')}:{o.get('server_port')}:{secret}"


def combine(outbounds: list[dict]) -> list[dict]:
    """Dedupe outbounds by identity (type/server/port/secret) — even when the names
    differ — keeping the FIRST occurrence and uniquifying tags. A dropped duplicate
    is announced on stderr (so importing a server you already have is visible, not
    silent), naming both the kept and the skipped tag."""
    used: set[str] = set(RESERVED)
    kept: dict[str, str] = {}  # identity key -> the tag we kept for it
    result: list[dict] = []
    for i, o in enumerate(outbounds, 1):
        key = key_of(o)
        if key in kept:
            dropped = o.get("tag") or f"server-{i}"
            if dropped != kept[key]:
                print(
                    f"note: not importing '{dropped}' — same server as '{kept[key]}' ({o.get('server')}:{o.get('server_port')})",
                    file=sys.stderr,
                )
            continue
        base = o.get("tag") or f"server-{i}"
        if base in RESERVED:
            base = f"{base}-{i}"
        tag, n = base, 2
        while tag in used:
            tag = f"{base}-{n}"
            n += 1
        used.add(tag)
        o["tag"] = tag
        kept[key] = tag
        result.append(o)
    return result


def fetch_subscription(url: str) -> list[str]:
    """Fetch a subscription URL and return its share links.

    Handles the common v2ray format (base64-encoded, newline-separated links)
    as well as a plain-text list of links.

    Sends a Shadowrocket-style User-Agent by default: many airports gate on it,
    returning raw share links for it but Clash YAML (which we can't parse) for a
    generic UA. Override with ROWT_SUB_UA if a provider needs a different signal.
    """
    ua = (
        os.environ.get("ROWT_SUB_UA")
        or "Shadowrocket/2.2.28 (iPhone; iOS 17.5.1; Scale/3.00)"
    )
    req = Request(url, headers={"User-Agent": ua})
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
