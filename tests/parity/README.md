# The parity harness

The differential harness for the port described in [PORTING.md](../../PORTING.md).
It characterizes what the bash actually does, and holds `crates/rowt-core` to
that behavior rather than to anyone's reading of the shell.

It also gave `bin/rowt` its first behavioral test suite, which is worth having
whether or not the port ever finishes.

## Running it

```sh
tests/parity/bin/parity list            # the command matrix
tests/parity/bin/parity run status      # one command, in a sandbox
tests/parity/bin/parity run -- explain example.com
tests/parity/bin/parity mask            # every read-only command, twice, diffed
tests/parity/bin/parity golden          # classifier verdicts vs the committed golden
tests/parity/bin/parity ledger          # regenerate LEDGER.md from bin/rowt

# cross-implementation gates (bash/Python vs Rust)
tests/parity/bin/parity render-matrix    # the rendered config, canonically
tests/parity/bin/parity classify-matrix  # lane AND matched reason
tests/parity/bin/parity lanes-diff       # lane-list edits: files + messages
tests/parity/bin/parity reconcile-diff   # corp reconcile, over generated cases
tests/parity/bin/parity netdetect-diff   # scutil --dns parser, over generated cases
tests/parity/bin/parity vless-diff       # share-link parser: stdout + stderr + status
tests/parity/bin/parity merge-diff       # import accumulation: the review file itself
tests/parity/bin/parity foreign-diff     # other clients' config trees, over generated cases
tests/parity/bin/parity sr-diff          # Shadowrocket's plist + rules, over generated cases
tests/parity/bin/parity watch-diff       # watchdog decisions
tests/parity/bin/parity platform-diff    # the argv the platform layer produces
tests/parity/bin/parity cli-diff         # whole commands, rowt-rs vs the shell

tests/parity/bin/selftest                # break each gate; every one must fire
```

## Proving the gates can fail

Everything above is pass-side evidence, and a suite that has only ever agreed
with itself has not been shown to detect anything. `bin/selftest` breaks a
throwaway copy of the repo once per gate and fails if any of them stays quiet —
a domain moved between lanes, normalization neutered, a captive-probe host
changed, the corp `domain_resolver` dropped, a classifier reason reworded, the
single-lane invariant disabled, a captive log line reworded. It asserts each
mutation actually landed first: a pattern that matched nothing looks exactly
like a gate that failed to fire.

This has repeatedly earned its keep. It caught `normalize.sed`'s temp-path rule
running greedy to end-of-token, so it swallowed whole paths and would have
hidden a file written to the wrong place. It caught `cmd | grep -q` under
`set -o pipefail` inverting its own result — grep exits on the first match, the
producer dies of SIGPIPE, and a detected difference reads as no difference. And
the lane gate once reported 12/12 identical purely because both sides were being
handed a malformed command and rejecting it in the same way.

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

Each case also runs in its own **session**, with no controlling terminal. That
is what makes `config import` comparable: it asks for confirmation on
`/dev/tty` rather than stdin, and a process that cannot open one treats the
failure as "no". The gate is therefore comparing the same thing a pipe or a
cron job would get, not a test-only path. The wrapper that arranges this also
signals the whole process group on timeout, so a `<lane> log` case's `tail -f`
dies with the case instead of outliving the run.

### The bundle fixtures

Three `.tgz` files under `fixtures/` are what `config import` is pointed at:
`config-bundle.tgz` (a well-formed bundle, every file stored 0644 so the 0600
the import applies to the four secret-bearing ones is visible in `fsstate`),
`not-a-bundle.tgz` (a readable archive carrying neither `servers.json` nor
`escape-domains.txt`), and `bundle-no-servers.tgz` (`servers.json` is `[]`,
which still renders — see PORTING.md §6.7). They are binary and committed, so
the regeneration is written down: stage the files, `chmod 644`, `touch -t
202608100000`, then

