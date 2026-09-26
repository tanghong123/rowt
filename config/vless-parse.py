#!/usr/bin/env python3
"""Parse share links (or a subscription) into sing-box outbounds.

    vless-parse.py '<vless://|ss://|trojan://...>' [--tag TAG]  # one link -> one object
    vless-parse.py --multi < links.txt                          # many links -> array
    vless-parse.py --sub '<subscription-url>'                   # fetch+decode -> array
    vless-parse.py --combine < array.json                       # dedupe + uniquify tags
    vless-parse.py --links < array.json                         # outbounds -> share links

Emits sing-box outbound(s) on stdout. Supported protocols: VLESS (incl. Reality),
VMess, AnyTLS, hysteria2 (incl. Salamander obfs), Shadowsocks (incl. 2022 and the
obfs/v2ray plugins), Trojan, and TUIC v5. In --multi/--sub each outbound gets a
unique tag from the link's #name (sanitized), falling back to "server-N".
--links goes the other way, one link per outbound, and only when the link
parses back to the same outbound. Stdlib only — no dependencies. Credentials
never touch the repo; the caller stores output under ~/.config/rowt/.
"""

from __future__ import annotations

import argparse
import binascii
import json
import os
import re
import sys
from urllib.parse import parse_qs, quote, unquote, urlencode, urlsplit
from urllib.request import Request, urlopen

RESERVED = {"escape", "auto", "direct", "corp", "block", "in", "local", "dns-out"}

SCHEMES = (
    "vless://",
    "vmess://",
    "anytls://",
    "hysteria2://",
    "hy2://",
    "ss://",
    "trojan://",
    "tuic://",
)

# The Shadowsocks methods the pinned sing-box (1.13.14) accepts, probed with
# `sing-box check`, which matches them case-sensitively; and the names other
# clients give three of them. A method sing-box refuses fails `sing-box check`
# for the WHOLE config, and with it every render, so it is refused here instead.
SS_METHODS = (
    "none",
    "aes-128-gcm",
    "aes-192-gcm",
    "aes-256-gcm",
    "chacha20-ietf-poly1305",
    "xchacha20-ietf-poly1305",
    "2022-blake3-aes-128-gcm",
    "2022-blake3-aes-256-gcm",
    "2022-blake3-chacha20-poly1305",
    "aes-128-ctr",
    "aes-192-ctr",
    "aes-256-ctr",
    "aes-128-cfb",
    "aes-192-cfb",
    "aes-256-cfb",
    "rc4-md5",
    "chacha20-ietf",
    "xchacha20",
)
SS_ALIASES = {
    "plain": "none",
    "chacha20-poly1305": "chacha20-ietf-poly1305",
    "xchacha20-poly1305": "xchacha20-ietf-poly1305",
}
# A Shadowsocks 2022 password is a base64 key of exactly this many bytes, or
# several joined by ":" (identity keys, which only the AES methods take).
SS_2022_KEY_BYTES = {
    "2022-blake3-aes-128-gcm": 16,
    "2022-blake3-aes-256-gcm": 32,
    "2022-blake3-chacha20-poly1305": 32,
}

_UUID = re.compile(
    r"[0-9a-fA-F]{8}(-?)[0-9a-fA-F]{4}\1[0-9a-fA-F]{4}\1[0-9a-fA-F]{4}\1[0-9a-fA-F]{12}"
)


def _first(qs: dict[str, list[str]], key: str, default: str = "") -> str:
    vals = qs.get(key)
    return unquote(vals[0]) if vals else default


_B64_ALPHABET = frozenset(
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
)


