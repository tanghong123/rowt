#!/usr/bin/env python3
"""Generate accumulation cases for the import-merge differential gate.

Synthetic only — documentation domains, RFC 5737 addresses, fixed synthetic
credentials. Nothing here comes from a real client, and nothing here may.

`config/test_import_merge.py` DOES invoke the script as a subprocess, so unlike
test_parse.py its six checks are replayable; they are carried over as Rust unit
tests and this corpus goes after what they do not reach — a review file whose
keys are typed wrong or ordered oddly, a --pool path that does not exist (which
the Python cannot distinguish from an empty one), subscription URLs that
normalize onto each other, and the `skipped` counters, where a value that will
not convert must leave the existing one alone.

Each case is a directory holding `argv` (one argument per line, paths RELATIVE
to the directory so it can be copied and run twice) plus the files it names.
"""
from __future__ import annotations
import json, os, random, sys

UUIDS = [f"00000000-0000-4000-8000-{i:012d}" for i in range(1, 6)]
HOSTS = ["h1.example", "h2.example", "192.0.2.1", "198.51.100.7", "203.0.113.9"]
SOURCES = ["", "src1", "shadowrocket", "clash-verge", "v2box", "香港"]
# Pairs that must normalize onto each other, and pairs that must not.
SUB_URLS = [
    "https://sub.example/s", "https://sub.example/s/", "https://SUB.EXAMPLE/s",
    "https://sub.example/s?token=1", "https://sub.example/s?token=1&name=foo",
    "https://sub.example/s?name=bar&token=1", "https://sub.example/s#frag",
    "https://sub.example/S", "https://sub.example/s?flag",
    "https://sub.example/s?q=a b", "https://sub.example/s?q=中",
    "https://u@[bad]:443/s", "not a url", "  https://sub.example/s  ",
    "http://sub.example/s", "https://other.example/s",
]
DOMAINS = ["example.com", "www.example.com", "example.org", "example.com"]
SKIPPED_VALS = [0, 1, 7, "2", "x", None, True, 1.5, [], 3.0]


def server(r: random.Random) -> dict:
    o = {
        "type": r.choice(["vless", "vmess", "anytls", "hysteria2"]),
        "tag": r.choice(["JP", "Tokyo 01", "香港 01", "escape", ""]),
        "server": r.choice(HOSTS),
        "server_port": r.choice([443, 8443, "443"]),
    }
    if r.random() < 0.7:
        o["uuid"] = r.choice(UUIDS + [""])
    else:
        o["password"] = r.choice(["pw-1", "pw-2", ""])
    if r.random() < 0.3:
        o["_source"] = r.choice(["stale-source", "src1"])   # must be OVERWRITTEN in place
    if r.random() < 0.25:
        o["tls"] = {"enabled": True, "server_name": "s.example"}
    return o


def review(r: random.Random, *, weird: bool) -> dict:
    """An importer extract, or an accumulation file. `weird` lets the required
    keys be the wrong type or missing, which the Python repairs in place."""
    d: dict = {}
    if weird and r.random() < 0.3:
        d["servers"] = r.choice([{}, "nope", 5, None])
    else:
        d["servers"] = [server(r) for _ in range(r.randint(0, 4))]
    if weird and r.random() < 0.25:
        d["subscriptions"] = r.choice([{}, "nope", None])
    else:
        d["subscriptions"] = [{"url": r.choice(SUB_URLS)} for _ in range(r.randint(0, 3))]
        if r.random() < 0.2:
            d["subscriptions"].append({"note": "no url here"})
    if r.random() < 0.6:
        d["proxy_domains"] = [r.choice(DOMAINS) for _ in range(r.randint(0, 3))]
    if r.random() < 0.5:
        d["skipped"] = {k: r.choice(SKIPPED_VALS)
                        for k in r.sample(["ss", "trojan", "unknown", "kcp"], r.randint(1, 3))}
    if weird and r.random() < 0.2:
        d["skipped"] = r.choice([[], "nope"])
    if r.random() < 0.2:                       # an unknown key must survive
        d["_note"] = "hand-edited"
    if r.random() < 0.15:                      # key order must be preserved
        d = {k: d[k] for k in reversed(list(d))}
    return d


def case(r: random.Random, d: str) -> None:
    argv: list[str] = []
    stdin = ""
    roll = r.random()

    if roll < 0.06:                            # argument errors
        argv = r.choice([[], ["--into", "into.json"], ["--add", "add.json"],
                         ["--into", "into.json", "--add", "add.json", "--nope"],
                         ["--into"]])
        with open(os.path.join(d, "argv"), "w", encoding="utf-8") as fh:
            fh.write("".join(a + "\n" for a in argv))
        with open(os.path.join(d, "stdin"), "w", encoding="utf-8") as fh:
            fh.write(stdin)
        return

    # --add: usually a review object, sometimes something that is not one.
    if r.random() < 0.10:
        add_text = r.choice(["[]", "5", '"str"', "not json", "", "null"])
    else:
        add_text = json.dumps(review(r, weird=False), ensure_ascii=False)
    with open(os.path.join(d, "add.json"), "w", encoding="utf-8") as fh:
        fh.write(add_text)
    argv += ["--add", "add.json"]

    # --into: a file that may not exist, may be empty, may be malformed.
    into_roll = r.random()
    if into_roll < 0.12:
        argv = ["--into", "-"] + argv          # write to stdout instead
    else:
        if into_roll < 0.30:
            pass                               # no file at all
        elif into_roll < 0.38:
            open(os.path.join(d, "into.json"), "w").close()
        elif into_roll < 0.46:
            with open(os.path.join(d, "into.json"), "w", encoding="utf-8") as fh:
                fh.write(r.choice(["not json", "[]", "[1,2]", "3"]))
        else:
            with open(os.path.join(d, "into.json"), "w", encoding="utf-8") as fh:
                fh.write(json.dumps(review(r, weird=True), ensure_ascii=False))
        argv = ["--into", "into.json"] + argv

    if r.random() < 0.85:
        argv += ["--source", r.choice(SOURCES)]

    for n in range(r.randint(0, 2)):
        name = f"pool{n}.json"
        pr = r.random()
        if pr < 0.20:
            pass                               # referenced but never written
        elif pr < 0.32:
            with open(os.path.join(d, name), "w", encoding="utf-8") as fh:
                fh.write(r.choice(["not json", "{}", ""]))
        else:
            with open(os.path.join(d, name), "w", encoding="utf-8") as fh:
                json.dump([server(r) for _ in range(r.randint(0, 3))], fh, ensure_ascii=False)
        argv += ["--pool", name]

    for n in range(r.randint(0, 2)):
        name = f"subs{n}.txt"
        if r.random() < 0.18:
            pass                               # referenced but never written
        else:
            lines = []
            for _ in range(r.randint(0, 4)):
                lines.append(r.choice(SUB_URLS) if r.random() < 0.75
                             else r.choice(["", "   ", "# a comment", "#"]))
            with open(os.path.join(d, name), "w", encoding="utf-8") as fh:
                fh.write("\n".join(lines) + r.choice(["\n", ""]))
        argv += ["--pool-subs", name]

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
