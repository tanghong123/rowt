#!/usr/bin/env python3
"""Extract servers, subscriptions and proxy rules from a Shadowrocket install.

Reads Shadowrocket's binary server store (NSKeyedArchiver plist: the app
container's ServerManager, or the copy in iCloud Drive, whose entries an older
and a newer Shadowrocket write in two layouts) and its Surge-style rule config,
and emits a JSON summary on stdout:

    {
      "subscriptions": [{"url": "...", "info": "...", "title": "..."}],
      "servers":       [ <sing-box outbound>, ... ],   # the protocols rowt speaks
      "proxy_domains": ["google.com", ...],            # [Rule] ...,PROXY
      "skipped":       {"WireGuard": 1, "Shadowsocks": 1} # not importable
    }

Nothing is written and no credentials are printed to stderr. Stdlib only.
"""

from __future__ import annotations

import argparse
import glob
import importlib.util
import json
import os
import plistlib
import re
import sys
from collections import Counter
from pathlib import Path
from plistlib import UID

# The share-link parser's Shadowsocks builder, so a method or key sing-box would
# refuse is refused the same way here (hyphenated filename → importlib).
_spec = importlib.util.spec_from_file_location(
    "vless_parse", Path(__file__).resolve().parent / "vless-parse.py"
)
assert _spec and _spec.loader
vp = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(vp)

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
    """Every dict in `$objects` with both a `type` and a `host`, its values
    dereferenced one level. Two shapes, both seen in real stores: a flat dict
    with the field names as its keys (the app container's ServerManager, from
    an older Shadowrocket), and an archived NSMutableDictionary with an NS.keys
    list beside an NS.objects list (the copy a newer Shadowrocket keeps in
    iCloud Drive, the only store a Mac has when it cannot open the app's
    container), zipped into a flat dict here with its keys through str()."""
    objs = plistlib.load(open(store, "rb"))["$objects"]

    def d(x):  # dereference a CF$UID, unwrapping NSString/NSMutableString objects
        v = objs[x.data] if isinstance(x, UID) else x
        if isinstance(v, dict) and "NS.string" in v:  # NSString wrapper -> inner str
            return v["NS.string"]
        return v

    out = []
    for o in objs:
        if not isinstance(o, dict):
            continue
        ks, vs = o.get("NS.keys"), o.get("NS.objects")
        if isinstance(ks, list) and isinstance(vs, list):
            e = {str(d(k)): d(v) for k, v in zip(ks, vs)}
            if "type" in e and "host" in e:
                out.append(e)
        elif "type" in o and "host" in o:
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
    try:
        transport = _vless_transport(s)
    except _Unknown:
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
    if transport:
        out["transport"] = transport
    return out


def _vless_transport(s: dict) -> dict | None:
    """The newer record (the one with a `path` key, which the archived iCloud
    store writes) keeps a VLESS entry's transport in the slots _obfs_transport
    reads, as a real ws entry made from rowt's own link showed on 2026-11-06:
    the Host in `obfsParam`, the path in `path`. Through 3.8.37 this converter
    read `obfsParam` as the path, so a ws VLESS imported with its Host as its
    path and no Host header. The older flat record has no `path` key at all,
    and no real ws or grpc entry of that shape has been seen, so it keeps the
    reading it has had since 1.0.0 (the path from `obfsParam`, then
    `pluginParam`; a service name from `obfsParam`), unpinned and unchanged.
    An obfs the importer does not know raises _Unknown on either record, and
    the entry is skipped as a Trojan or VMess one is."""
    if "path" in s:
        return _obfs_transport(s)
    obfs = str(s.get("obfs") or "none").lower()
    if obfs == "none":
        return None
    if obfs in ("ws", "websocket"):
        return {
            "type": "ws",
            "path": str(s.get("obfsParam") or s.get("pluginParam") or "/"),
        }
    if obfs == "grpc":
        return {"type": "grpc", "service_name": str(s.get("obfsParam") or "")}
    raise _Unknown(obfs)


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


