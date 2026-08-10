#!/usr/bin/env python3
"""Generate the corpus for `parity sr-diff`.

    gen-sr-cases.py <dir> [count]      # prints the number of cases

Each case is a fake `$HOME` holding a Shadowrocket install: a binary
NSKeyedArchiver server store, `.conf` rule files in the three places the
importer globs for them, or neither.

    argv     one argument per line (may be empty)
    home/    the fake HOME
    stdin    empty

What it goes after:

  * the archive walk — `$objects` entries that are and are not dicts, `type`
    and `host` present or missing, `CF$UID` values that point at strings, at
    `NSString` wrappers, at other dicts (which stay dicts, since the
    dereference is ONE level), and off the end of the array;
  * the coercions on the way out: `int(port or 443)` over ints, floats,
    numeric strings, junk strings, bools and `None`; `str()` over bytes, reals,
    dates and UIDs; `bool("0")`, which is True;
  * that the VLESS id comes from `password` and only falls back to `uuid`;
  * `_sanitize`'s collapse of a name to `[A-Za-z0-9._-]`, its reserved-name
    suffix, and the fact that a server which fails to convert has ALREADY
    taken its tag;
  * `_find_conf`'s three patterns in order, the newest-by-mtime pick within
    one, and `glob`'s refusal to match a leading dot — the opposite of the
    `pathlib` glob `foreign-import.py` uses;
  * `_parse_rules` over `[Rule]` sections with CRLF, bare CR, undecodable
    bytes, and rules whose action is not the third field;
  * plists that are truncated, byte-flipped or simply not plists, where the
    invariant is that both sides agree on which ones are readable.

XML plists are deliberately absent: the Rust reads only `bplist00`, which is
what Shadowrocket writes and all `bin/rowt` can ever produce, and the gap is
named in `rowt-core::bplist` rather than papered over with a corpus that avoids
noticing it.

Seeded, so a failure replays. Hosts are RFC 5737 / RFC 3849 documentation
addresses; no real store, credential or rule file is ever read.
"""

from __future__ import annotations

import datetime
import os
import plistlib
import random
import sys
from plistlib import UID

RNG = random.Random(20260810)

GC = "Library/Group Containers/group.com.liguangming.Shadowrocket"
ICLOUD = "Library/Mobile Documents/iCloud~com~liguangming~Shadowrocket/Documents"
CLOUDDOCS = "Library/Mobile Documents/com~apple~CloudDocs"

HOSTS = ["192.0.2.10", "198.51.100.7", "203.0.113.44", "node.example.com", "2001:db8::1"]
SUB_URLS = [
    "https://sub.example.com/x?token=abc",
    "http://sub.example.net/p",
    "https://sub.example.com/x#名前",
    "ftp://sub.example.com/x",
    "httpshhh",
]
TITLES = [
    "Home",
    "HK 01",
    "东京 节点",
    "escape",
    "direct",
    "",
    "--dashes--",
    "a.b_c-d",
    "  padded  ",
    "dup",
    "dup",
]
# Weighted toward the three the importer actually converts, so the Reality,
# flow and transport branches are reached often rather than eventually.
TYPES = [
    "VLESS",
    "VLESS",
    "VLESS",
    "VLESS",
    "AnyTLS",
    "AnyTLS",
    "Subscribe",
    "Subscribe",
    "Shadowsocks",
    "Trojan",
    "",
    None,
    "vless",
]
PORTS = [443, 8443, 0, "8443", " 8443 ", "abc", 1.5, True, None, -1, 65536]
OBFS = ["none", "ws", "WebSocket", "grpc", "", None, "tls"]


def w(path: str, data: bytes | str) -> None:
    os.makedirs(os.path.dirname(path), exist_ok=True)
    mode = "wb" if isinstance(data, bytes) else "w"
    with open(path, mode) as f:
        f.write(data)


def scalar(rng: random.Random):
    """A leaf the way it can appear in a real archive — including the ones whose
    `str()` is not obvious."""
    return rng.choice(
        [
            "text",
            "",
            7,
            0,
            -3,
            1.5,
            2.0,
            True,
            False,
            None,
            b"bytes",
            b"",
            datetime.datetime(2026, 8, 9, 12, 0, 0),
        ]
    )


