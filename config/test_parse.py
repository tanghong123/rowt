#!/usr/bin/env python3
"""Smoke tests for vless-parse.py — stdlib only, no framework.

Run:  python3 config/test_parse.py   (prints "ok" and exits 0 on success)
"""

from __future__ import annotations

import base64
import importlib.util
import json
import pathlib

_here = pathlib.Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("vparse", _here / "vless-parse.py")
assert _spec and _spec.loader
vp = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(vp)


def _vmess(**cfg) -> str:
    return "vmess://" + base64.b64encode(json.dumps(cfg).encode()).decode()


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


def test_vmess_ws_tls():
    link = _vmess(
        ps="Tokyo",
        add="cdn.example.com",
        port="443",
        id="b831381d-6324-4d53-ad4f-8cda48b30811",
        aid="0",
        scy="auto",
        net="ws",
        host="cdn.example.com",
        path="/ray",
        tls="tls",
    )
    o = vp.parse_vmess(link, "Tokyo")
    eq(o["type"], "vmess", "type")
    eq(o["server"], "cdn.example.com", "server")
    eq(o["server_port"], 443, "port")
    eq(o["uuid"], "b831381d-6324-4d53-ad4f-8cda48b30811", "uuid")
    eq(o["security"], "auto", "security")
    eq(o["alter_id"], 0, "alter_id")
    eq(
        o["transport"],
        {"type": "ws", "path": "/ray", "headers": {"Host": "cdn.example.com"}},
        "ws transport",
    )
    eq(o["tls"]["server_name"], "cdn.example.com", "tls sni")


def test_vmess_tcp_plain():
    o = vp.parse_vmess(
        _vmess(ps="p", add="1.2.3.4", port="8080", id="u", net="tcp", tls=""), "t"
    )
    assert "transport" not in o, "tcp has no transport"
    assert "tls" not in o, "no tls when tls empty"
    eq(o["server_port"], 8080, "port")


def test_vmess_tag_from_ps():
    # In --multi the tag comes from the vmess `ps` field (not a #fragment).
    out = vp.parse_many(
        [_vmess(ps="My Node", add="h.example", port="443", id="u", net="tcp")]
    )
    eq(out[0]["tag"], "My-Node", "vmess tag from ps")


def test_vmess_kcp_rejected():
    try:
        vp.parse_vmess(_vmess(ps="p", add="h", port="443", id="u", net="kcp"), "t")
    except ValueError:
        return
    raise AssertionError("expected ValueError for kcp transport")


def test_vmess_dispatch():
    link = _vmess(ps="d", add="h.example", port="443", id="u", net="tcp")
    eq(vp.parse_link(link, "d"), vp.parse_vmess(link, "d"), "parse_link routes vmess")


def test_key_of_ignores_name():
    a = {"type": "vless", "tag": "Elm", "server": "h", "server_port": 443, "uuid": "u"}
    b = {
        "type": "vless",
        "tag": "Hong-Server",
        "server": "h",
        "server_port": 443,
        "uuid": "u",
    }
    eq(vp.key_of(a), vp.key_of(b), "same endpoint, different name → same key")


def test_combine_dedups_by_identity():
    outs = [
        {
            "type": "vless",
            "tag": "Ds415",
            "server": "h",
            "server_port": 443,
            "uuid": "u",
        },
        {
            "type": "vless",
            "tag": "Xuhui",
            "server": "h",
            "server_port": 443,
            "uuid": "u",
        },
        {
            "type": "vless",
            "tag": "Other",
            "server": "h2",
            "server_port": 443,
            "uuid": "u",
        },
    ]
    res = vp.combine(outs)
    eq(
        [o["tag"] for o in res],
        ["Ds415", "Other"],
        "keeps first of a dup pair, drops the alias",
    )


def _b64(s: str) -> str:
    return base64.b64encode(s.encode()).decode()


def _refused(link: str, why: str) -> None:
    try:
        vp.parse_link(link, "t")
    except ValueError as e:
        assert why in str(e), f"{link}: refused with {e!r}, not {why!r}"
        return
    raise AssertionError(f"{link}: parsed, should have been refused ({why})")


