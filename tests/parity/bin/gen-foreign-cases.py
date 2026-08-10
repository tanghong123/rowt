#!/usr/bin/env python3
"""Generate the corpus for `parity foreign-diff`.

Unlike the other Python gates the input here is not a string — it is a client
config DIRECTORY, so each case is a whole fake `$HOME` with a Clash Verge /
FlClash profile tree or a V2Box SQLite store inside it, plus the `--existing`
pool files and the PATH the run should see.

    gen-foreign-cases.py <dir> [count]      # prints the number of cases

Each case is a directory:

    argv         one argument per line (may be empty)
    path_mode    which PATH the runner should use: real | none | fail | garbage
                 | empty — i.e. whether `yq` is there, missing, failing, or
                 answering with something that is not JSON
    home/        the fake HOME the importer looks under
    pool/        servers.json / manual.json / subs.txt, when the case uses them
    stdin        empty (the importer reads none), so the runner can redirect

What the corpus goes after, beyond "does a normal profile import":

  * the `profiles.yaml` branch vs the scan branch, which handle a `yq` problem
    completely differently — fatal with an index, `except Exception: continue`
    without one;
  * `sorted(root.rglob(...))`, which orders by PATH COMPONENT, so sibling dirs
    named `a` and `a-x` come out in the opposite order from a string sort;
  * `rglob` seeing dotfiles (a `glob.glob` would not), and a DIRECTORY whose own
    name ends in .yaml;
  * every SQLite storage class in ZURL/ZTYPE/ZSUBSCRIBE — a BLOB url reaches
    `bytes.startswith(str)` and a non-zero INTEGER one reaches `int.strip`,
    both of which end the run;
  * `reality-opts`/`ws-opts`/`grpc-opts` that are not mappings;
  * `str()` and truthiness over YAML scalars: `port: null` renders `None` while
    a missing port renders empty, `flow: false` survives the `not in (None, "")`
    filter as the text `False`.

Everything is generated from a fixed seed, so a failure replays. Hosts are
RFC 5737 / RFC 3849 documentation addresses and `example`-family domains; no
real endpoint, credential or client tree is ever read.
"""

from __future__ import annotations

import json
import os
import random
import sqlite3
import stat
import sys

RNG = random.Random(20260809)

CV_DIR = "Library/Application Support/io.github.clash-verge-rev.clash-verge-rev"
CV_ALT = "Library/Application Support/io.github.zzzgydi.clash-verge"
FC_DIR = "Library/Application Support/FlClash"
FC_ALT = ".config/FlClash"
V2_DB = "Library/Group Containers/group.hossin.asaadi.V2Box/DB.sqlite"

HOSTS = [
    "192.0.2.10",
    "198.51.100.7",
    "203.0.113.44",
    "node.example.com",
    "gate.example.net",
    "[2001:db8::1]",
    "2001:db8::2",
]
NAMES = [
    "Home",
    "HK 01",
    "东京/直连",
    "a=b&c",
    "",
    "escape",
    "node#1",
    "  padded  ",
    "sao-paulo",
    "​zero",
]
SNIS = ["www.example.com", "cdn.example.net", "", None]
PATHS = ["/", "/ws", "/a b", "/%2f", "/中文"]


def w(path: str, text: str) -> None:
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        f.write(text)


def yamlish(obj) -> str:
    """JSON is valid YAML, and yq reads it the same for both sides — which is
    what the gate needs. The genuinely YAML-shaped cases are literals below."""
    return json.dumps(obj, ensure_ascii=False, indent=1) + "\n"