def _b64decode(s: str) -> bytes:
    """`base64.b64decode(s)` under one rule on every Python: a complete pad
    sequence ENDS the data.

    That is CPython <= 3.12's forgiving decoder. 3.13 changed it to read on past
    padding (RFC 4648 §3.3 allows either), so a body with anything after its
    `==` decoded one way under Apple's python3 and another under brew's, and the
    parity gates run whichever python3 is first on PATH. Cutting the input at
    that pad gives every version the old answer, which is also rowt-rs's
    (`sharelink::b64decode`), and it is the forgiving one: junk after a
    subscription's padding is ignored instead of failing the whole body with
    "Incorrect padding". Up to the cut the two rules agree byte for byte,
    errors included.
    """
    try:
        data = s.encode("ascii")  # what b64decode's own str check does first
    except UnicodeEncodeError:
        raise ValueError(
            "string argument should contain only ASCII characters"
        ) from None
    quad = pads = 0
    for i, c in enumerate(data):
        if c == 0x3D:  # "=" — counted only once a quad can take padding
            if quad >= 2:
                pads += 1
                if quad + pads >= 4:
                    return binascii.a2b_base64(data[: i + 1])
            continue
        if c in _B64_ALPHABET:
            pads = 0
            quad = (quad + 1) % 4
    return binascii.a2b_base64(data)


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

    transport = _transport(qs)
    if transport:
        out["transport"] = transport
    return out


def _transport(qs: dict[str, list[str]]) -> dict | None:
    """The `type=` transport that VLESS and Trojan links share; tcp is none."""
    net = _first(qs, "type", "tcp").lower()
    if net in ("ws", "websocket"):
        transport: dict = {"type": "ws", "path": _first(qs, "path", "/")}
        host = _first(qs, "host")
        if host:
            transport["headers"] = {"Host": host}
        return transport
    if net == "grpc":
        return {"type": "grpc", "service_name": _first(qs, "serviceName")}
    if net in ("http", "h2"):
        transport = {"type": "http", "path": _first(qs, "path", "/")}
        host = _first(qs, "host")
        if host:
            transport["host"] = [h for h in host.split(",") if h]
        return transport
    return None


def parse_trojan(link: str, tag: str = "escape") -> dict:
    """Turn a trojan:// URI into a sing-box Trojan outbound.

    The whole userinfo is the password: a `:` in it is not a separator, as it
    is for the uuid-shaped schemes. TLS is on unless the link says
    `security=none`, and the transports are VLESS's.
    """
    u = urlsplit(link)
    password = unquote(u.netloc.rpartition("@")[0])
    server = u.hostname or ""
    port = u.port or 443
    if not password or not server:
        raise ValueError("trojan link missing password or host")

    qs = parse_qs(u.query)
    security = _first(qs, "security", "tls").lower()
    out: dict = {
        "type": "trojan",
        "tag": tag,
        "server": server,
        "server_port": int(port),
        "password": password,
    }
    if security != "none":
        insecure = _first(qs, "allowInsecure") or _first(qs, "insecure", "0")
        tls: dict = {
            "enabled": True,
            "server_name": _first(qs, "sni") or _first(qs, "peer") or server,
            "insecure": insecure in ("1", "true", "True"),
        }
        alpn = _first(qs, "alpn")
        if alpn:
            tls["alpn"] = [a for a in alpn.split(",") if a]
        tls["utls"] = {"enabled": True, "fingerprint": _first(qs, "fp", "chrome")}
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

    transport = _transport(qs)
    if transport:
        out["transport"] = transport
    return out


def parse_tuic(link: str, tag: str = "escape") -> dict:
    """Turn a tuic:// (v5, `uuid:password@host`) URI into a sing-box TUIC outbound.

    QUIC, so TLS is always on. With no `alpn` it offers h3, as Clash.Meta does:
    TUIC servers are commonly set to require it. The uuid is checked because
    sing-box refuses a malformed one for the whole config; tuning values it
    does not know are dropped instead, leaving its defaults.
    """
    u = urlsplit(link)
    uuid = unquote(u.username or "")
    password = unquote(u.password or "")
    server = u.hostname or ""
    port = u.port or 443
    if not uuid or not password or not server:
        raise ValueError("tuic link missing uuid, password or host")
    if not _UUID.fullmatch(uuid):
        raise ValueError("tuic link has a malformed uuid")

    qs = parse_qs(u.query)
    insecure = (
        _first(qs, "allow_insecure")
        or _first(qs, "allowInsecure")
        or _first(qs, "insecure", "0")
    )
    tls: dict = {
        "enabled": True,
        "server_name": _first(qs, "sni") or _first(qs, "peer") or server,
        "insecure": insecure in ("1", "true", "True"),
    }
    alpn = _first(qs, "alpn", "h3")
    if alpn:
        tls["alpn"] = [a for a in alpn.split(",") if a]

    out: dict = {
        "type": "tuic",
        "tag": tag,
        "server": server,
        "server_port": int(port),
        "uuid": uuid,
        "password": password,
    }
    cc = _first(qs, "congestion_control") or _first(qs, "congestion_controller")
    cc = cc.lower().replace("-", "_")
    if cc in ("cubic", "new_reno", "bbr"):
        out["congestion_control"] = cc
    relay = _first(qs, "udp_relay_mode").lower()
    if relay in ("native", "quic"):
        out["udp_relay_mode"] = relay
    out["tls"] = tls
    return out


