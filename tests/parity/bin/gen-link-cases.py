#!/usr/bin/env python3
"""Generate share-link cases for the vless-parse differential gate.

Synthetic only. Every credential is a fixed pattern (`00000000-…`, `pw-N`), every
host is a documentation name or an RFC 5737 / RFC 3849 address — nothing here
comes from a real subscription, and nothing here may.

`config/test_parse.py` is a checklist, not a corpus: it calls the parser
functions directly, so it cannot be replayed through two binaries, and it never
touches `parse_vless` or `parse_anytls` at all. So the axes below are crossed
deliberately rather than sampled — protocol x security x transport x userinfo
shape x port shape x percent-torture x name — because that is where the Python
has no coverage today and the port has no evidence.

Each case is a directory holding `argv` (one argument per line) and `stdin`.
"""
from __future__ import annotations
import json, os, random, sys

UUIDS = [f"00000000-0000-4000-8000-{i:012d}" for i in range(1, 7)]
# Userinfo shapes: plain, percent-encoded, doubly-encoded, the last-@ rule,
# empty (which every parser must refuse).
USERS = UUIDS + ["pw-1", "pw-2", "p%40ss", "p%2540ss", "p+w", "a:b@c", "%E4%B8%AD", ""]
HOSTS = ["h1.example", "h2.example", "example.com", "HOST.Example",
         "192.0.2.1", "198.51.100.7", "203.0.113.9",
         "[2001:db8::1]", "[bad]", "[1.2.3.4]", "[2001:db8::1",
         "xn--fiq228c.example", "中文.example", "h.example℀", ""]
PORTS = ["", ":443", ":0", ":8443", ":65535", ":65536", ":99999", ":abc", ":00443", ":"]
# `#name` — including ones that sanitize to nothing, collide, or are reserved.
NAMES = ["JP", "Tokyo 01", "香港 01", "escape", "auto", "direct", "---", "",
         "a/b", "%E9%A6%99%E6%B8%AF", "name#with", "dup", "dup", "Ds415", "..", "-x-"]
SECURITY = ["", "none", "tls", "reality", "xtls", "TLS", "Reality"]
NETS = ["", "tcp", "ws", "websocket", "grpc", "http", "h2", "kcp", "QUIC"]
PATHS = ["/", "/ray", "%2Fa+b", "%252Fa", "%zz", "/p?x", ""]
ALPNS = ["", "h3", "h3,h2", "h3,,h2", ","]
FPS = ["", "chrome", "firefox", "safari"]
INSECURE = ["0", "1", "true", "True", "yes", ""]
MBPS = ["", "20", "0", "abc", "20.5", "007"]

VMESS_NETS = ["tcp", "ws", "grpc", "http", "h2", "kcp", "quic", "", "WS"]
VMESS_TLS = ["", "tls", "1", "true", "reality", "none", "TLS"]
VMESS_PORTS = [443, "443", "8443", 0, "abc", "", 443.0]


def q(r: random.Random, proto: str) -> str:
    """A query string with the parameters that protocol actually reads, plus
    some it does not (which must be ignored identically)."""
    parts = []
    add = lambda k, v: parts.append(f"{k}={v}")
    if proto == "vless":
        add("security", r.choice(SECURITY))
        add("type", r.choice(NETS))
        if r.random() < 0.4:
            add("flow", r.choice(["", "xtls-rprx-vision"]))
        if r.random() < 0.5:
            add("pbk", r.choice(["", "PUBKEY-0001"]))
        if r.random() < 0.4:
            add("sid", r.choice(["", "ab", "00"]))
        if r.random() < 0.6:
            add("serviceName", r.choice(["", "grpc-svc", "a%2Fb"]))
    else:
        add("insecure", r.choice(INSECURE))
        if proto in ("hysteria2", "hy2"):
            add("upmbps", r.choice(MBPS))
            add("downmbps", r.choice(MBPS))
            if r.random() < 0.5:
                add("obfs", r.choice(["", "salamander"]))
                add("obfs-password", r.choice(["", "obfs-1", "o%20p"]))
    if r.random() < 0.7:
        add("sni", r.choice(["", "sni.example", "s%2Ee.example"]))
    if r.random() < 0.4:
        add("peer", r.choice(["", "peer.example"]))
    if r.random() < 0.6:
        add("fp", r.choice(FPS))
    if r.random() < 0.6:
        add("alpn", r.choice(ALPNS))
    if r.random() < 0.6:
        add("path", r.choice(PATHS))
    if r.random() < 0.5:
        add("host", r.choice(["", "cdn.example", "a.example,b.example", "a.example,"]))
    if r.random() < 0.2:
        parts.append("bare-flag")          # no '=', so parse_qs drops it
    if r.random() < 0.2:
        add("unknown", "ignored")
    r.shuffle(parts)
    return "&".join(parts)