def server_entry(rng: random.Random, objs: list) -> dict:
    """One `$objects` entry with a `type` and a `host`, whose values are a mix of
    literals and UIDs into the array being built."""

    def ref(v):
        # Half the time via a CF$UID, as NSKeyedArchiver actually stores it.
        if rng.random() < 0.5:
            objs.append(v)
            return UID(len(objs) - 1)
        return v

    typ = rng.choice(TYPES)
    e: dict = {}
    e["type"] = ref(typ)
    # A Subscribe row keeps its URL in the same `host` slot a server does, and
    # only a value that starts with "http" is taken.
    hosts = SUB_URLS if typ == "Subscribe" else HOSTS
    e["host"] = ref(rng.choice(hosts + ["", None, 5]))
    if rng.random() < 0.8:
        e["port"] = ref(rng.choice(PORTS))
    if rng.random() < 0.8:
        e["password"] = ref(rng.choice(["pw-real", "pw-real", "pw-real", "", None, 42, b"pw"]))
    if rng.random() < 0.6:
        e["uuid"] = ref(rng.choice(["shadowrockets-own-id", "", None]))
    if rng.random() < 0.7:
        e["title"] = ref(rng.choice(TITLES + [None, 5]))
    if rng.random() < 0.45:
        e["xtls"] = ref(rng.choice([2, "2", 2.0, 0, None, "1"]))
    if rng.random() < 0.4:
        e["peer"] = ref(rng.choice(["www.example.com", "", None]))
    if rng.random() < 0.5:
        e["publicKey"] = ref(rng.choice(["PBK123", "PBK123", "", None]))
    if rng.random() < 0.3:
        e["shortId"] = ref(rng.choice(["ab", "", None, 12]))
    if rng.random() < 0.4:
        e["tls"] = ref(rng.choice([True, False, 1, 0, "0", None]))
    if rng.random() < 0.5:
        e["obfs"] = ref(rng.choice(OBFS))
    if rng.random() < 0.4:
        e["obfsParam"] = ref(rng.choice(["/ws", "", None, "svc"]))
    if rng.random() < 0.25:
        e["pluginParam"] = ref(rng.choice(["/plugin", "", None]))
    if rng.random() < 0.3:
        e["allowInsecure"] = ref(rng.choice([True, False, 1, 0, "0", "", None]))
    if rng.random() < 0.25:
        e["data"] = ref(rng.choice(["upload=1", "", None, b"raw"]))
    if rng.random() < 0.1:
        # A UID pointing off the end of $objects — an IndexError with no
        # handler anywhere.
        e["title"] = UID(len(objs) + 500)
    if rng.random() < 0.12:
        # A UID whose target is a dict: the dereference is one level, so what
        # lands in the entry is still a dict — and `str()` of it shows the
        # inner UIDs undereferenced.
        objs.append({"inner": UID(0), "k": "v"})
        e[rng.choice(["title", "obfsParam", "host"])] = UID(len(objs) - 1)
    if rng.random() < 0.1:
        objs.append({"NS.string": rng.choice(["wrapped.example.com", "包装"])})
        e[rng.choice(["host", "title", "password"])] = UID(len(objs) - 1)
    return e


def store_bytes(rng: random.Random) -> bytes:
    objs: list = [None]
    shape = rng.random()
    entries = [server_entry(rng, objs) for _ in range(rng.randint(1, 6))]
    if rng.random() < 0.3:
        # Non-server noise in the array, which the walk must skip.
        objs.append(rng.choice(["$null", 5, {"only": "type"}, {"host": "h"}, [1, 2]]))
    objs.extend(entries)

    if shape < 0.06:
        root = {"$objects": rng.choice(["a string", 5, None, {"0": "x"}])}
    elif shape < 0.1:
        root = {"$version": 100000}  # no $objects at all
    elif shape < 0.13:
        root = [1, 2, 3]  # a list at the top: not subscriptable by str
    else:
        root = {
            "$version": 100000,
            "$objects": objs,
            "$archiver": "NSKeyedArchiver",
            "$top": {"root": UID(1)},
        }
    return plistlib.dumps(root, fmt=plistlib.FMT_BINARY)


def damaged(rng: random.Random, data: bytes) -> bytes:
    """A plist that has been through something. Both implementations must agree
    on which of these are still readable."""
    kind = rng.random()
    b = bytearray(data)
    if kind < 0.3 and len(b) > 40:
        return bytes(b[: rng.randint(8, len(b) - 1)])  # truncated
    if kind < 0.7 and len(b) > 12:
        for _ in range(rng.randint(1, 3)):
            i = rng.randrange(8, len(b))
            b[i] = rng.randrange(256)
        return bytes(b)
    if kind < 0.85:
        return b"not a plist at all\n" + bytes(b[:20])
    return b""


CONF_BODIES = [
    "[General]\nbypass-system = true\nDOMAIN,ignored.example.com,PROXY\n"
    "[Rule]\nDOMAIN-SUFFIX,Example.COM,PROXY\nDOMAIN,a.example.net,DIRECT\n"
    "IP-CIDR,192.0.2.0/24,PROXY,no-resolve\ndomain,b.example.org,proxy\n"
    "# a comment\n\nDOMAIN,example.com,PROXY\nFINAL,PROXY\n[Host]\n"
    "DOMAIN,after.example,PROXY\n",
    "[rule]\r\nDOMAIN-SUFFIX,crlf.example,PROXY\r\nDOMAIN,x.example,REJECT\r\n",
    "[Rule]\rDOMAIN,bare-cr.example,PROXY\rDOMAIN-SUFFIX,two.example,PROXY\r",
    "[Rule]\nDOMAIN-SUFFIX , spaced.example , PROXY \nDOMAIN,dup.example,PROXY\n"
    "DOMAIN,DUP.example,PROXY\nDOMAIN,,PROXY\n,,\n",
    "[Rule]\nDOMAIN-SUFFIX,例え.example,PROXY\nDOMAIN-KEYWORD,kw,PROXY\n",
    "no sections at all\nDOMAIN,orphan.example,PROXY\n",
    "[Rule]\n",
    "",
]