def _b64_key_bytes(s: str) -> int:
    """How many bytes `s` holds as padded standard base64, the only form
    sing-box reads a Shadowsocks 2022 key in, or -1 if it is not that."""
    if len(s) % 4 or not re.fullmatch(r"[A-Za-z0-9+/]*={0,2}", s):
        return -1
    return len(s) // 4 * 3 - s.count("=")


def _ss_plugin(spec: str) -> tuple[str, str]:
    """A SIP003 `plugin` value (`name;opt=v;flag`) as sing-box's plugin and
    plugin_opts.

    sing-box builds in obfs-local (simple-obfs) and v2ray-plugin and no others,
    and reads their mode case-sensitively, so the mode is lowercased and then
    checked against what it accepts.
    """
    name, _, rest = spec.partition(";")
    name = name.strip().lower()
    if name == "simple-obfs":
        name = "obfs-local"
    if name not in ("obfs-local", "v2ray-plugin"):
        raise ValueError(
            f"shadowsocks plugin {name or '(empty)'} is not supported by sing-box"
        )
    key = "obfs" if name == "obfs-local" else "mode"
    opts = rest.split(";") if rest else []
    mode = None
    for i, opt in enumerate(opts):
        k, eq, v = opt.partition("=")
        if eq and k == key:
            mode = v.lower()
            opts[i] = f"{key}={mode}"
    allowed = ("http", "tls") if name == "obfs-local" else ("websocket", "quic")
    if mode is not None and mode not in allowed:
        raise ValueError(
            f"{name} mode {mode or '(empty)'} is not supported by sing-box"
        )
    if mode == "quic" and "tls" not in opts:
        raise ValueError("v2ray-plugin mode quic needs tls")
    return name, ";".join(opts)


def shadowsocks_outbound(
    tag: str, server: str, port: int, method: str, password: str, plugin: str = ""
) -> dict:
    """A sing-box Shadowsocks outbound, for the ss:// parser and the Shadowrocket
    importer alike, refusing what the pinned sing-box would refuse. Let through,
    one bad method or key fails `sing-box check` for the whole config, and with
    it every render until the server is removed by hand."""
    m = method.strip().lower()
    m = SS_ALIASES.get(m, m)
    if m not in SS_METHODS:
        raise ValueError(
            f"shadowsocks method {method.strip() or '(empty)'} is not supported by sing-box"
        )
    if not password and m != "none":
        raise ValueError("shadowsocks server missing password")
    if not 0 < port < 65536:
        raise ValueError(f"shadowsocks port {port} is out of range")
    size = SS_2022_KEY_BYTES.get(m)
    if size:
        keys = password.split(":")
        if len(keys) > 1 and m == "2022-blake3-chacha20-poly1305":
            raise ValueError(f"shadowsocks {m} takes a single key")
        if any(_b64_key_bytes(k) != size for k in keys):
            raise ValueError(f"shadowsocks {m} needs base64 keys of {size} bytes")
    out: dict = {
        "type": "shadowsocks",
        "tag": tag,
        "server": server,
        "server_port": port,
        "method": m,
        "password": password,
    }
    if plugin:
        out["plugin"], out["plugin_opts"] = _ss_plugin(plugin)
    return out


def _ss_b64(s: str) -> str:
    """vmess's forgiving base64 — either alphabet, padding optional."""
    pad = "=" * (-len(s) % 4)
    try:
        raw = _b64decode(s.replace("-", "+").replace("_", "/") + pad)
    except Exception as e:  # noqa: BLE001
        raise ValueError("ss link is not valid base64") from e
    return raw.decode("utf-8", "replace")


