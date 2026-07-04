#!/usr/bin/env python3
"""Smoke tests for vless-parse.py — stdlib only, no framework.

Run:  python3 config/test_parse.py   (prints "ok" and exits 0 on success)
"""

from __future__ import annotations

import importlib.util
import pathlib

_here = pathlib.Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("vparse", _here / "vless-parse.py")
assert _spec and _spec.loader
vp = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(vp)


def eq(got, want, msg):
    if got != want:
        raise AssertionError(f"{msg}: got {got!r}, want {want!r}")


def test_basic():
    o = vp.parse_hysteria2(
        "hysteria2://pw@h.example:443?insecure=0&sni=s.example&upmbps=20&downmbps=80#JP",
        "JP",
    )
    eq(o["type"], "hysteria2", "type")
    eq(o["tag"], "JP", "tag")
    eq(o["server"], "h.example", "server")
    eq(o["server_port"], 443, "port")
    eq(o["password"], "pw", "password")
    eq(o["tls"]["enabled"], True, "tls.enabled")
    eq(o["tls"]["server_name"], "s.example", "tls.server_name")
    eq(o["tls"]["insecure"], False, "tls.insecure")
    eq(o["up_mbps"], 20, "up_mbps")
    eq(o["down_mbps"], 80, "down_mbps")
    assert "obfs" not in o, "no obfs expected"
    assert "alpn" not in o["tls"], "no alpn expected"


def test_insecure_and_default_port():
    o = vp.parse_hysteria2("hysteria2://pw@h.example?insecure=1", "t")
    eq(o["tls"]["insecure"], True, "insecure=1")
    eq(o["server_port"], 443, "default port")
    eq(o["tls"]["server_name"], "h.example", "sni falls back to host")


def test_obfs_and_alpn():
    o = vp.parse_hysteria2(
        "hysteria2://pw@h.example:8443?obfs=salamander&obfs-password=xyz&alpn=h3,h2#N",
        "N",
    )
    eq(o["obfs"], {"type": "salamander", "password": "xyz"}, "obfs")
    eq(o["tls"]["alpn"], ["h3", "h2"], "alpn")
    eq(o["server_port"], 8443, "port")


def test_alias():
    a = vp.parse_link("hy2://pw@h.example:443#X", "X")
    b = vp.parse_link("hysteria2://pw@h.example:443#X", "X")
    eq(a, b, "hy2:// == hysteria2://")


def test_parse_many_skips_header():
    out = vp.parse_many(
        [
            "REMARKS=foo",
            "hysteria2://pw@h1.example:443#a",
            "hysteria2://pw2@h2.example:443#b",
        ]
    )
    eq(len(out), 2, "two nodes, header skipped")
    tags = {o["tag"] for o in out}
    eq(len(tags), 2, "unique tags")


def test_missing_password_raises():
    try:
        vp.parse_hysteria2("hysteria2://@h.example:443", "t")
    except ValueError:
        return
    raise AssertionError("expected ValueError for missing password")


def main() -> int:
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            fn()
    print("ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
