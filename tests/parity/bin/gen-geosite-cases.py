#!/usr/bin/env python3
"""Generate cases for `parity geosite-diff`.

A case is a whole little world, because that is what `geosite-lookup.py` reads:
a cache directory that may or may not hold a category, a sing-box that may or
may not work, and a base URL that is always unreachable. The interesting
behaviour is not the lookup — `rowt_core::geosite` is unit-tested — it is what
each side does when a piece of the world is missing, since every one of those
paths is required to print nothing and exit 0.

Half the cases can fetch and half cannot, because the difference between the two
is load-bearing: `--have` consults a category WITHOUT allowing a download, on
the grounds that a set nobody has cannot be why the domain is covered today. If
every fetch failed, that rule would be unfalsifiable — a lookup that ignored it
would pass.

The fetchable half points `ROWT_GEOSITE_BASE` at a `file://` directory rather
than the network, which is only sound because curl and urllib were checked
against each other on it first: both return the bytes for a file that is there
and both leave NO destination file behind for one that is not. The other half
points at a port with nothing on it.

Usage: gen-geosite-cases.py <workdir> [count]   -> prints the case count
"""

from __future__ import annotations

import json
import os
import random
import stat
import sys
from pathlib import Path

# A category's decompiled contents. The fake sing-box "decompiles" by copying,
# so a .srs in a seed is simply this text under another name.
RULESETS = {
    "example": {"rules": [{"domain_suffix": [".example.com"], "domain": ["example.net"]}]},
    "google": {"rules": [{"domain_suffix": ["google.com", "youtube.com"]}]},
    "apple": {"rules": [{"domain_suffix": ["apple.com"]}]},
    "widgets": {"rules": [{"domain_suffix": ["widgets.example"]}]},
    # No domain_suffix at all — a set that parses and covers nothing.
    "empty": {"rules": []},
}

# Nine sets that all cover the same domain, so at least one case produces more
# results than MAX_SHOWN. Without them the cap is decoration: no realistic seed
# reaches six matches, and a lookup that printed all of them would pass.
for _i in range(1, 10):
    RULESETS[f"many{_i}"] = {"rules": [{"domain_suffix": ["example.com"]}]}

# What a fetch can find, for the cases whose base is a file:// directory. Note
# `google` is here and never in a seed: that is the case that catches a lookup
# which lets `--have` reach the network.
FETCHABLE = ["example", "google", "apple", "widgets", "empty"]

DOMAINS = [
    "a.example.com",       # brand "example" — covered by the example set
    "example.com",         # the apex itself
    "mail.google.com",     # brand "google", also an umbrella
    "www.bbc.co.uk",       # brand walks past two generics
    "notexample.com",      # dot-boundary: must NOT be covered by .example.com
    "example.net",         # exact, not suffix
    "widgets.example",     # brand "widgets" via a cached set
    "localhost",           # no dot — declines before touching anything
    "geosite:google",      # already a category — declines
    "  A.Example.COM.  ",  # normalisation: trim, lowercase, strip trailing dot
    "",                    # empty positional
    "com",                 # generic only, no dot
]

# Every shape of cache seed. `j` seeds a decompiled .json (no sing-box needed),
# `s` seeds a .srs (forces the decompile path), and the two damaged ones are
# what a half-finished earlier run leaves behind.
SEEDS = [
    [],
    [("example", "j")],
    [("example", "s")],
    [("google", "j")],
    [("apple", "j"), ("widgets", "s")],
    [("example", "j"), ("google", "j"), ("apple", "j")],
    [("empty", "j")],
    [("example", "bad-json")],   # .json that will not parse
    [("example", "bad-srs")],    # .srs the fake sing-box refuses
    [("example", "empty-srs")],  # zero-byte .srs — an interrupted download
    # More matches than MAX_SHOWN, so the cap is exercised rather than assumed.
    # `.srs`, not `.json`: `cached()` globs *.srs, so a json-only seed is
    # invisible to the candidate list and these nine would never be tried.
    [("example", "j")] + [(f"many{i}", "s") for i in range(1, 10)],
]

HAVES = [None, "google", "example", "apple,google", "nosuchcat", "", "google,"]

