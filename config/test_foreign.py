#!/usr/bin/env python3
"""Smoke tests for foreign-import.py's Clash-proxy → share-link conversion.

Run:  python3 config/test_foreign.py   (prints "ok" and exits 0 on success)
"""

from __future__ import annotations

import importlib.util
import pathlib

_here = pathlib.Path(__file__).resolve().parent


def _load(name, filename):
    spec = importlib.util.spec_from_file_location(name, _here / filename)
    assert spec and spec.loader
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


fi = _load("foreign_import", "foreign-import.py")
vp = _load("vparse", "vless-parse.py")


def eq(got, want, msg):
    if got != want:
        raise AssertionError(f"{msg}: got {got!r}, want {want!r}")


def test_vless_reality_roundtrips():
    proxy = {
        "name": "Home",
        "type": "vless",
        "server": "1.2.3.4",
        "port": 8443,
        "uuid": "01779e49-274c-4237-a4a6-f16f91b7850e",
        "network": "tcp",
        "tls": True,
        "flow": "xtls-rprx-vision",
        "servername": "www.microsoft.com",
        "client-fingerprint": "chrome",
        "reality-opts": {"public-key": "PBK123", "short-id": "ab"},
    }
    link = fi.clash_proxy_to_link(proxy)
    assert link and link.startswith("vless://"), link
    # …and it parses back into a valid-looking sing-box VLESS+Reality outbound.
    o = vp.parse_link(link, "Home")
    eq(o["type"], "vless", "type")
    eq(o["server"], "1.2.3.4", "server")
    eq(o["server_port"], 8443, "port")
    eq(o["flow"], "xtls-rprx-vision", "flow")
    eq(o["tls"]["server_name"], "www.microsoft.com", "sni")
    eq(o["tls"]["reality"]["public_key"], "PBK123", "reality pbk")
    eq(o["tls"]["reality"]["short_id"], "ab", "reality sid")


def test_vmess_ws_roundtrips():
    proxy = {
        "name": "V",
        "type": "vmess",
        "server": "h.example",
        "port": 443,
        "uuid": "u-1",
        "alterId": 0,
        "cipher": "auto",
        "network": "ws",
        "tls": True,
        "servername": "h.example",
        "ws-opts": {"path": "/p", "headers": {"Host": "h.example"}},
    }
    o = vp.parse_link(fi.clash_proxy_to_link(proxy), "V")
    eq(o["type"], "vmess", "type")
    eq(o["transport"]["type"], "ws", "ws transport")
    eq(o["transport"]["path"], "/p", "ws path")
    eq(o["tls"]["server_name"], "h.example", "sni")


def test_unsupported_type_is_none():
    for t in ("trojan", "ss", "tuic", "wireguard"):
        eq(
            fi.clash_proxy_to_link({"type": t, "name": "x", "server": "h", "port": 1}),
            None,
            f"{t} unsupported",
        )


def test_skipped_counts_unsupported():
    from collections import Counter

    skipped: Counter = Counter()
    proxies = [
        {"type": "vless", "name": "a", "server": "h", "port": 1, "uuid": "u"},
        {"type": "trojan", "name": "b", "server": "h", "port": 2, "password": "p"},
        {"type": "ss", "name": "c", "server": "h", "port": 3},
    ]
    links = fi._links_from_clash_proxies(proxies, skipped)
    eq(len(links), 1, "one supported link")
    eq(dict(skipped), {"trojan": 1, "ss": 1}, "skipped counts")


def test_build_review_skips_existing_pool():
    # A candidate whose endpoint is already in the pool (under a different name) is
    # dropped from the review with a duplicate count — not re-imported.
    proxy = {
        "type": "vless",
        "name": "Xuhui",
        "server": "1.2.3.4",
        "port": 8443,
        "uuid": "u",
    }
    link = fi.clash_proxy_to_link(proxy)
    parsed = vp.parse_link(link, "Xuhui")
    existing = {vp.key_of(parsed): "Ds415"}  # same endpoint, different name
    from collections import Counter

    review = fi.build_review([link], [], Counter(), existing)
    eq(review["servers"], [], "existing server is skipped")
    eq(review["skipped"].get("duplicate"), 1, "counted as a duplicate")


def test_build_review_keeps_new_server():
    proxy = {
        "type": "vless",
        "name": "New",
        "server": "9.9.9.9",
        "port": 443,
        "uuid": "n",
    }
    link = fi.clash_proxy_to_link(proxy)
    from collections import Counter

    review = fi.build_review([link], [], Counter(), existing={})
    eq([s["tag"] for s in review["servers"]], ["New"], "a genuinely new server is kept")


def main() -> int:
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            fn()
    print("ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