def parse_ss(link: str, tag: str = "escape") -> dict:
    """Turn an ss:// link into a sing-box Shadowsocks outbound.

    Three shapes: SIP002's `ss://base64(method:password)@host:port`, SIP022's
    `ss://method:password@host:port` with each half percent-encoded (how the
    2022 keys travel), and the legacy all-base64
    `ss://base64(method:password@host:port)`. Split by hand, not by urlsplit:
    standard base64 carries `/`, which would end the netloc early. A `plugin=`
    query is SIP003's `name;opt=v`.
    """
    body, _, query = link[len("ss://") :].partition("#")[0].partition("?")
    if "@" in body:
        userinfo, _, hostport = body.rpartition("@")
        cred = unquote(userinfo)
        if ":" not in cred:
            cred = _ss_b64(cred)
    else:
        # A trailing "/" is the path separator of `…/?plugin=`, not base64: a
        # well-formed legacy body ends in the encoding of a port digit, never "/".
        cred, at, hostport = _ss_b64(body.rstrip("/")).rpartition("@")
        if not at:
            raise ValueError("ss link is not method:password@host:port")
    method, colon, password = cred.partition(":")
    if not colon:
        raise ValueError("ss link missing method:password")

    hostport = hostport.rstrip("/")
    if hostport.startswith("["):
        host, _, rest = hostport[1:].partition("]")
        port = rest[1:] if rest.startswith(":") else ""
    else:
        host, sep, port = hostport.rpartition(":")
        if not sep:
            host, port = hostport, ""
    if not host:
        raise ValueError("ss link missing host")
    if not (port.isascii() and port.isdigit() and 0 < int(port) < 65536):
        raise ValueError("ss link missing a valid port")
    return shadowsocks_outbound(
        tag,
        host.lower(),
        int(port),
        method,
        password,
        _first(parse_qs(query), "plugin"),
    )


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
        raw = _b64decode(body.replace("-", "+").replace("_", "/") + pad)
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
    if link.startswith("ss://"):
        return parse_ss(link, tag)
    if link.startswith("trojan://"):
        return parse_trojan(link, tag)
    if link.startswith("tuic://"):
        return parse_tuic(link, tag)
    raise ValueError("unsupported protocol")


def _hostport(o: dict) -> str:
    """`host:port` for a link's netloc. A bare IPv6 address is bracketed, which
    urlsplit needs to read the port back."""
    host = str(o.get("server", ""))
    if ":" in host:
        host = f"[{host}]"
    return f"{host}:{o.get('server_port', '')}"


def _query(pairs: list[tuple[str, str]]) -> str:
    """The query, in the order given, leaving out empty values: absent and empty
    read back alike (parse_qs drops a blank value)."""
    return urlencode([(k, v) for k, v in pairs if v])


def _listed(v: object) -> str:
    """An alpn list, or a transport's hosts, as the comma list a link carries."""
    if not isinstance(v, list):
        raise TypeError("expected a list")
    return ",".join(str(x) for x in v)


def _transport_pairs(tr: dict) -> list[tuple[str, str]]:
    """The `type=` transport of a VLESS or Trojan link — _transport, reversed."""
    ty = str(tr.get("type", ""))
    if ty == "ws":
        return [
            ("type", "ws"),
            ("path", str(tr.get("path", ""))),
            ("host", str((tr.get("headers") or {}).get("Host", ""))),
        ]
    if ty == "grpc":
        return [("type", "grpc"), ("serviceName", str(tr.get("service_name", "")))]
    if ty == "http":
        return [
            ("type", "http"),
            ("path", str(tr.get("path", ""))),
            ("host", _listed(tr.get("host", []))),
        ]
    # An unknown type is written as it is: it cannot parse back to itself, so
    # to_link refuses the outbound rather than dropping its transport.
    return [("type", ty or "tcp")]


def _tls_pairs(tls: dict, insecure_key: str) -> list[tuple[str, str]]:
    """The TLS half of a VLESS or Trojan query: security, sni, alpn, fp, the
    insecure flag under the scheme's own name, and REALITY's key and short id."""
    reality = tls.get("reality") or {}
    return [
        ("security", "reality" if reality else "tls"),
        ("sni", str(tls.get("server_name", ""))),
        ("alpn", _listed(tls.get("alpn", []))),
        ("fp", str((tls.get("utls") or {}).get("fingerprint", ""))),
        (insecure_key, "1" if tls.get("insecure") and insecure_key else ""),
        ("pbk", str(reality.get("public_key", ""))),
        ("sid", str(reality.get("short_id", ""))),
    ]