def test_ss_sip002_base64_userinfo_may_carry_a_slash():
    cred = _b64("aes-256-gcm:pa/ss?w")
    assert "/" in cred, "the case needs a '/' in the base64"
    o = vp.parse_link(f"ss://{cred}@H.Example:8388#N", "N")
    eq(o["type"], "shadowsocks", "type")
    eq(o["server"], "h.example", "host is lowercased, as urlsplit does")
    eq(o["server_port"], 8388, "port")
    eq(o["method"], "aes-256-gcm", "method")
    eq(o["password"], "pa/ss?w", "password")
    assert "plugin" not in o, "no plugin expected"


def test_ss_urlsafe_unpadded_with_an_obfs_plugin():
    cred = base64.urlsafe_b64encode(b"chacha20-ietf-poly1305:p@ss").decode()
    o = vp.parse_link(
        f"ss://{cred.rstrip('=')}@h.example:443/"
        "?plugin=simple-obfs%3Bobfs%3DHTTP%3Bobfs-host%3Dcdn.example#N",
        "N",
    )
    eq(o["password"], "p@ss", "password")
    eq(o["plugin"], "obfs-local", "simple-obfs is obfs-local")
    eq(o["plugin_opts"], "obfs=http;obfs-host=cdn.example", "mode lowercased")


def test_ss_sip022_plaintext_and_ipv6():
    key = base64.b64encode(bytes(range(32))).decode()
    o = vp.parse_link(
        f"ss://2022-blake3-aes-256-gcm:{key.replace('+', '%2B').replace('/', '%2F')}"
        "@[2001:DB8::1]:8388",
        "t",
    )
    eq(o["method"], "2022-blake3-aes-256-gcm", "method")
    eq(o["password"], key, "the percent-encoded key")
    eq(o["server"], "2001:db8::1", "brackets stripped")


def test_ss_legacy_all_base64():
    o = vp.parse_link("ss://" + _b64("aes-128-gcm:p:w@legacy.example:8389"), "t")
    eq((o["server"], o["server_port"]), ("legacy.example", 8389), "endpoint")
    eq(o["password"], "p:w", "the first ':' is the separator")


def test_ss_names_other_clients_use():
    o = vp.parse_link("ss://" + _b64("CHACHA20-POLY1305:pw") + "@h.example:1", "t")
    eq(o["method"], "chacha20-ietf-poly1305", "alias + case")
    o = vp.parse_link(
        "ss://" + _b64("aes-256-gcm:pw") + "@h.example:1"
        "?plugin=v2ray-plugin%3Bmode%3DWebSocket%3Btls%3Bpath%3D%2Fws",
        "t",
    )
    eq(o["plugin_opts"], "mode=websocket;tls;path=/ws", "v2ray mode lowercased")


def test_ss_refuses_what_sing_box_would():
    b = _b64
    k16 = base64.b64encode(bytes(16)).decode()
    k32 = base64.b64encode(bytes(32)).decode()
    _refused(f"ss://{b('rc4:pw')}@h.example:1", "method rc4 is not supported")
    _refused(f"ss://2022-blake3-aes-128-gcm:{k32}@h.example:1", "keys of 16 bytes")
    _refused("ss://2022-blake3-aes-256-gcm:notbase64!@h.example:1", "keys of 32")
    _refused(
        f"ss://2022-blake3-chacha20-poly1305:{k32}:{k32}@h.example:1", "single key"
    )
    _refused(f"ss://{b('aes-256-gcm:')}@h.example:1", "missing password")
    _refused(f"ss://{b('aes-256-gcm:pw')}@h.example", "missing a valid port")
    _refused(f"ss://{b('aes-256-gcm:pw')}@h.example:65536", "missing a valid port")
    _refused(f"ss://{b('aes-256-gcm')}@h.example:1", "missing method:password")
    _refused("ss://!!!!", "not method:password@host:port")
    base = f"ss://{b('aes-256-gcm:pw')}@h.example:1?plugin="
    _refused(base + "shadow-tls%3Bhost%3Dx", "plugin shadow-tls is not supported")
    _refused(base + "obfs-local%3Bobfs%3Dbogus", "obfs-local mode bogus")
    _refused(base + "v2ray-plugin%3Bmode%3Dquic", "mode quic needs tls")
    # And what it does take, so the refusals above are not refusing everything.
    vp.parse_link(f"ss://2022-blake3-aes-128-gcm:{k16}:{k16}@h.example:1", "t")
    vp.parse_link(base + "v2ray-plugin%3Bmode%3Dquic%3Btls", "t")