```
COPYFILE_DISABLE=1 tar cf - --uid 0 --gid 0 <files> | gzip -n > <name>.tgz
```

— `-n` and the fixed mtime keep the archive byte-stable, and
`COPYFILE_DISABLE` keeps macOS from adding `._` entries.

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
unattended (`tail -f` and the TUI) or that MUTATE the machine are skipped with a
stated cause — `mask` asks whether a read-only command is byte-stable across two
runs, which is not a question a writer has. Those are `cli-diff`'s, and the
skip reasons now say which scenario compares each one. Subcommands reachable only
through a `*` catch-all cannot be enumerated from the source and are not
listed.

**Watchdog coverage** — the captive decision table of DESIGN.md §11 runs from
the matrix: clear, captive-by-body, captive-by-redirect, unknown-on-failure,
plus the drop and restore branches. A scenario overlay
(`PARITY_SCENARIO=proxy-on`) supplies machine states the base fixtures don't
describe, since the drop only fires when the proxy is actually on. Verified: the
drop emits the full `sudo networksetup …` sequence and logs the §11 line.

## The cross-implementation gates

`crates/rowt-core` holds the render, the classifier, lane edits, the corp
reconcile and the watchdog's decision table. Each has a gate:

| gate | compares | scale |
|---|---|---|
| `render-matrix` | rendered config, canonical JSON | 21 shapes × host + vm |
| `classify-matrix` | `(lane, reason)` per destination | 9 shapes × 92 destinations |
| `lanes-diff` | all three lane files + messages | 12 edits |
| `reconcile-diff` | stdout contract vs the Python | 210 cases, 200 randomized |
| `netdetect-diff` | stdout BYTES vs the Python (key order is contract) | 800 generated cases |
| `vless-diff` | stdout + stderr + exit status vs the Python | 2,000 generated cases |
| `merge-diff` | the review FILE, plus the streams, vs the Python | 1,500 generated cases |
| `foreign-diff` | stdout + stderr + exit status, over client config TREES | 1,200 generated cases |
| `sr-diff` | stdout + stderr + exit status, over Shadowrocket installs | 1,200 generated cases |
| `watch-diff` | decisions, read back from watch.log + trace | 5 cases |
| `platform-diff` | the argv the platform layer produces | 8 cases |
| `cli-diff` | stdout, status, the config tree (content + mode), argv trace, audit log | 241 cases |

`merge-diff` is the only gate whose primary artifact is a file written in
place: `cmd_import` reads the accumulation straight back with jq, and a human
edits it between the extract and the apply, so `_source`'s position inside an
entry and the top-level key order are compared as bytes. It also runs each case
twice in two copies of its own directory, since the run mutates its inputs.

`foreign-diff` and `sr-diff` are the two whose input is a DIRECTORY rather than
a string, because the programs they gate read another application's install.
Each case is a whole fake `$HOME`. For `foreign-diff` that holds a Clash
profile tree or a V2Box SQLite store, plus the PATH the run should see — so the
same corpus drives `yq` present, missing, failing, and answering with something
that is not JSON. That last axis is not
decoration: with a `profiles.yaml` index a `yq` problem is fatal and without
one it is `except Exception: continue`, so the same broken `yq` either stops
the import or silently yields nothing depending on which client wrote the
directory.

`sr-diff` is the one that reads a binary format. Its corpus writes real
NSKeyedArchiver plists with `plistlib`, then truncates and byte-flips a fifth
of them — so the gate is as much about agreeing on which damaged stores are
still READABLE as about what a readable one yields. It also carries the one
place the Rust is deliberately narrower than the Python, named in
`rowt-core::bplist`: XML plists are not read, only `bplist00`, which is what
Shadowrocket writes and all `bin/rowt` can produce. The corpus contains no XML
plist, because a corpus that avoids a gap is not evidence the gap is closed —
the module doc is.

