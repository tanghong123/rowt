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
tests/parity/bin/selftest               # break the system 3 ways; every gate must fire
```

## Proving the gates can fail

Everything above is pass-side evidence, and a suite that has only ever agreed
with itself has not been shown to detect anything. `bin/selftest` breaks a
throwaway copy of the repo three ways and fails if a gate stays quiet: a domain
moved between lanes (the golden must fail), normalization neutered (`mask` must
go loud), and a captive-probe host changed in `_proxy_bypass_want` (the
`proxy_on` argv trace must differ). It asserts each mutation actually landed
first — a pattern that matched nothing looks exactly like a gate that failed to
fire.

Writing it caught two bugs in the gates themselves: `normalize.sed`'s temp-path
rule was greedy to end-of-token, so it swallowed the whole path and would have
hidden a file written to the wrong place; and `set -o pipefail` combined with
`cmd | grep -q` inverts the result — grep exits on the first match, the
producer dies of SIGPIPE, and a detected difference reads as no difference.

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

**The nondeterministic-field mask** — `normalize.sed`. With it, all 49
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

It records rowt's own `matched:` reason next to the lane. The lane alone is a
weak gate: with four lanes over 92 cases, a classifier that matched the wrong
suffix — or matched by resolved IP instead of by suffix — still lands on the
right lane and passes.

**The coverage ledger** — `LEDGER.md`, generated from `run_command()` and
expanded by each `cmd_*` function's first-argument `case`, so subcommands are
measured too. Counting only first-level commands would let `watch install` hide
behind `watch status`. 63 rows, all covered; entries that cannot run
unattended (`tail -f`, the TUI, `uninstall`'s prompt, and the ones needing an
import fixture) are skipped with a stated cause. Subcommands reachable only
through a `*` catch-all cannot be enumerated from the source and are not
listed.

**Watchdog coverage** — the captive decision table of DESIGN.md §11 runs from
the matrix: clear, captive-by-body, captive-by-redirect, unknown-on-failure,
plus the drop and restore branches. A scenario overlay
(`PARITY_SCENARIO=proxy-on`) supplies machine states the base fixtures don't
describe, since the drop only fires when the proxy is actually on. Verified: the
drop emits the full `sudo networksetup …` sequence and logs the §11 line.

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