def _to_shadowsocks(s: dict, tag: str) -> dict | None:
    """Shadowrocket keeps simple-obfs in `obfs` (http/tls), its host in
    `obfsParam`. In the store this was written against, `plugin` was "none" or
    absent and `pluginParam` empty on every entry, so anything else there is
    skipped rather than guessed at. So are a server `chain`ed through another,
    since rowt cannot express the hop, an SSR `proto`, and a `tls` flag."""
    host, port = s.get("host"), s.get("port")
    if not host or s.get("chain") or s.get("tls"):
        return None
    if str(s.get("proto") or "none").lower() not in ("none", "origin"):
        return None
    if str(s.get("plugin") or "none").lower() != "none":
        return None
    obfs = str(s.get("obfs") or "none").lower()
    plugin = ""
    if obfs in ("http", "tls"):
        plugin = f"obfs-local;obfs={obfs}"
        if s.get("obfsParam"):
            plugin += f";obfs-host={s.get('obfsParam')}"
    elif obfs != "none":
        return None
    try:
        return vp.shadowsocks_outbound(
            tag,
            str(host),
            int(port or 443),
            str(s.get("method") or ""),
            str(s.get("password") or ""),
            plugin,
        )
    except ValueError:
        return None


def _to_hysteria2(s: dict, tag: str) -> dict | None:
    """Shadowrocket's Hysteria2 entry, as a colleague's dump of a real store
    showed it (names 2026-10-31, non-secret values 2026-11-01): the auth string
    in `password` (`uuid` is Shadowrocket's own id, as for VLESS), `alpn`
    ("h3"), `allowInsecure` and `udp` as ints, the port a string, `tls` False
    — meaningless for a protocol that is TLS over QUIC, so not read — `obfs`,
    `plugin` and `proto` all "none", `hpkp` a certificate pin, and
    `pluginParam` an archived NSDictionary whose contents the dump did not
    show. Salamander is the one obfs hysteria2 has and takes a password, which
    is assumed to sit where the other types keep their obfs argument
    (`obfsParam`) — unconfirmed, the real entry having no obfs; without one, or
    with any other obfs, the entry is skipped rather than guessed at. So is one
    with a plugin or a proto other than none, as a Shadowsocks entry is: a
    shape no store has shown. `udp` is not read, as for every other type. The
    pin rides along raw as `_pin_sha256` — sing-box pins a public key, not a
    certificate, so `server import --apply` fetches the certificate and checks
    the pin before recording the public-key hash, or refuses the server.
    Settings in `pluginParam` are not read; a note on stderr says so. The
    outbound has the shape parse_hysteria2 builds from a link, so a store
    import and a link import of one node agree (no uTLS: QUIC has none)."""
    host, port, pw = s.get("host"), s.get("port"), s.get("password")
    if not host or not pw:
        return None
    if str(s.get("plugin") or "none").lower() != "none":
        return None
    if str(s.get("proto") or "none").lower() != "none":
        return None
    sni = str(s.get("peer") or host)
    tls: dict = {
        "enabled": True,
        "server_name": sni,
        "insecure": str(s.get("allowInsecure") or "0").lower() in ("1", "true"),
    }
    alpn = str(s.get("alpn") or "")
    if alpn:
        tls["alpn"] = [a for a in alpn.split(",") if a]
    out: dict = {
        "type": "hysteria2",
        "tag": tag,
        "server": str(host),
        "server_port": int(port or 443),
        "password": str(pw),
        "tls": tls,
    }
    obfs = str(s.get("obfs") or "none").lower()
    if obfs == "salamander":
        opw = s.get("obfsParam")
        if not opw:
            return None
        out["obfs"] = {"type": "salamander", "password": str(opw)}
    elif obfs != "none":
        return None
    # Only for an entry that converts: a skipped one carries and says nothing.
    # The pin (hpkp, Hysteria 2's pinSHA256: a certificate hash) rides along
    # raw as `_pin_sha256` for `server import --apply`, which fetches the
    # server's certificate, checks it against the pin and records sing-box's
    # public-key pin — or refuses the server. Not a sing-box field; `combine`
    # strips what no fetch consumed.
    if s.get("hpkp"):
        out["_pin_sha256"] = str(s.get("hpkp"))
    # An archived dictionary holds settings when its key list is non-empty;
    # any other shape (a string, from an older store) when it is truthy.
    pp = s.get("pluginParam")
    held = pp.get("NS.keys") if isinstance(pp, dict) else pp
    if held:
        print(
            f"note: '{tag}': settings in Shadowrocket's pluginParam are not imported (the importer does not read that field)",
            file=sys.stderr,
        )
    return out