`vless-diff`, `merge-diff`, `foreign-diff` and `sr-diff` are the gates that
compare stderr, because it is where stderr is a result: `server add` and the
subscription rebuild run the parser with stderr on the user's terminal, so
"skipping a link (…)" and "not importing 'X' — same server as 'Y'" are what the
user learns about their own servers.

They are also the only gates that normalize anything, and they report how many
cases it touched so a growing number is visible. `vless-diff` squashes the
parenthetical of a `JSONDecodeError`, whose wording is interpreter-version
specific. `foreign-diff` and `sr-diff` squash tracebacks to `<EXC:TypeName>` on
both sides:
several inputs really do die rather than fail cleanly — a `reality-opts:` that
is a string, a BLOB in the `ZURL` column — and the invariant worth holding is
that the SAME inputs crash with the same kind of error and the same inputs
succeed byte for byte. Matching frame lines would pin the interpreter, not the
behaviour. Everything printed before the traceback still compares exactly.

### The CLI gate, and why it reads the disk

`cli-diff` compares five things, and the biggest of them is not a stream: after
both sides run, it snapshots the whole config tree — every file except logs,
pidfiles, caches and the sing-box binary — as a normalized checksum plus the
file MODE, and requires the two to match.

That was added for the pool arms (`server add|rm|clear`, `sub add|rm|update|clear`),
and it is what makes them gateable at all: what those commands DO is write
`manual.json`, `subs.txt`, the rebuilt `servers.json` and the `selected` line of
`state`. Their stdout is a summary of the result, so an implementation that
printed the summary and wrote nothing would have passed. The mode travels with
the checksum because those files hold credentials and subscription tokens and
are 0600 on purpose — a checksum cannot see a dropped `chmod`, and getting one
wrong is a security regression rather than a near-miss.

It found three real divergences the day it was added: `rowt-rs` was writing
`host.json`, `vm.json` and `state` world-readable where the shell's
`mktemp`-then-`mv` idiom left them owner-only. `host.json` carries the escape
server's uuid.

Three limits, stated rather than implied:

* **A successful subscription fetch is not compared.** `bin/rowt` fetches with
  `urllib` inside `vless-parse.py`, which no PATH shim can reach; `rowt-rs`
  runs `curl`, which the recorder shim answers. The fixture subscriptions point
  at a closed local port so both sides take the fetch-FAILED path, and
  `normalize.sed` drops the one `curl` line only one of them can produce. What
  a real subscription body parses into is `vless-diff`'s job.
* **`server import --from <client>` is compared on an EMPTY machine.** The
  sandbox `HOME` contains no VPN client, so what those cases prove is that both
  sides find nothing and fail identically. What a client tree full of data
  yields is `foreign-diff`'s and `sr-diff`'s question, over 1,200 synthetic
  installs each — and `yq` is not on the sandbox PATH, so the Clash YAML path
  could not be compared here even with a fixture. The half that writes,
  `--apply`, IS fully compared, from a seeded `import-review.json`.
* **No case runs a lifecycle arm under `router-up`.** That scenario makes the
  router "running" by writing the harness's own pid into `host.pid`, so any
  case that stops or restarts it kills the gate. It rules out
  `PARITY_SCENARIO=router-up` for `up`, `down`, `reload`, `restart` and
  `uninstall` alike — the arms themselves ARE gated, from a router-down start,
  which is the state whose failure path decides the exit status and clears the
  proxy. `corp sync` is out of reach for the same reason.
* **A purge leaves nothing to compare, and that is agreement.** `diff -q` on
  two files that are both absent exits 2, which reads like "differs" — so the
  lane-file check asks whether either side still has the file before it
  compares. `uninstall --purge` removes the config directory on both sides,
  and for a while that looked like three diverging lane files.

### The render gate

`parity render-matrix` runs every case in `render-cases.txt`, covering the selector branches
(auto / pinned / unknown / no servers), cached and uncached `geosite:`
categories, the ad set, an empty corp lane, vm mode, unicode and tie-breaking
suffixes, and the `ROWT_*` knobs — and requires canonical-JSON equality on both
`host.json` and `vm.json`.