def vmess_link(r: random.Random) -> str:
    import base64
    cfg = {}
    if r.random() < 0.9:
        cfg["ps"] = r.choice(NAMES + [None, 1234])
    if r.random() < 0.95:
        cfg["add"] = r.choice(["h1.example", "192.0.2.1", "", "cdn.example"])
    if r.random() < 0.95:
        cfg["id"] = r.choice(UUIDS + ["", None])
    cfg["port"] = r.choice(VMESS_PORTS)
    if r.random() < 0.8:
        cfg["aid"] = r.choice([0, "0", 2, "", None, "x"])
    if r.random() < 0.8:
        cfg["scy"] = r.choice(["auto", "aes-128-gcm", "", None])
    cfg["net"] = r.choice(VMESS_NETS)
    if r.random() < 0.7:
        cfg["host"] = r.choice(["", "cdn.example", "a.example,b.example"])
    if r.random() < 0.7:
        cfg["path"] = r.choice(["/", "/ray", "", "svc"])
    cfg["tls"] = r.choice(VMESS_TLS)
    if r.random() < 0.4:
        cfg["sni"] = r.choice(["", "sni.example"])
    if r.random() < 0.4:
        cfg["alpn"] = r.choice(ALPNS)
    if r.random() < 0.4:
        cfg["fp"] = r.choice(FPS)
    body = json.dumps(cfg, ensure_ascii=False).encode("utf-8")
    enc = base64.b64encode(body).decode()
    roll = r.random()
    if roll < 0.08:                     # url-safe alphabet, unpadded
        enc = enc.replace("+", "-").replace("/", "_").rstrip("=")
    elif roll < 0.14:                   # truncated — a padding error either way
        enc = enc[: max(1, len(enc) - r.randint(1, 3))]
    elif roll < 0.18:                   # not base64-JSON at all
        enc = r.choice(["!!!!", "bm90IGpzb24=", "WzEsIDIsIDNd", "IjQ0MyI=", ""])
    return "vmess://" + enc


def link(r: random.Random) -> str:
    proto = r.choice(["vless", "vless", "vmess", "anytls", "hysteria2", "hy2",
                      "ss", "trojan"])
    if proto == "vmess":
        return vmess_link(r)
    if proto in ("ss", "trojan"):
        return f"{proto}://{r.choice(UUIDS)}@h1.example:443#unsupported"
    user, host, port = r.choice(USERS), r.choice(HOSTS), r.choice(PORTS)
    s = f"{proto}://{user}@{host}{port}"
    if r.random() < 0.85:
        s += "?" + q(r, proto)
    if r.random() < 0.8:
        s += "#" + r.choice(NAMES)
    return s


def junk(r: random.Random) -> str:
    return r.choice(["", "   ", "# a comment", "REMARKS=free plan",
                     "STATUS=expires 2099-01-01", "not a link at all",
                     "ss://x@h1.example:443", "\tvless://"])


def outbound(r: random.Random) -> dict:
    o: dict = {}
    if r.random() < 0.95:
        o["type"] = r.choice(["vless", "vmess", "anytls", "hysteria2"])
    if r.random() < 0.85:
        o["tag"] = r.choice(NAMES + ["escape", "auto", ""])
    o["server"] = r.choice(["h1.example", "h2.example", "192.0.2.1"])
    o["server_port"] = r.choice([443, 8443, "443"])
    if r.random() < 0.7:
        o["uuid"] = r.choice(UUIDS + [""])
    else:
        o["password"] = r.choice(["pw-1", "pw-2", ""])
    if r.random() < 0.25:
        # Unknown keys must survive a round trip in their original order.
        o["tls"] = {"enabled": True, "server_name": "s.example"}
        o["_source"] = "shadowrocket"
    return o


