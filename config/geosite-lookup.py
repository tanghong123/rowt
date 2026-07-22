#!/usr/bin/env python3
"""List geosite categories that cover a domain — for `rowt <lane> add`'s hint.

    geosite-lookup.py <domain> [--have cat1,cat2,...]

Prints, one per line, the geosite categories that CONTAIN <domain> (so the user
can `rowt <lane> add geosite:<name>` if they want the whole service). If a category
already on the lane (`--have`) covers it, prints a single `have:<cat>` line instead
(it's already covered — nothing to suggest).

Candidates checked: the domain's own brand (its SLD label), a curated list of big
owners, and every set already cached locally. Missing brand/umbrella sets are
fetched best-effort; on any failure it prints nothing (so `add` still works
offline). Env: ROWT_CACHE, ROWT_SB, ROWT_GEOSITE_BASE.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import urllib.request
from pathlib import Path

GENERIC = {
    "com",
    "org",
    "net",
    "io",
    "co",
    "me",
    "app",
    "dev",
    "xyz",
    "tv",
    "gl",
    "sk",
    "bz",
    "la",
    "vg",
    "pk",
    "gd",
    "sh",
    "in",
    "ly",
    "tt",
    "mp",
    "cr",
    "goo",
    "www",
    "cdn",
    "raw",
    "gist",
    "mail",
    "drive",
    "docs",
    "chat",
    "hub",
    "colab",
    "ne",
    "or",
    "research",
    "static",
    "cloud",
    "api",
    "edu",
    "gov",
    "ac",
    "uk",
    "jp",
    "hk",
    "cn",
}
UMBRELLAS = "google meta amazon microsoft apple cloudflare akamai fastly netflix telegram openai anthropic".split()
MAX_SHOWN = 6


def main() -> int:
    args, domain, have = sys.argv[1:], None, []
    i = 0
    while i < len(args):
        if args[i] == "--have" and i + 1 < len(args):
            have = [x for x in args[i + 1].split(",") if x]
            i += 2
        else:
            domain = args[i]
            i += 1
    if not domain:
        return 0
    domain = domain.strip().lower().rstrip(".")
    if not domain or "." not in domain or domain.startswith("geosite:"):
        return 0

    cache = Path(os.environ.get("ROWT_CACHE") or (Path.home() / ".config/rowt/cache"))
    sb = os.environ.get("ROWT_SB") or str(Path.home() / ".config/rowt/bin/sing-box")
    base = (
        os.environ.get("ROWT_GEOSITE_BASE")
        or "https://github.com/SagerNet/sing-geosite/raw/rule-set"
    )
    if not Path(sb).exists():
        return 0
    cache.mkdir(parents=True, exist_ok=True)

    def load(name: str, allow_fetch: bool):
        js, srs = cache / f"geosite-{name}.json", cache / f"geosite-{name}.srs"
        if not js.exists():
            if not srs.exists():
                if not allow_fetch:
                    return None
                try:
                    urllib.request.urlretrieve(f"{base}/geosite-{name}.srs", srs)
                except Exception:  # noqa: BLE001
                    return None
            try:
                subprocess.run(
                    [sb, "rule-set", "decompile", str(srs), "-o", str(js)],
                    check=True,
                    capture_output=True,
                )
            except Exception:  # noqa: BLE001
                return None
        try:
            doc = json.loads(js.read_text())
        except Exception:  # noqa: BLE001
            return None
        suf, exact = set(), set()
        for r in doc.get("rules", []):
            for s in r.get("domain_suffix", []) or []:
                suf.add(s.lstrip("."))
            for s in r.get("domain", []) or []:
                exact.add(s)
        return exact, suf

    def covers(cov, d: str) -> bool:
        exact, suf = cov
        return d in exact or any(d == s or d.endswith("." + s) for s in suf)

    # already covered by a category on this lane? then there's nothing to suggest.
    for name in have:
        cov = load(name, False)
        if cov and covers(cov, domain):
            print(f"have:{name}")
            return 0

    labels = domain.split(".")
    brand = [
        lbl
        for lbl in dict.fromkeys(
            [
                labels[-2] if len(labels) >= 2 else "",
                labels[-3] if len(labels) >= 3 else "",
            ]
        )
        if lbl and lbl not in GENERIC
    ]
    cached = sorted(
        p.name[len("geosite-") : -len(".srs")] for p in cache.glob("geosite-*.srs")
    )
    have_set = set(have)

    seen, results = set(), []
    # brand first (most relevant), then locally-cached (free), then umbrellas.
    for name, allow in (
        [(n, True) for n in brand]
        + [(n, False) for n in cached]
        + [(n, True) for n in UMBRELLAS]
    ):
        if name in seen or name in have_set:
            continue
        seen.add(name)
        cov = load(name, allow)
        if cov and covers(cov, domain):
            results.append(name)

    for name in results[:MAX_SHOWN]:
        print(name)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