def proxy(rng: random.Random) -> dict:
    t = rng.choice(
        ["vless", "vless", "vmess", "vmess", "anytls", "hysteria2", "trojan", "ss", "tuic", "", None]
    )
    p: dict = {"name": rng.choice(NAMES), "server": rng.choice(HOSTS)}
    if t is not None or rng.random() < 0.5:
        p["type"] = t
    port = rng.choice([443, 8443, "8443", 0, None, "abc", 65536, 1.5])
    if rng.random() < 0.9:
        p["port"] = port
    if rng.random() < 0.85:
        p["uuid"] = rng.choice(
            ["01779e49-274c-4237-a4a6-f16f91b7850e", "u-1", 7, "", None, "a/b c"]
        )
    if rng.random() < 0.4:
        p["password"] = rng.choice(["pw", "p+w/x", "", None, 42])
    if rng.random() < 0.2:
        p["auth"] = rng.choice(["auth1", ""])
    net = rng.choice(["tcp", "ws", "websocket", "grpc", "h2", "TCP", None])
    if net is not None or rng.random() < 0.5:
        p["network"] = net
    if rng.random() < 0.6:
        p["tls"] = rng.choice([True, False, None, "yes", 0])
    if rng.random() < 0.3:
        p["skip-cert-verify"] = rng.choice([True, False, 1])
    if rng.random() < 0.5:
        p["servername"] = rng.choice(SNIS)
    if rng.random() < 0.4:
        p["sni"] = rng.choice(SNIS)
    if rng.random() < 0.3:
        p["client-fingerprint"] = rng.choice(["chrome", "", None])
    if rng.random() < 0.3:
        p["flow"] = rng.choice(["xtls-rprx-vision", "", False, None])
    if rng.random() < 0.35:
        # A non-mapping here is an AttributeError that ends the whole import.
        p["reality-opts"] = rng.choice(
            [
                {"public-key": "PBK123", "short-id": "ab"},
                {"public-key": "", "short-id": None},
                {},
                "oops",
                7,
                [],
                None,
            ]
        )
    if rng.random() < 0.35:
        p["ws-opts"] = rng.choice(
            [
                {"path": rng.choice(PATHS), "headers": {"Host": "h.example.com"}},
                {"path": None, "headers": {"host": "lower.example.com"}},
                {"headers": "not-a-map"},
                {"headers": {}},
                "oops",
                {},
            ]
        )
    if rng.random() < 0.2:
        p["grpc-opts"] = rng.choice([{"grpc-service-name": "svc"}, {}, "oops", None])
    if rng.random() < 0.3:
        p[rng.choice(["alterId", "alter-id"])] = rng.choice([0, 1, "2", None, ""])
    if rng.random() < 0.25:
        p["cipher"] = rng.choice(["auto", "aes-128-gcm", "", None])
    if rng.random() < 0.2:
        p["obfs"] = rng.choice(["salamander", "", None])
        p["obfs-password"] = rng.choice(["op", "", None])
    return p


def proxies_doc(rng: random.Random) -> object:
    shape = rng.random()
    if shape < 0.72:
        return {"proxies": [proxy(rng) for _ in range(rng.randint(1, 4))]}
    if shape < 0.78:
        return {"proxies": []}
    if shape < 0.83:
        # Not a list: a mapping iterates its KEYS, a string its characters, and
        # neither is a dict — so both are zero links rather than an error.
        return {"proxies": rng.choice([{"a": 1}, "abc"])}
    if shape < 0.87:
        return {"proxies": 5}  # not iterable — TypeError
    if shape < 0.9:
        return {"proxies": [proxy(rng), "a string in the list", None, 5]}
    if shape < 0.94:
        return {"rules": ["DOMAIN,example.com,DIRECT"]}
    if shape < 0.97:
        return [proxy(rng)]  # a list at the top level has no .get
    return None


SUB_URLS = [
    "https://sub.example.com/x?token=abc",
    "https://Sub.Example.com/x/?name=Display&token=abc",
    "http://sub.example.net/p",
    "https://sub.example.com/x?token=abc&name=Other",
    "https://sub.example.com/X?token=abc",
    "not-a-url",
    "",
]


def index_doc(rng: random.Random) -> object:
    shape = rng.random()
    if shape < 0.08:
        return rng.choice([None, {}, [], "scalar", 5, {"items": None}, {"items": []}])
    if shape < 0.13:
        return ["a", "b"]  # .get on a list
    if shape < 0.17:
        return {"items": rng.choice([5, "abc", {"k": {"type": "local", "file": "a.yaml"}}])}
    items = []
    for i in range(rng.randint(1, 4)):
        r = rng.random()
        if r < 0.35:
            it = {"type": "remote", "url": rng.choice(SUB_URLS)}
            if rng.random() < 0.5:
                it["name"] = rng.choice(["ByWave", "", None])
            if rng.random() < 0.6:
                it["uid"] = f"uid{i}"
            items.append(it)
        elif r < 0.75:
            items.append(
                {
                    "type": "local",
                    "file": rng.choice(["a.yaml", "b.yaml", "missing.yaml", "sub/c.yaml"]),
                }
            )
        elif r < 0.8:
            items.append({"type": "local", "file": rng.choice([5, None, ["a"], True])})
        elif r < 0.85:
            items.append({"type": rng.choice(["Remote", "LOCAL", "merge", ""]), "url": "https://s/x"})
        else:
            items.append(rng.choice(["a string item", 5, None, []]))
    return {"items": items}


BAD_YAML = [
    "items:\n\t- tab-indented\n",
    "a: [1, 2\n",
    "%YAML 1.2\n---\n{a: 1}\n",
]


