# The parity harness

Phase 0 of the port described in [PORTING.md](../../PORTING.md): characterize
what the bash actually does, so a Rust implementation can be held to it.

Nothing here tests the Rust (there isn't any yet). What it does today is
capture bash's behavior as artifacts a second implementation must reproduce —
and, as a side effect, give `bin/rowt` its first behavioral test suite.

## Running it

```sh
tests/parity/bin/parity list            # the command matrix
tests/parity/bin/parity run status      # one command, in a sandbox
tests/parity/bin/parity run -- explain example.com
tests/parity/bin/parity mask            # every read-only command, twice, diffed
tests/parity/bin/parity golden          # classifier verdicts vs the committed golden
tests/parity/bin/parity ledger          # regenerate LEDGER.md from bin/rowt
```

## The sandbox

Every run gets a scratch tree with `HOME` and `XDG_CONFIG_HOME` pointed into
it, seeded from `fixtures/config`, and a shim directory first on `PATH`. The
shims (`shims/_recorder`, symlinked under each name) record their argv and
return canned responses from `fixtures/env`.

That containment is not a nicety — rowt is a daily driver, and the matrix
includes `up`, `down`, `proxy on` and `restart`. Verified: a sandboxed
`proxy on --force` produces the full `networksetup` argv sequence in the trace
while the real system proxy is untouched, and sandboxed `up`/`down` leave the
running router alone. `sudo` is recorded and then delegates to the shim for the
command it wraps, so an unshimmed effectful command fails loudly instead of
running for real.

Four artifacts come out of each run: `stdout`, `stderr`, `rc`, `trace` (every
shimmed call with its argv) and `fsstate` (the config tree afterwards,
checksummed over *normalized* content).

## What Phase 0 established

**The nondeterministic-field mask** — `normalize.sed`. With it, all 38
terminating read-only commands are byte-identical across two runs. Each rule
was earned by a `parity mask` failure, never added speculatively:

- wall-clock stamps and the audit log's `+0800` variant
- pids, in output and in the audit context
- operation durations (`rc=0 (3s)`)
- `rowt report`'s saved filename, which embeds the wall clock — the *name*
  varies, not only the contents, so it has to be masked in `fsstate` too

One nondeterminism was fixed at the fixture instead: `clash_secret` generates
itself randomly on first use, so the fixture pre-seeds it.

**The classifier golden** — `golden/classify.tsv`, 92 verdicts generated from
the lane lists by `bin/gen-corpus.py`: every suffix with its near-misses, every
CIDR at its boundaries, the RFC1918 fall-through edges, and controls. This is
the corpus the design calls for, and it is deliberately *not* the lane logs —
those are error logs, so their domains are whatever happened to fail.

**The coverage ledger** — `LEDGER.md`, generated from `run_command()` in
bin/rowt so it cannot drift. All 37 command arms are in the matrix; four are
excluded with a stated cause (`tail -f`, the TUI, and `uninstall`'s
confirmation prompt).

## Characterized behavior worth knowing

Facts the golden pins down, none of which are bugs introduced here. Under
§6.7 of the design these are **behavior**: the Rust port reproduces them
first, and any change to them is a separate, deliberate commit.

- **Suffix matching has no dot boundary.** `_longest_domain_hit` compares with
  `case "$val" in *"$e")`, so the suffix `example.com` also captures
  `xexample.com` — a different domain. This mirrors sing-box's `domain_suffix`
  (HasSuffix), which the render targets, so `explain` and the live router
  agree. Write `.example.com` if you want the boundary.
- **Addresses just outside a corp CIDR can still be corp** via the RFC1918
  fall-through — `10.100.0.0` is outside `10.99.0.0/16` and still lands in
  corp, because `10.0.0.0/8` catches it.
- **`resolve_ip()` is defined twice** (bin/rowt lines ~1750 and ~2726). The
  second wins for every call; the first — which pins the lookup to
  `$DNS_DIRECT` — is dead code.

## Fixtures are synthetic, and stay that way

Everything committed here uses documentation domains and RFC 5737 ranges. The
generator accepts `--lists` pointing at a real config directory, which is a
**local-only** operation: that output carries employer-internal names and must
never be committed.