def _unchecked_link(o: dict) -> str:
    """A share link for outbound o in the form parse_link reads, not yet checked:
    to_link parses it back. The name travels as the fragment, or as vmess's `ps`,
    which is where _link_name looks for it."""
    t = o.get("type")
    tag = str(o.get("tag", ""))
    frag = "#" + quote(tag, safe="")
    tls = o.get("tls") or {}
    tr = o.get("transport") or {}
    if t == "vless":
        q = [("encryption", "none"), ("flow", str(o.get("flow", "")))]
        q += _tls_pairs(tls, "") if tls else [("security", "none")]
        q += _transport_pairs(tr)
        user = quote(str(o.get("uuid", "")), safe="")
        return f"vless://{user}@{_hostport(o)}?{_query(q)}{frag}"
    if t == "trojan":
        q = _tls_pairs(tls, "allowInsecure") if tls else [("security", "none")]
        q += _transport_pairs(tr)
        user = quote(str(o.get("password", "")), safe="")
        return f"trojan://{user}@{_hostport(o)}?{_query(q)}{frag}"
    if t == "tuic":
        q = [
            ("sni", str(tls.get("server_name", ""))),
            ("alpn", _listed(tls.get("alpn", []))),
            ("congestion_control", str(o.get("congestion_control", ""))),
            ("udp_relay_mode", str(o.get("udp_relay_mode", ""))),
            ("allow_insecure", "1" if tls.get("insecure") else ""),
        ]
        user = quote(str(o.get("uuid", "")), safe="")
        pw = quote(str(o.get("password", "")), safe="")
        return f"tuic://{user}:{pw}@{_hostport(o)}?{_query(q)}{frag}"
    if t == "anytls" or t == "hysteria2":
        q = [
            ("sni", str(tls.get("server_name", ""))),
            ("alpn", _listed(tls.get("alpn", []))),
            ("insecure", "1" if tls.get("insecure") else ""),
        ]
        if t == "anytls":
            q.append(("fp", str((tls.get("utls") or {}).get("fingerprint", ""))))
        else:
            obfs = o.get("obfs") or {}
            q += [
                ("upmbps", str(o["up_mbps"]) if "up_mbps" in o else ""),
                ("downmbps", str(o["down_mbps"]) if "down_mbps" in o else ""),
                ("obfs", "salamander" if obfs else ""),
                ("obfs-password", str(obfs.get("password", ""))),
            ]
        pw = quote(str(o.get("password", "")), safe="")
        return f"{t}://{pw}@{_hostport(o)}?{_query(q)}{frag}"
    if t == "shadowsocks":
        method, pw = str(o.get("method", "")), str(o.get("password", ""))
        if method.startswith("2022-"):
            # SIP022: percent-encoded plaintext, the form 2022 keys travel in.
            cred = quote(f"{method}:{pw}", safe=":")
        else:
            # SIP002: URL-safe base64 of method:password, unpadded.
            raw = binascii.b2a_base64(f"{method}:{pw}".encode(), newline=False)
            cred = raw.decode().replace("+", "-").replace("/", "_").rstrip("=")
        plugin = str(o.get("plugin", ""))
        if plugin and o.get("plugin_opts"):
            plugin += ";" + str(o["plugin_opts"])
        tail = "/?" + urlencode([("plugin", plugin)]) if plugin else ""
        return f"ss://{cred}@{_hostport(o)}{tail}{frag}"
    if t == "vmess":
        ty = str(tr.get("type", ""))
        net, host, path = ty or "tcp", "", ""
        if ty == "ws":
            host = str((tr.get("headers") or {}).get("Host", ""))
            path = str(tr.get("path", ""))
        elif ty == "grpc":
            path = str(tr.get("service_name", ""))
        elif ty == "http":
            net, host, path = "h2", _listed(tr.get("host", [])), str(tr.get("path", ""))
        j = {
            "v": "2",
            "ps": tag,
            "add": str(o.get("server", "")),
            "port": str(o.get("server_port", "")),
            "id": str(o.get("uuid", "")),
            "aid": str(o.get("alter_id", 0)),
            "scy": str(o.get("security", "auto")),
            "net": net,
            "type": "none",
            "host": host,
            "path": path,
            "tls": "tls" if tls else "",
            "sni": str(tls.get("server_name", "")),
            "alpn": _listed(tls.get("alpn", [])),
            "fp": str((tls.get("utls") or {}).get("fingerprint", "")),
        }
        body = binascii.b2a_base64(json.dumps(j).encode(), newline=False).decode()
        return "vmess://" + body
    raise ValueError(f"no share link for type {t}")