def make_clash_tree(rng: random.Random, home: str, base: str, indexed: bool) -> None:
    root = os.path.join(home, base)
    if indexed:
        doc = index_doc(rng)
        if rng.random() < 0.08:
            w(os.path.join(root, "profiles.yaml"), rng.choice(BAD_YAML))
        else:
            w(os.path.join(root, "profiles.yaml"), yamlish(doc))
        os.makedirs(os.path.join(root, "profiles"), exist_ok=True)
        for f in ["a.yaml", "b.yaml", "sub/c.yaml"]:
            if rng.random() < 0.75:
                w(os.path.join(root, "profiles", f), yamlish(proxies_doc(rng)))
        return
    # No index: the scan branch. The directory names are chosen so that a sort
    # by string and a sort by path component disagree.
    names = [
        "a/b.yaml",
        "a-x/c.yaml",
        "a.d/e.yaml",
        ".hidden.yaml",
        ".hid/deep.yml",
        "plain.yml",
        "nested/deeper/z.yaml",
        "not-yaml.txt",
    ]
    for f in names:
        if rng.random() < 0.7:
            body = yamlish(proxies_doc(rng)) if f.endswith((".yaml", ".yml")) else "junk\n"
            w(os.path.join(root, f), body)
    if rng.random() < 0.25:
        # A DIRECTORY whose own name matches the glob: rglob yields it, and yq
        # then fails on it — swallowed here, but it still costs a subprocess.
        os.makedirs(os.path.join(root, "adir.yaml"), exist_ok=True)
    if rng.random() < 0.2:
        os.makedirs(root, exist_ok=True)  # an empty dir still "exists"


SQL_VALUES = [
    "vless://u@192.0.2.10:443?type=tcp&security=none#N",
    "vmess://eyJ2IjogIjIiLCAicHMiOiAiViIsICJhZGQiOiAiMTkyLjAuMi4xMCIsICJwb3J0IjogIjQ0MyIsICJpZCI6ICJ1IiwgImFpZCI6ICIwIiwgInNjeSI6ICJhdXRvIiwgIm5ldCI6ICJ0Y3AifQ==",
    "anytls://pw@198.51.100.7:8443?sni=www.example.com#A",
    "hysteria2://pw@203.0.113.44:443?sni=www.example.com",
    "hy2://pw@203.0.113.44:443",
    "trojan://pw@192.0.2.10:443",
    "ss://YWVzOnB3@192.0.2.10:8388",
    "  vless://u@192.0.2.10:443#padded  ",
    "not a link",
    "",
]


def make_v2box_db(rng: random.Random, path: str) -> None:
    os.makedirs(os.path.dirname(path), exist_ok=True)
    shape = rng.random()
    if shape < 0.06:
        w(path, "this is not a database\n")
        return
    con = sqlite3.connect(path)
    if shape < 0.12:
        con.execute("CREATE TABLE ZOTHER (A, B)")
        con.commit()
        con.close()
        return
    con.execute("CREATE TABLE ZCDV2RAYITEM (ZTYPE, ZURL, ZSUBSCRIBE)")
    for _ in range(rng.randint(0, 5)):
        ztype = rng.choice(["VLESS", "AnyTLS", "Subscribe", 7, 0, 1.5, None, b"B", "", "SS"])
        r = rng.random()
        if r < 0.72:
            zurl = rng.choice(SQL_VALUES)
        elif r < 0.8:
            zurl = None
        elif r < 0.86:
            zurl = rng.choice([0, 0.0, ""])  # falsy: becomes "" without a strip
        elif r < 0.93:
            zurl = rng.choice([5, 2.5])  # truthy non-str: AttributeError
        else:
            zurl = rng.choice([b"vless://u@192.0.2.10:443", b"junk"])  # bytes: TypeError
        zsub = rng.choice(
            [None, "", " https://sub.example.com/x?token=abc ", "https://sub.example.com/x", "nope", 0]
        )
        con.execute("INSERT INTO ZCDV2RAYITEM VALUES (?,?,?)", (ztype, zurl, zsub))
    con.commit()
    con.close()


POOL_SERVERS = [
    {"type": "vless", "tag": "Ds415", "server": "192.0.2.10", "server_port": 443, "uuid": "u"},
    {"type": "anytls", "tag": "Ds416", "server": "198.51.100.7", "server_port": 8443, "password": "pw"},
]


def make_pool(rng: random.Random, d: str, argv: list[str]) -> None:
    if rng.random() < 0.45:
        path = os.path.join(d, "pool", "servers.json")
        shape = rng.random()
        if shape < 0.15:
            w(path, "{not json")
        elif shape < 0.25:
            w(path, json.dumps({"servers": POOL_SERVERS}))  # not a list: ignored
        else:
            w(path, json.dumps(POOL_SERVERS[: rng.randint(1, 2)]))
        argv += ["--existing", "pool/servers.json"]
    if rng.random() < 0.2:
        argv += ["--existing", "pool/does-not-exist.json"]
    if rng.random() < 0.4:
        path = os.path.join(d, "pool", "subs.txt")
        lines = ["# a comment", "https://sub.example.com/x?token=abc"]
        if rng.random() < 0.5:
            lines.append("https://sub.example.net/p")
        if rng.random() < 0.3:
            lines.append("")
        w(path, "\n".join(lines) + "\n")
        argv += ["--existing-subs", "pool/subs.txt"]
    if rng.random() < 0.15:
        argv += ["--existing-subs", "pool/nope.txt"]