class _Unknown(Exception):
    """An `obfs` the transport reader does not know."""


def _obfs_transport(s: dict) -> dict | None:
    """The transport a Trojan, VMess or VLESS entry carries, in the shape
    _transport builds from a link's `type=`. Shadowrocket keeps the kind in
    `obfs` ("websocket" for ws, "grpc"), the path (or the gRPC service name) in
    its own `path` key and the ws Host header in `obfsParam` (and again under
    obfsParams["Host"], which is not read); the slots are the same for all three
    types, as real entries made from rowt's own links showed on 2026-11-06.
    "none", or unset, is no transport. Anything else raises _Unknown, and the
    caller skips the entry rather than import a server that would never connect
    with its transport dropped. What is not here is not pinned: a Trojan entry
    Shadowrocket made from an h2 link (`type=http`) came out with `obfs` none,
    the path and Host kept in their slots and no h2 flag set, so Shadowrocket
    keeps no h2 transport for a Trojan and the entry imports as it holds it,
    plain TLS; what "h2" or "http" in `obfs` would mean has not been seen."""
    obfs = str(s.get("obfs") or "none").lower()
    if obfs == "none":
        return None
    if obfs in ("ws", "websocket"):
        transport: dict = {"type": "ws", "path": str(s.get("path") or "/")}
        host = str(s.get("obfsParam") or "")
        if host:
            transport["headers"] = {"Host": host}
        return transport
    if obfs == "grpc":
        return {"type": "grpc", "service_name": str(s.get("path") or "")}
    raise _Unknown(obfs)


def _to_trojan(s: dict, tag: str) -> dict | None:
    """Shadowrocket's Trojan entry, as a real store holds one made from rowt's
    own link (2026-11-06, made-up values): the password in `password`, the SNI
    in `peer`, `allowInsecure` an int, `alpn` a comma list, the transport in
    `obfs`/`path`/`obfsParam`, `tls` unset (a Trojan is always TLS, so it is not
    read) and no fingerprint field at all, so the outbound takes the `chrome`
    the link parser takes when a link names none. The shape is parse_trojan's,
    so a store import and a link import of one node agree."""
    host, pw = s.get("host"), s.get("password")
    if not host or not pw:
        return None
    try:
        transport = _obfs_transport(s)
    except _Unknown:
        return None
    tls: dict = {
        "enabled": True,
        "server_name": str(s.get("peer") or host),
        "insecure": bool(s.get("allowInsecure")),
    }
    alpn = str(s.get("alpn") or "")
    if alpn:
        tls["alpn"] = [a for a in alpn.split(",") if a]
    tls["utls"] = {"enabled": True, "fingerprint": "chrome"}
    out: dict = {
        "type": "trojan",
        "tag": tag,
        "server": str(host),
        "server_port": int(s.get("port") or 443),
        "password": str(pw),
        "tls": tls,
    }
    if transport:
        out["transport"] = transport
    return out


def _to_vmess(s: dict, tag: str) -> dict | None:
    """Shadowrocket spells the type `Vmess`. The id sits in `password`, as for
    VLESS (`uuid` is Shadowrocket's own id, and is not a fallback here);
    `alterId` is a string, `method` the cipher (sing-box's `security`), `tls` a
    real bool, `alpn` a comma list, the transport as for Trojan. No fingerprint
    is stored, and parse_vmess adds uTLS only when a link names one, so none is
    added here. The SNI falls back from `peer` through the ws Host to the host,
    as the link's falls from `sni` through `host` to `add`. An `alterId` that is
    not a number skips the entry, as a Shadowsocks port that is not one does."""
    host, vid = s.get("host"), s.get("password")
    if not host or not vid:
        return None
    try:
        transport = _obfs_transport(s)
        alter_id = int(s.get("alterId") or 0)
    except (_Unknown, ValueError):
        return None
    out: dict = {
        "type": "vmess",
        "tag": tag,
        "server": str(host),
        "server_port": int(s.get("port") or 443),
        "uuid": str(vid),
        "security": str(s.get("method") or "auto"),
        "alter_id": alter_id,
    }
    if transport:
        out["transport"] = transport
    if s.get("tls"):
        tls: dict = {
            "enabled": True,
            "server_name": str(s.get("peer") or s.get("obfsParam") or host),
        }
        alpn = str(s.get("alpn") or "")
        if alpn:
            tls["alpn"] = [a for a in alpn.split(",") if a]
        out["tls"] = tls
    return out