# Cases that must exist rather than must-probably-exist. Randomly pairing a seed
# with a domain leaves the interesting combinations to luck: the nine-set seed
# and a domain those nine actually cover co-occur about once in fifty, so a run
# of sixty could easily contain no test of MAX_SHOWN at all — and did.
#   (domain, seed, have, sing-box, can-fetch)
PINNED = [
    # More matches than the cap, with room to spare.
    ("a.example.com", "many", None, "ok", False),
    ("example.com", "many", None, "ok", False),
    # `--have` names a set that is NOT cached but IS downloadable. Consulting it
    # would print `have:google` and leave two files behind; not consulting it is
    # the rule, and this is what holds the rule to it.
    ("mail.google.com", "none", "google", "ok", True),
    # The plain fetch-and-decompile path, start to finish.
    ("a.example.com", "none", None, "ok", True),
    # Same, with a sing-box that cannot decompile what it just downloaded.
    ("a.example.com", "none", None, "broken", True),
]

NAMED_SEEDS = {
    "none": [],
    "many": [("example", "j")] + [(f"many{i}", "s") for i in range(1, 10)],
}

# A sing-box stand-in. It is handed `rule-set decompile <srs> -o <json>` and
# copies, which is exactly enough: the gate is about who calls it and what each
# side does when it fails, not about the real decompiler.
FAKE_SB = """#!/bin/sh
# args: rule-set decompile <srs> -o <json>
[ "$1" = rule-set ] && [ "$2" = decompile ] || exit 2
src="$3"; out="$5"
# A .srs this stand-in cannot read is a decompile failure, same as the real one.
head -c 12 "$src" 2>/dev/null | grep -q '{' || exit 1
cat "$src" > "$out" || exit 1
exit 0
"""

FAKE_SB_BROKEN = """#!/bin/sh
exit 1
"""


def write_seed(seed_dir: Path, seed: list[tuple[str, str]]) -> None:
    seed_dir.mkdir(parents=True, exist_ok=True)
    for name, kind in seed:
        body = json.dumps(RULESETS[name])
        if kind == "j":
            (seed_dir / f"geosite-{name}.json").write_text(body)
        elif kind == "s":
            (seed_dir / f"geosite-{name}.srs").write_text(body)
        elif kind == "bad-json":
            (seed_dir / f"geosite-{name}.json").write_text("{not json")
        elif kind == "bad-srs":
            (seed_dir / f"geosite-{name}.srs").write_text("\x00\x01binary garbage")
        elif kind == "empty-srs":
            (seed_dir / f"geosite-{name}.srs").write_text("")


def main() -> int:
    work = Path(sys.argv[1])
    count = int(sys.argv[2]) if len(sys.argv) > 2 else 200
    rng = random.Random(0xC0FFEE)
    work.mkdir(parents=True, exist_ok=True)

    n = 0
    for i in range(count):
        d = work / f"case{i:04d}"
        d.mkdir(parents=True, exist_ok=True)

        pinned = PINNED[i] if i < len(PINNED) else None
        if pinned:
            domain, seed_name, have, sb_kind, can_fetch = pinned
            seed = NAMED_SEEDS[seed_name]
        else:
            domain = DOMAINS[i % len(DOMAINS)]
            seed = SEEDS[rng.randrange(len(SEEDS))]
            have = HAVES[rng.randrange(len(HAVES))]
            # 1 in 8 gets no sing-box at all, 1 in 8 gets one that always fails
            # — both are "print nothing, exit 0" and both are reachable in the
            # wild (a fresh install, and a sing-box too old for the format).
            sb_kind = rng.choice(["ok"] * 6 + ["missing", "broken"])
            can_fetch = i % 2 == 0

        write_seed(d / "seed", seed)

        argv = []
        if have is not None:
            argv += ["--have", have]
        argv.append(domain)
        if not pinned:
            # A few put the domain first, which the Python's loop also accepts.
            if rng.random() < 0.2 and have is not None:
                argv = [domain, "--have", have]
            # And a few end on a dangling --have, which becomes the domain.
            if rng.random() < 0.1:
                argv = [domain, "--have"]
        (d / "argv").write_text("\n".join(argv) + ("\n" if argv else ""))

        # See the module docstring on why both halves are needed.
        if can_fetch:
            fetchdir = d / "fetch"
            fetchdir.mkdir(exist_ok=True)
            for name in FETCHABLE:
                (fetchdir / f"geosite-{name}.srs").write_text(json.dumps(RULESETS[name]))
            (d / "base").write_text(f"file://{fetchdir}\n")
        else:
            (d / "base").write_text("http://127.0.0.1:1\n")

        binp = d / "bin"
        binp.mkdir(exist_ok=True)
        if sb_kind != "missing":
            sb = binp / "sing-box"
            sb.write_text(FAKE_SB if sb_kind == "ok" else FAKE_SB_BROKEN)
            sb.chmod(sb.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)
        (d / "sb-kind").write_text(sb_kind + "\n")
        n += 1

    print(n)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