# `str.splitlines()` breaks on far more than \n, and both implementations must
# agree on all of it — a subscription pasted out of a browser can carry U+2028.
SEPS = ["\n", "\r\n", "\r", "\x0b", "\x0c", "\x1c", "\x85", " "]


def sub_body(r: random.Random) -> str:
    """A subscription body: the plain-text form, the base64 form, or neither."""
    lines = [link(r) if r.random() < 0.8 else junk(r) for _ in range(r.randint(1, 6))]
    text = r.choice(SEPS).join(lines) + r.choice(["", "\n"])
    roll = r.random()
    if roll < 0.45:
        return text
    if roll < 0.85:
        import base64
        enc = base64.b64encode(text.encode("utf-8")).decode()
        if r.random() < 0.3:                       # url-safe, unpadded
            enc = enc.replace("+", "-").replace("/", "_").rstrip("=")
        return r.choice(["", "  ", "\n"]) + enc    # leading space must be stripped
    # Bodies that yield no links at all: base64 of prose, and not-base64.
    return r.choice(["bm90aGluZyBoZXJl", "!!!!", "clash: yaml: here", "", "%%%"])


def case(r: random.Random, d: str) -> None:
    argv: list[str] = []
    stdin = ""
    roll = r.random()
    if roll < 0.36:                               # one link, one outbound
        argv = [link(r)]
        if r.random() < 0.4:
            argv += ["--tag", r.choice(["escape", "JP", "a b", "香港"])]
    elif roll < 0.66:                             # --multi over a mixed list
        argv = ["--multi"]
        lines = []
        for _ in range(r.randint(1, 8)):
            lines.append(link(r) if r.random() < 0.75 else junk(r))
        if r.random() < 0.25:
            # A deliberate name collision whose FIRST member cannot be split at
            # all. Whether a failing link reserves its tag is invisible unless
            # something else wants that tag — `urlsplit` raising inside
            # `_tag_for` leaves it free, a missing password does not.
            nm = r.choice(["JP", "dup", "escape", ""])
            bad = r.choice(["[bad]", "[1.2.3.4]", "h.example℀", "[2001:db8::1", "@h1.example"])
            lines[0:0] = [f"vless://{UUIDS[0]}@{bad}:443#{nm}",
                          f"hysteria2://pw-1@h1.example:443#{nm}"]
        stdin = r.choice(SEPS).join(lines) + r.choice(["\n", "", "\r\n"])
    elif roll < 0.82:
        # --sub against a file:// URL. urlopen and curl both speak it, so the
        # half of fetch_subscription that decides base64-vs-plaintext gets real
        # coverage without a network — and that half runs for every
        # subscription on every `up`, where mis-reading it drops every server.
        # The file always exists: a missing one is a urlopen traceback, which is
        # not a behaviour worth pinning.
        body = os.path.join(d, "body")
        with open(body, "w", encoding="utf-8") as fh:
            fh.write(sub_body(r))
        argv = ["--sub", "file://" + body]
    elif roll < 0.96:                             # --combine over an array
        argv = ["--combine"]
        outs = [outbound(r) for _ in range(r.randint(0, 6))]
        if outs and r.random() < 0.5:             # force a duplicate pair
            outs.append(dict(outs[0], tag="Alias"))
        stdin = json.dumps(outs, ensure_ascii=False)
        if r.random() < 0.06:
            # Only inputs whose failure is a first-token one. A well-formed but
            # non-object element (`[1, 2]`) makes the Python die inside
            # `key_of` with an AttributeError traceback; the Rust reports it
            # cleanly instead, which is the better behaviour and is pinned by a
            # unit test rather than by this gate.
            stdin = r.choice(["", "not json"])
    else:                                         # argument errors
        argv = r.choice([[], ["--tag"], ["--nope"], ["a.link", "b.link"]])
    with open(os.path.join(d, "argv"), "w", encoding="utf-8") as fh:
        fh.write("".join(a + "\n" for a in argv))
    with open(os.path.join(d, "stdin"), "w", encoding="utf-8") as fh:
        fh.write(stdin)


def main() -> int:
    out = sys.argv[1]
    count = int(sys.argv[2]) if len(sys.argv) > 2 else 400
    r = random.Random(20260809)      # seeded: a failing case must be reproducible
    for i in range(count):
        d = os.path.join(out, f"case{i:04d}")
        os.makedirs(d, exist_ok=True)
        case(r, d)
    print(count)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