def _to_tuic(s: dict, tag: str) -> dict | None:
    """The TUIC uuid sits in `user` (the `uuid` key is, as ever, Shadowrocket's
    own id), the password in `password`, the SNI in `peer`, `allowInsecure` an
    int, `alpn` a comma list (h3 when unset, as parse_tuic offers it), the
    congestion control in `proto` ("bbr"), and the UDP relay mode in `udp` as a
    number: 1 is native, 2 is quic (entries made from links naming each,
    2026-11-06). Any other value leaves sing-box's default. A uuid sing-box
    would refuse skips the entry, as parse_tuic refuses the link."""
    host, uuid, pw = s.get("host"), s.get("user"), s.get("password")
    if not host or not uuid or not pw:
        return None
    uuid = str(uuid)
    if not vp._UUID.fullmatch(uuid):
        return None
    tls: dict = {
        "enabled": True,
        "server_name": str(s.get("peer") or host),
        "insecure": bool(s.get("allowInsecure")),
    }
    alpn = str(s.get("alpn") or "h3")
    if alpn:
        tls["alpn"] = [a for a in alpn.split(",") if a]
    out: dict = {
        "type": "tuic",
        "tag": tag,
        "server": str(host),
        "server_port": int(s.get("port") or 443),
        "uuid": uuid,
        "password": str(pw),
    }
    cc = str(s.get("proto") or "").lower().replace("-", "_")
    if cc in ("cubic", "new_reno", "bbr"):
        out["congestion_control"] = cc
    relay = {"1": "native", "2": "quic"}.get(str(s.get("udp")))
    if relay:
        out["udp_relay_mode"] = relay
    out["tls"] = tls
    return out


def _names_under(pattern: str) -> bool:
    """A DOMAIN-WILDCARD pattern of the one shape a lane entry can hold: `*.x`,
    with no other wildcard (`*`, `?`, or a `[...]` class) in `x`."""
    rest = pattern[2:]
    return (
        pattern.startswith("*.") and bool(rest) and not any(c in rest for c in "*?[]")
    )


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
        elif action == "PROXY" and typ == "DOMAIN-WILDCARD" and _names_under(value):
            # `*.x` (the names under x) is rowt's dot-led `.x`, which is how
            # `rowt config export --to shadowrocket` writes one. Any other
            # wildcard shape has no rowt rule, so it is left out.
            doms.append(value[1:].lower())
    # dedupe, keep order
    seen: set[str] = set()
    return [x for x in doms if not (x in seen or seen.add(x))]


# By Shadowrocket's own `type` spelling (`Vmess`, with `VMess` accepted too).
CONVERTERS = {
    "VLESS": _to_vless,
    "AnyTLS": _to_anytls,
    "Shadowsocks": _to_shadowsocks,
    "Hysteria2": _to_hysteria2,
    "Trojan": _to_trojan,
    "Vmess": _to_vmess,
    "VMess": _to_vmess,
    "TUIC": _to_tuic,
}


def main() -> int:
    ap = argparse.ArgumentParser(description="Shadowrocket -> rowt importer")
    ap.add_argument("--store", help="ServerManager / v2.model plist (auto-detected)")
    ap.add_argument("--conf", help="default.conf rule file (auto-detected)")
    ap.add_argument(
        "--detect",
        action="store_true",
        help="print 'shadowrocket' if importable data is present, else nothing",
    )
    args = ap.parse_args()

    store = args.store or _find(STORE_CANDIDATES)

    if args.detect:
        # The store is an instant os.path.isfile check; _find_conf() does a
        # recursive glob over the whole iCloud Drive (com~apple~CloudDocs/**)
        # that can take many seconds and stall on dataless files. Only pay for
        # it when there's no store, so detection stays inside its time box.
        if store or args.conf or _find_conf():
            print("shadowrocket")
        return 0

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
            elif typ in CONVERTERS:
                tag = _sanitize(str(s.get("title") or s.get("host")), i, used)
                convert = CONVERTERS[typ]
                v = convert(s, tag)
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