def make_case(rng: random.Random, d: str, i: int) -> str:
    """Build one case; returns its path_mode."""
    home = os.path.join(d, "home")
    os.makedirs(home, exist_ok=True)
    argv: list[str] = []
    mode = "real"

    kind = rng.random()
    if kind < 0.06:
        # Pure argparse surface — no tree needed.
        argv = rng.choice(
            [
                ["--help"],
                [],
                ["--from", "nope"],
                ["--from"],
                ["--zzz"],
                ["--det"],
                ["--exist", "x"],
                ["--from=v2box", "--path", "nowhere/db.sqlite"],
                ["positional"],
                ["--detect", "--from", "v2box"],
            ]
        )
    elif kind < 0.2:
        argv = ["--detect"]
        if rng.random() < 0.6:
            make_clash_tree(rng, home, rng.choice([CV_DIR, CV_ALT]), rng.random() < 0.6)
        if rng.random() < 0.5:
            make_clash_tree(rng, home, rng.choice([FC_DIR, FC_ALT]), False)
        if rng.random() < 0.5:
            make_v2box_db(rng, os.path.join(home, V2_DB))
        mode = rng.choice(["real", "real", "none"])
    elif kind < 0.5:
        argv = ["--from", "clash-verge"]
        indexed = rng.random() < 0.65
        if rng.random() < 0.9:
            make_clash_tree(rng, home, rng.choice([CV_DIR, CV_DIR, CV_ALT]), indexed)
        if rng.random() < 0.15:
            argv += ["--path", rng.choice(["home/" + CV_DIR, "home/" + CV_DIR + "/", "nope/"])]
        mode = rng.choice(["real", "real", "real", "none", "fail", "garbage", "empty"])
    elif kind < 0.7:
        argv = ["--from", "flclash"]
        if rng.random() < 0.9:
            make_clash_tree(rng, home, rng.choice([FC_DIR, FC_ALT]), rng.random() < 0.2)
        mode = rng.choice(["real", "real", "real", "none", "fail", "garbage", "empty"])
    else:
        argv = ["--from", "v2box"]
        if rng.random() < 0.9:
            make_v2box_db(rng, os.path.join(home, V2_DB))
        if rng.random() < 0.12:
            argv += ["--path", "home/" + V2_DB]

    if argv and argv[0] not in ("--help", "positional", "--zzz"):
        make_pool(rng, d, argv)

    w(os.path.join(d, "argv"), "".join(a + "\n" for a in argv))
    w(os.path.join(d, "path_mode"), mode + "\n")
    w(os.path.join(d, "stdin"), "")
    return mode


SHIMS = {
    # Exit non-zero: `_yq_json` raises RuntimeError, which the index branch lets
    # out of main as `error: …` and the scan branch swallows.
    "fail": '#!/bin/sh\necho "yq: bad expression" >&2\nexit 3\n',
    # Exit zero with something that is not JSON: a JSONDecodeError, which is NOT
    # a RuntimeError and so is a traceback in the index branch.
    "garbage": "#!/bin/sh\nprintf 'not json at all\\n'\nexit 0\n",
    # Exit zero with nothing: `json.loads(\"null\")` — a None document.
    "empty": "#!/bin/sh\nexit 0\n",
}


def make_path_dirs(work: str) -> None:
    """One PATH directory per yq behaviour. Each still has python3 on it, since
    the runner invokes the Python side by name."""
    for mode in ["none", *SHIMS]:
        d = os.path.join(work, f"bin-{mode}")
        os.makedirs(d, exist_ok=True)
        link = os.path.join(d, "python3")
        if not os.path.lexists(link):
            os.symlink(sys.executable, link)
        if mode in SHIMS:
            p = os.path.join(d, "yq")
            w(p, SHIMS[mode])
            os.chmod(p, os.stat(p).st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def main() -> int:
    out = sys.argv[1]
    count = int(sys.argv[2]) if len(sys.argv) > 2 else 300
    os.makedirs(out, exist_ok=True)
    make_path_dirs(out)
    for i in range(count):
        d = os.path.join(out, f"case{i:04d}")
        os.makedirs(d, exist_ok=True)
        make_case(RNG, d, i)
    print(count)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