def to_link(o: dict) -> str | None:
    """The share link for a sing-box outbound, or None when no link carries it
    exactly. Checked, not assumed: the link is parsed back, and anything but the
    same outbound (a host's case aside — names are case-insensitive, and every
    parser but vmess's lowercases them) refuses it. So an outbound built by hand,
    or by another importer, with a setting links cannot express is never
    exported with that setting silently gone."""
    try:
        link = _unchecked_link(o)
        back = parse_link(link, str(o.get("tag", "")))
    except (ValueError, TypeError, AttributeError):
        return None

    def norm(d: dict) -> dict:
        n = {**d, "server": str(d.get("server", "")).lower()}
        tls = n.get("tls")
        # An empty alpn list and none are the same TLS to sing-box, and no link
        # can write the empty one. Not for tuic: there no alpn reads back as h3.
        if isinstance(tls, dict) and tls.get("alpn") == [] and n.get("type") != "tuic":
            n["tls"] = {k: v for k, v in tls.items() if k != "alpn"}
        return n

    return link if norm(back) == norm(o) else None


def _link_name(link: str) -> str:
    """The human name of a share link: the URI `#fragment` for vless/anytls/hy2,
    or the `ps` field for vmess:// (whose name lives inside the base64 JSON)."""
    if link.startswith("vmess://"):
        body = link[len("vmess://") :].strip()
        pad = "=" * (-len(body) % 4)
        try:
            cfg = json.loads(
                _b64decode(body.replace("-", "+").replace("_", "/") + pad).decode(
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
        if not link.startswith(SCHEMES):
            proto = link.split("://", 1)[0]
            print(f"warning: skipping unsupported link ({proto}://)", file=sys.stderr)
            continue
        try:
            out.append(parse_link(link, _tag_for(link, i, used)))
        except ValueError as e:
            print(f"warning: skipping a link ({e})", file=sys.stderr)
    if not out:
        raise ValueError(
            "no usable vless:// / vmess:// / anytls:// / hysteria2:// / ss:// /"
            " trojan:// / tuic:// links found"
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
            body = _b64decode(body.replace("-", "+").replace("_", "/") + pad).decode(
                "utf-8", "replace"
            )
        except Exception as e:  # noqa: BLE001
            raise ValueError(f"could not decode subscription body: {e}") from e
    if "://" not in body:
        raise ValueError(
            "subscription did not yield any share links (Clash/JSON not supported)"
        )
    return body.splitlines()


def main() -> int:
    ap = argparse.ArgumentParser(description="share link(s) -> sing-box outbound JSON")
    ap.add_argument(
        "link",
        nargs="?",
        help="a share link: vless, vmess, anytls, hysteria2, ss, trojan or tuic",
    )
    ap.add_argument("--tag", default="escape", help="outbound tag in single mode")
    ap.add_argument(
        "--multi", action="store_true", help="read links from stdin -> array"
    )
    ap.add_argument("--sub", metavar="URL", help="fetch a subscription URL -> array")
    ap.add_argument(
        "--combine", action="store_true", help="dedupe an array read from stdin"
    )
    ap.add_argument(
        "--links",
        action="store_true",
        help="read an outbound array from stdin -> one share link per line",
    )
    args = ap.parse_args()

    try:
        if args.combine:
            result: object = combine(json.load(sys.stdin))
        elif args.links:
            data = json.load(sys.stdin)
            if not isinstance(data, list):
                raise ValueError("--links wants a JSON array of outbounds")
            for o in data:
                link = to_link(o) if isinstance(o, dict) else None
                if link:
                    print(link)
                else:
                    name = o.get("tag", "?") if isinstance(o, dict) else "?"
                    print(
                        f"warning: '{name}' has settings no share link carries exactly — not exported",
                        file=sys.stderr,
                    )
            return 0
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