BAD_BYTES = b"[Rule]\nDOMAIN,\xff\xfe-bad.example,PROXY\nDOMAIN-SUFFIX,ok.example,PROXY\n"


def place_confs(rng: random.Random, home: str) -> None:
    """Scatter .conf files over the three globbed locations, with distinct
    mtimes so `max(..., key=getmtime)` is not deciding on a tie."""
    stamp = 1_600_000_000
    spots: list[str] = []
    if rng.random() < 0.45:
        spots.append(f"{CLOUDDOCS}/default.conf")
    if rng.random() < 0.4:
        spots.append(f"{CLOUDDOCS}/Shadowrocket/default.conf")
    if rng.random() < 0.25:
        spots.append(f"{CLOUDDOCS}/a/b/default.conf")
    if rng.random() < 0.2:
        # glob's `**` does NOT descend into a hidden directory, and its `*`
        # does not match a leading dot.
        spots.append(f"{CLOUDDOCS}/.hidden/default.conf")
    if rng.random() < 0.2:
        spots.append(f"{CLOUDDOCS}/.dotfile.conf")
    if rng.random() < 0.4:
        spots.append(f"{ICLOUD}/one.conf")
    if rng.random() < 0.3:
        spots.append(f"{ICLOUD}/deep/two.conf")
    if rng.random() < 0.4:
        spots.append(f"{GC}/default.conf")
    if rng.random() < 0.2:
        spots.append(f"{GC}/other.conf")
    for i, rel in enumerate(spots):
        p = os.path.join(home, rel)
        if rng.random() < 0.08:
            w(p, BAD_BYTES)
        else:
            w(p, rng.choice(CONF_BODIES))
        os.utime(p, (stamp + i * 97, stamp + i * 97))
    if rng.random() < 0.1:
        # A DIRECTORY named like a hit: glob yields it, `isfile` drops it.
        os.makedirs(os.path.join(home, ICLOUD, "adir.conf"), exist_ok=True)


def make_case(rng: random.Random, d: str) -> None:
    home = os.path.join(d, "home")
    os.makedirs(home, exist_ok=True)
    argv: list[str] = []

    kind = rng.random()
    if kind < 0.07:
        argv = rng.choice(
            [
                ["--help"],
                ["--zzz"],
                ["--store"],
                ["--det"],
                ["--store", "--conf", "x"],
                ["positional"],
                ["--conf=/nonexistent.conf"],
                # `args.conf or _find_conf()` is truthiness: an empty one falls
                # through, so --detect still has to run the glob.
                ["--conf", "", "--detect"],
            ]
        )
        w(os.path.join(d, "argv"), "".join(a + "\n" for a in argv))
        w(os.path.join(d, "stdin"), "")
        return

    if rng.random() < 0.8:
        data = store_bytes(rng)
        if rng.random() < 0.22:
            data = damaged(rng, data)
        where = rng.choice(
            [f"{GC}/ServerManager", f"{ICLOUD}/shadowrocket.v2.model", f"{GC}/ServerManager"]
        )
        w(os.path.join(home, where), data)
        if rng.random() < 0.1:
            # Both candidates present: the Group Container one wins by order.
            w(os.path.join(home, f"{ICLOUD}/shadowrocket.v2.model"), store_bytes(rng))
    if rng.random() < 0.08:
        # A directory where the store should be: `isfile` says no, which is the
        # warning branch rather than an error. Only when no file landed there.
        p = os.path.join(home, f"{GC}/ServerManager")
        if not os.path.exists(p):
            os.makedirs(p, exist_ok=True)

    place_confs(rng, home)

    if kind < 0.25:
        argv = ["--detect"]
    else:
        if rng.random() < 0.15:
            argv += ["--store", os.path.join("home", GC, "ServerManager")]
        if rng.random() < 0.12:
            argv += ["--conf", os.path.join("home", GC, "default.conf")]
        if rng.random() < 0.06:
            argv += ["--store", "home/nonexistent"]
        elif rng.random() < 0.06:
            # An EMPTY --store is falsy, so `args.store or _find(...)` still
            # auto-detects and the real store under HOME is read. Placed here
            # rather than with the argparse cases so there IS one to find.
            argv += ["--store", ""]
    w(os.path.join(d, "argv"), "".join(a + "\n" for a in argv))
    w(os.path.join(d, "stdin"), "")


def main() -> int:
    out = sys.argv[1]
    count = int(sys.argv[2]) if len(sys.argv) > 2 else 300
    os.makedirs(out, exist_ok=True)
    for i in range(count):
        d = os.path.join(out, f"case{i:04d}")
        os.makedirs(d, exist_ok=True)
        make_case(RNG, d)
    print(count)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