Canonical rather than byte-exact, because key order is not meaning. But
**blocking**, because this is the only gate that inspects the artifact being
rewritten: `explain` walks the list files and never reads the rendered config,
so a render that lost the corp outbound's `domain_resolver` would diff
perfectly clean through it. `selftest` step 5 drops exactly that field and
requires the gate to go red.

Validated against the real config too — 22 servers, real lane lists, cached
geosites — canonically identical. That is a **local-only** check: the rendered
config carries credentials, so compare it in place and never copy or paste the
output.

## Shadow mode

Two shadows exist, both off unless switched on, and neither ever authoritative:

    ROWT_RENDER_SHADOW=1    every `rowt render` also renders in Rust and
                            compares canonically
    ROWT_WATCH_SHADOW=1     every watchdog tick compares the shell's decisions
                            against the FSM's planned actions

Divergences land in `~/.config/rowt/log/parity.log`, recorded only when the
verdict changes so steady state stays quiet. The watch shadow is handed the
captive verdict the tick already computed rather than probing for itself — a
shadow that probed separately would observe a different instant, double the
traffic to the probe hosts, and fill the log with timing artifacts.

This is what accumulates evidence from networks no fixture can stage: offices,
hotels, planes, VPN-up states. `selftest` steps 6 and 10 verify both shadows
actually record a divergence rather than failing quietly.

### Divergence bundles

A verdict alone tells you *that* two implementations disagreed, which is the
least useful half. So the watch shadow also captures, on divergence only, the
raw output of every command the observation was built from — `scutil --dns`,
the `networksetup` readers, `route`, `ipconfig`, `ifconfig`, `netstat`,
`sysctl` — keyed exactly the way the recorder shim expects.

    ~/.config/rowt/log/parity/<timestamp>/
      obs.json   what the Rust tick was given
      plan       what it decided
      actual     what the shell decided
      state      the state keys that fed it — the PRE-tick captive flag, since
                 the tick may have just changed it
      env/       the machine, as raw command output

    tests/parity/bin/parity replay-bundle <dir>

Because env/ is shim-shaped, replaying drops it straight into a sandbox and
both implementations see exactly the machine that disagreed — offline, and as
often as you like. That matters most for the failure this cannot otherwise
catch: the two sides *parsing the same machine differently*. A decision log can
never show that.

The replay pins the captive verdict to what the bundle recorded rather than
probing again, or it would classify the portal afresh and answer a question you
did not ask.

Bundles are mode 700 and **local only** — they contain your DNS configuration,
network names and internal addresses.

## Does it actually work? (`bin/boot-test`)

Every other gate compares two implementations. This one asks what comparison
cannot: sing-box is handed the **Rust** render and real traffic is pushed
through each lane.

    tests/parity/bin/boot-test                       # synthetic servers
    tests/parity/bin/boot-test --server <tag>        # …plus a real tunnel

It renders with both, makes sing-box judge the Rust output, boots that config on
a private port, and drives the direct, block and escape lanes for real — then
runs `reload` and `router down` and checks the machine is untouched.

**The safety rule it encodes, learned by breaking it:** a private config
directory and a private port do *not* isolate the system proxy. That setting is
global, and `up`, `down`, `reload`, `restart` and `router down` all reach for
it — `reload` re-points it at the test instance's port, `router down` turns it
off. Running a second instance without accounting for that will break the
machine's networking, which is exactly what happened here first.

So every system-proxy write in both implementations now goes through a single
guarded function — `_ns_write` in the shell, `sudo_networksetup` in
rowt-platform — and `ROWT_NO_SYSPROXY=1` turns them all into no-ops while reads
keep working. With the guard set, the lifecycle commands that used to be too
dangerous to test are the ones this exercises. The test verifies the guard held
by comparing the system proxy before and after.

`--server` copies real credentials into a mode-700 scratch directory removed on
exit, including on failure. Local only; never CI.

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