def test_trojan_is_tls_by_default_and_the_userinfo_is_the_password():
    o = vp.parse_link("trojan://p%40ss:w@T.example:443?allowInsecure=1#N", "N")
    eq(o["password"], "p@ss:w", "whole userinfo, decoded")
    eq(o["server"], "t.example", "server")
    eq(o["tls"]["server_name"], "t.example", "sni falls back to the host")
    eq(o["tls"]["insecure"], True, "allowInsecure")
    eq(o["tls"]["utls"], {"enabled": True, "fingerprint": "chrome"}, "utls")
    assert "transport" not in o, "tcp has no transport"


def test_trojan_security_none_reality_and_transports():
    o = vp.parse_link("trojan://pw@t.example:80?security=none", "t")
    assert "tls" not in o, "security=none drops tls"
    o = vp.parse_link(
        "trojan://pw@t.example?security=reality&pbk=K&sid=ab&sni=s.example"
        "&type=ws&path=%2Fws&host=cdn.example",
        "t",
    )
    eq(o["tls"]["reality"], {"enabled": True, "public_key": "K", "short_id": "ab"}, "")
    eq(o["tls"]["server_name"], "s.example", "sni")
    eq(
        o["transport"],
        {"type": "ws", "path": "/ws", "headers": {"Host": "cdn.example"}},
        "ws transport",
    )
    _refused("trojan://pw@t.example?security=reality", "missing pbk")
    _refused("trojan://t.example:443", "missing password or host")


def test_tuic_defaults_and_tuning():
    u = "2DD61D93-75D8-4DA4-AC0E-6AECE7EAC365"
    o = vp.parse_link(f"tuic://{u}:p%3Aw@u.example:8443#N", "N")
    eq((o["uuid"], o["password"]), (u, "p:w"), "uuid and password")
    eq(o["tls"]["alpn"], ["h3"], "h3 when the link names none")
    eq(o["tls"]["insecure"], False, "insecure")
    assert "congestion_control" not in o, "sing-box's default"
    o = vp.parse_link(
        f"tuic://{u}:pw@u.example?congestion_control=New-Reno&udp_relay_mode=QUIC"
        "&alpn=h3,spdy&allow_insecure=1",
        "t",
    )
    eq(o["congestion_control"], "new_reno", "normalized")
    eq(o["udp_relay_mode"], "quic", "relay mode")
    eq(o["tls"]["alpn"], ["h3", "spdy"], "alpn")
    eq(o["tls"]["insecure"], True, "allow_insecure")
    o = vp.parse_link(f"tuic://{u}:pw@u.example?congestion_control=vegas", "t")
    assert "congestion_control" not in o, "an unknown one is dropped, not fatal"
    vp.parse_link(f"tuic://{u.replace('-', '')}:pw@u.example", "t")
    _refused("tuic://not-a-uuid:pw@u.example", "malformed uuid")
    _refused(f"tuic://{u}@u.example", "missing uuid, password or host")


def test_parse_many_still_skips_what_rowt_does_not_speak():
    import contextlib
    import io

    err = io.StringIO()
    with contextlib.redirect_stderr(err):
        out = vp.parse_many(
            [
                "wireguard://k@w.example:51820#wg",
                "ss://" + _b64("aes-256-gcm:pw") + "@h.example:1#ss",
                "trojan://pw@t.example#tr",
            ]
        )
    eq([o["tag"] for o in out], ["ss", "tr"], "tags")
    assert "skipping unsupported link (wireguard://)" in err.getvalue(), err.getvalue()


def main() -> int:
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            fn()
    print("ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
