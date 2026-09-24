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


def test_ss_obfs_roundtrips():
    proxy = {
        "name": "S",
        "type": "ss",
        "server": "h.example",
        "port": 8388,
        "cipher": "aes-256-gcm",
        "password": "p@ss:w/rd",
        "plugin": "obfs",
        "plugin-opts": {"mode": "tls", "host": "cdn.example"},
    }
    o = vp.parse_link(fi.clash_proxy_to_link(proxy), "S")
    eq(o["type"], "shadowsocks", "type")
    eq((o["server"], o["server_port"]), ("h.example", 8388), "endpoint")
    eq((o["method"], o["password"]), ("aes-256-gcm", "p@ss:w/rd"), "credentials")
    eq(o["plugin"], "obfs-local", "plugin")
    eq(o["plugin_opts"], "obfs=tls;obfs-host=cdn.example", "plugin_opts")


def test_ss_v2ray_plugin_roundtrips_and_shadow_tls_is_refused_by_name():
    proxy = {
        "name": "V",
        "type": "ss",
        "server": "h.example",
        "port": 443,
        "cipher": "chacha20-ietf-poly1305",
        "password": "pw",
        "plugin": "v2ray-plugin",
        "plugin-opts": {"mode": "websocket", "tls": True, "path": "/ws"},
    }
    o = vp.parse_link(fi.clash_proxy_to_link(proxy), "V")
    eq(o["plugin_opts"], "mode=websocket;tls;path=/ws", "v2ray opts")
    proxy["plugin"] = "shadow-tls"
    try:
        vp.parse_link(fi.clash_proxy_to_link(proxy), "V")
    except ValueError as e:
        assert "shadow-tls" in str(e), e
    else:
        raise AssertionError("shadow-tls should be refused")


def test_trojan_ws_roundtrips():
    proxy = {
        "name": "T",
        "type": "trojan",
        "server": "t.example",
        "port": 443,
        "password": "p/w",
        "sni": "s.example",
        "skip-cert-verify": True,
        "network": "ws",
        "ws-opts": {"path": "/tr", "headers": {"Host": "cdn.example"}},
    }
    o = vp.parse_link(fi.clash_proxy_to_link(proxy), "T")
    eq(o["type"], "trojan", "type")
    eq(o["password"], "p/w", "password")
    eq(o["tls"]["server_name"], "s.example", "sni")
    eq(o["tls"]["insecure"], True, "skip-cert-verify")
    eq(o["transport"]["path"], "/tr", "ws path")


def test_tuic_roundtrips():
    proxy = {
        "name": "U",
        "type": "tuic",
        "server": "u.example",
        "port": 443,
        "uuid": "2DD61D93-75D8-4DA4-AC0E-6AECE7EAC365",
        "password": "pw",
        "congestion-controller": "bbr",
        "udp-relay-mode": "native",
    }
    o = vp.parse_link(fi.clash_proxy_to_link(proxy), "U")
    eq(o["type"], "tuic", "type")
    eq(o["congestion_control"], "bbr", "cc")
    eq(o["udp_relay_mode"], "native", "relay")
    eq(o["tls"]["alpn"], ["h3"], "alpn")


def test_unsupported_type_is_none():
    for t in ("ssr", "wireguard", "snell", "hysteria"):
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
        {"type": "wireguard", "name": "c", "server": "h", "port": 3},
        {"type": "ssr", "name": "d", "server": "h", "port": 4},
    ]
    links = fi._links_from_clash_proxies(proxies, skipped)
    eq(len(links), 2, "vless and trojan become links")
    eq(dict(skipped), {"wireguard": 1, "ssr": 1}, "skipped counts")


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


def test_norm_sub_ignores_name_param_and_slash():
    a = fi.norm_sub("https://Sub.Example.com/x/?name=ByWave&token=abc")
    b = fi.norm_sub("https://sub.example.com/x?token=abc")
    eq(a, b, "name= param and trailing slash ignored, host lowercased")


def test_build_review_skips_existing_subscription():
    from collections import Counter

    subs = [
        {"url": "https://sub.example.com/x?token=abc&name=ByWave", "name": "ByWave"}
    ]
    existing = {fi.norm_sub("https://sub.example.com/x?token=abc")}
    review = fi.build_review([], subs, Counter(), {}, existing)
    eq(review["subscriptions"], [], "already-saved subscription is skipped")
    eq(review["skipped"].get("duplicate-subscription"), 1, "counted as a duplicate sub")


def test_build_review_dedupes_subs_within_source():
    from collections import Counter

    subs = [
        {"url": "https://s/x?name=A", "name": "A"},
        {
            "url": "https://s/x?name=B",
            "name": "B",
        },  # same endpoint, different display name
    ]
    review = fi.build_review([], subs, Counter(), {}, set())
    eq(len(review["subscriptions"]), 1, "within-source duplicate subs collapsed")


def main() -> int:
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            fn()
    print("ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
