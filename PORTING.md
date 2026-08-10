# Porting rowt: bash → Rust, macOS → macOS + Linux

*Investigation and design, 2026-08-08. Status: proposed — nothing here is built.*

The question asked: rowt is mostly bash — does it make sense to refactor it
into a systems language, modularized, so that (a) it can run on both macOS and
Linux, and (b) maintenance and verification get easier?

**Verdict: yes — Rust, one Cargo workspace, strangler migration in six phases.**
Portability is the deciding factor: supporting Linux from bash means threading
`case $(uname)` through every platform call site of a 4.3k-line monolith, which
is strictly worse than both the status quo and a rewrite. A typed platform
layer is the cheaper path to the stated goal, and the testability gains come
along for free.

---

## 1. What exists today (measured 2026-08-08)

| Component | Language | Size | Role |
|---|---|---|---|
| `bin/rowt` | bash | 4,332 lines, 181 functions | everything: CLI, render, lanes, watchdog, captive, proxy, VM |
| `config/*.py` | Python | 2,529 lines (734 of them tests) | import pipeline, net-detect, reconcile, fake portal |
| `rowt-monitor/` | Rust | 4,344 lines + tests | TUI + collector sidecar; golden-test culture already in place |
| `install.sh` | bash | 148 lines | copy + symlink + zshrc wiring (no platform calls) |
| `lima/` | YAML/bash | — | VM escape variant (Lima + socket_vmnet) |
| sing-box | Go (external) | — | the actual router engine; **already cross-platform** |
| corp-route | bash (separate repo) | — | root route daemon; stays separate by design (DESIGN.md §6) |

`bin/rowt` breaks down into these subsystems (by line range):

| Subsystem | ~lines | Portable? |
|---|---|---|
| prelude, state (`sget`/`sset`), audit, utils | 330 | yes |
| artifact fetch (geosites, rulesets, guest images) | 100 | yes |
| lane lists: edit/add/rm, `corp_sync`, `corp_suggest` | 480 | yes (pure set logic) |
| lane logs, monitor launch, connections view | 250 | yes |
| **config render** (`assemble_host`, `group_jq`) | 235 | yes (pure: lists+state → JSON) |
| import / servers / sub / use / ping / probe | 420 | yes (heavy lifting already in Python) |
| host router + collector lifecycle | 175 | mostly (pid/exec) |
| **VM subsystem** (Lima, socket_vmnet) | ~280 | **no — macOS-only by purpose** |
| system proxy + captive + discovery journal | 200 | **no — networksetup ×37** |
| env / shell-init / completion | 130 | yes |
| setup / onboard | 195 | mixed |
| classify / explain / status | 135 | yes (pure) |
| **watchdog** (tick, recover, health, sudoers) | 365 | logic yes; effects no |
| uninstall / skill / revert / diag | 305 | mixed |
| help text | 525 | yes |
| audit / metrics / config / dispatch | 230 | yes |

The pattern is stark: **the logic is portable; the ~15% that touches the OS is
concentrated in three seams** (plus one macOS-only feature). `net-detect.py`
already demonstrates the target shape — a pure parser with `--input FILE` for
tests and exactly one platform subprocess (`scutil --dns`) behind it.

## 2. The platform seams

| Plane | macOS today | Linux equivalent | Notes |
|---|---|---|---|
| System proxy | `networksetup -set{socksfirewall,web,secureweb}proxy*` (37 call sites, sudoers-whitelisted) | fragmented: GNOME `gsettings`, KDE, env vars | **the hard seam — see §4.1** |
| Watchdog schedule | `launchctl` + LaunchAgent plist (7 sites) | systemd user service/timer | mechanical |
| Discovery | `scutil --dns`, `ipconfig getpacket` (8 sites) | `resolvectl status` / NetworkManager | net-detect.py parser split already isolates this |
| Route reading | `netstat -rn` (3 sites; rowt never *writes* routes) | `ip route` | trivial |
| Boot id | `sysctl kern.boottime` | `/proc/sys/kernel/random/boot_id` | trivial |
| Socket binding | `bind_interface` in sing-box config | same — sing-box maps to IP_BOUND_IF / SO_BINDTODEVICE itself | **free** |
| Captive probe | `curl --noproxy` | identical | free |
| VM escape variant | Lima + socket_vmnet | n/a — its purpose is "run the engine in a Linux guest"; moot on Linux | gate `cfg(target_os = "macos")`, do not port |

## 3. Why Rust (and not Go, and not modular bash)

- **Rust**: the repo already carries 4.3k lines of it with a working test
  culture (golden renders, doc-enforcement tests) and a proven brew
  prebuilt-asset release lane for Rust binaries. One workspace lets the
  monitor/collector consume `rowt-core` types instead of re-parsing state
  files. No new toolchain.
- **Go**: sing-box being Go is irrelevant — rowt *execs* it, never links it.
  Choosing Go adds a second toolchain and orphans the monitor. No advantage.
- **Modular bash**: splitting the monolith into sourced files fixes file size
  and nothing else. The pains that motivated this — the jq render as one giant
  single-quoted string (an apostrophe in a comment broke `bash -n`), captive
  tests requiring live system-proxy toggles, zsh quoting traps, stringly
  state — all survive. And Linux support would still fork every call site.

## 4. Target architecture

```
rowt/                       (Cargo workspace root)
├── crates/
│   ├── rowt-core/          # PURE: render + classify today; lanes, captive FSM,
│   │                       #       state, discovery journal, metrics to come
│   ├── rowt-platform/      # trait Platform + Mac (built); Linux in Phase 5
│   ├── rowt-cli/           # `rowt-rs` — built ALONGSIDE the shell, not replacing it
│   └── rowt-import/        # (last/optional) port of the Python import pipeline
├── rowt-monitor/           # existing crate, joins the workspace; uses rowt-core
├── config/*.py             # shrink over time; import pipeline may stay Python
└── bin/rowt                # shrinks each phase, deleted in Phase 4
```

The trait is deliberately small — everything the bash calls out for, nothing
more:

```rust
trait Platform {
    // proxy plane
    fn proxy_set(&self, cfg: &ProxyConfig) -> Result<()>;   // 3 protos + bypass list
    fn proxy_clear(&self) -> Result<()>;
    fn proxy_read(&self) -> Result<ProxyStatus>;
    // service plane
    fn watchdog_install(&self) -> Result<()>;
    fn watchdog_uninstall(&self) -> Result<()>;
    // discovery plane
    fn dns_snapshot(&self) -> Result<DnsSnapshot>;          // per-iface domains + ns
    fn dhcp_search_domains(&self, iface: &str) -> Result<Vec<String>>;
    fn default_route(&self) -> Result<Option<GatewayInfo>>;
    fn vpn_ifaces(&self) -> Result<Vec<String>>;
    fn boot_id(&self) -> Result<String>;
}
```

And the watchdog becomes a pure function — the payoff seam:

```rust
// No side effects. The captive state machine (DESIGN.md §11), recovery,
// health checks and journal decisions all live here, unit-testable with a
// fabricated Observation instead of a live proxy toggle.
fn watch_tick(obs: &Observation, st: &State) -> (Vec<Action>, State);
```

**Invariants — what does NOT change:**
- On-disk formats: `state` key=value file, lane list files, log formats,
  `~/.config/rowt` layout. The monitor and collector keep working mid-migration
  and `brew upgrade` stays seamless.
- CLI surface: every documented subcommand, flag and output shape (the rowt
  skill and muscle memory depend on them).
- sing-box stays the engine; corp-route stays a separate root daemon (its
  lifecycle, privileges and multi-corp scope are different — a rewrite does
  not absorb it, though it could later borrow `rowt-platform` as a library).

### 4.1 Linux design decisions

1. **Prefer tun mode over system proxy on Linux.** There is no
   `networksetup` equivalent — desktop proxy settings are fragmented
   (gsettings / KDE / env vars) and CLI apps ignore them anyway. sing-box tun
   mode removes the whole proxy plane: all traffic enters the lanes without
   any per-desktop wiring. Consequences:
   - needs `CAP_NET_ADMIN` → run the engine as a systemd system service with
     `AmbientCapabilities=CAP_NET_ADMIN` (analogous role to today's sudoers
     entry, and tighter).
   - captive handling changes shape: "drop the proxy" becomes "pause the tun
     route" — same FSM, different Action emitted, which is exactly what the
     pure-tick design accommodates.
   - explicit-proxy mode remains as fallback (gsettings on GNOME; otherwise
     print env exports), mainly for unprivileged setups.
2. **Tun must coexist with other VPN clients — by exclusion, never by
   fighting.** On macOS this problem doesn't exist: system proxy is opt-in
   per app, so a corp VPN client's outer tunnel and tailscaled never touch rowt, and
   rowt is route-inert (DESIGN.md §6). Naive tun breaks that on three fronts:
   `auto_route`'s policy rules outrank the VPN's routes (and `strict_route`
   is documented to break Tailscale's fwmark routing); DNS hijack collides
   with VPN-pushed per-link DNS; and tun captures the VPN client's *own*
   tunnel traffic — worst case the corp gateway endpoint lands in the escape
   lane and the corp VPN rides the VLESS proxy. The design therefore is:
   - `route_exclude_address` = PRIVATE_CIDRS (RFC1918 + 100.64/10 +
     169.254/16) + learned corp CIDRs (sing-box accepts rule-sets here, so
     `corp_sync` can feed it). Corp/overlay traffic never enters rowt's tun;
     the kernel hands it to the VPN's tun directly — the Linux analog of
     "corp lane = direct, unbound", one layer down. Accepted loss: corp-lane
     logs/metrics don't see that traffic on Linux.
   - `strict_route: false`; exclude the VPN clients' own traffic via
     sing-box's CIDR/interface/UID primitives (leave `tailscale0` and the
     corp tun alone; pin the corp gateway endpoint direct).
   - **No-fighting rule:** rowt never re-asserts routing rules against
     another daemon. If the watchdog sees its capture rule shadowed, it
     journals the event and fails open (traffic flows un-laned) — same
     philosophy as the captive handler stepping aside. Route arbitration
     stays corp-route's job if that daemon ever comes to Linux.
   - Whether a given corp VPN client ships a Linux build is open, but Tailscale does and
     has a documented sing-box interaction — coexistence is mandatory
     regardless.
3. **Discovery via systemd-resolved** (`resolvectl` or D-Bus): per-link
   domains ≈ `scutil --dns` scoped resolvers; DHCP search domains from
   NetworkManager when present. Same `DnsSnapshot` shape out.
4. **Watchdog as a long-running systemd service** with an internal tick
   (closest to launchd KeepAlive semantics), not a timer firing oneshots.
5. **VM subsystem is not ported** — compiled out on Linux.

## 5. Migration plan — strangler, every phase ships alone

Ordering rule: highest testability × lowest platform risk first. The bash
monolith stays the daily driver until Phase 4; each phase is individually
revertible and gated.

| Phase | Scope | Parity gate (mechanisms in §6) |
|---|---|---|
| **0** ✅ | Characterization: generate + harvest the corpus (§6.1), build the differential harness (§6.2) and platform shims (§6.4), write the coverage ledger (§6.8), commit synthetic fixtures | **done** — `tests/parity/`. Mask makes all 38 read-only commands byte-stable across runs; 92-verdict classifier golden; ledger green on all 37 command arms; containment verified (sandboxed `proxy on` / `up` / `down` leave the live system untouched) |
| **1** ◑ | `rowt-core::render` — replace the giant jq program. bash calls `rowt-rs render` internally. | **render done** — `crates/rowt-render`, 18/18 cases canonically identical on host + vm (`parity render-matrix`), and identical against the real 22-server config. Remaining: throwaway-port outbound oracle, bash cutover, shadow window |
| **2** ✅ | classify/explain, lane set logic, absorb `corp-sync-reconcile.py` | **done** — classify: 9/9 cases × 92 destinations identical on `(lane, reason)`; lane edits: 12/12 cases identical across all three files + messages; reconcile: 210 generated cases identical to the Python. `selftest` 9/9 |
| **3** ◑ | watchdog: FSM into core, effects via `PlatformMac`; `cmd_watch` execs the Rust tick | **FSM + shadow done** — `rowt-core::watch`, 17 unit tests replaying §11's decision table; `parity watch-diff` 5/5; `ROWT_WATCH_SHADOW=1` compares the shell's decisions against the FSM's plan on every real tick, feeding it the tick's own captive verdict rather than re-probing. Remaining: `PlatformMac` effects, the `cmd_watch` cutover, and **time** — the shadow window itself |
| **4** ◑ | CLI. Built as `rowt-rs` alongside the shell first, so each command lands with evidence; only then does bash reduce to a wrapper and get deleted. Formula ships the prebuilt binary (monitor-asset pattern). | **all 37 arms native** — `parity cli-diff` compares stdout, exit status, the **whole config tree** (content and mode), the **argv trace** and the **audit log** over 215 cases. Remaining: `config import`, then the `ROWT_IMPL` cutover |
| **5** | `PlatformLinux` + tun mode + systemd units; CI matrix (macOS + ubuntu — core tests run on both, platform tests feature-gated); linux tar assets | fresh-VM install → onboard → probe → captive drill; VPN-coexistence drill with Tailscale up |
| 6 ◑ | port the import pipeline (1,302 lines of parsing Python). No longer optional — the 2026-08-09 decision is one language in the repo, and this is what retires `depends_on "python@3.12"`. | **all four done** — `rowt-core::{sharelink,importmerge,foreign,srimport}` (+`bplist`); `parity vless-diff` 2,000 cases, `merge-diff` 1,500 (the review FILE plus the streams), `foreign-diff` 1,200 (synthetic client config TREES), `sr-diff` 1,200 (synthetic Shadowrocket installs). What remains is the CUTOVER: `bin/rowt` still calls the Pythons, which are the reference side of those gates |

#### What still reaches for bash, and the order it comes back in

Every one of the 37 command arms answers natively. What remains are SUB-arms of
arms that are otherwise ported:

    config import                   prompts on /dev/tty

`native()` deliberately does not claim it, and hands it through the §6.6
fallthrough instead. A listed-but-unimplemented arm is worse than an unlisted
one, and `selftest` 23 asserts the two agree.

So the order is fixed rather than a preference:

1. ~~**the import pipeline -> Rust.**~~ Done: `vless-parse.py`
   (`rowt-core::sharelink`), `import-merge.py` (`::importmerge`),
   `foreign-import.py` (`::foreign`) and `sr-import.py` (`::srimport`, on
   `::bplist`), each behind its own differential gate.
2. ~~**the shell around it -> Rust.**~~ Done: `crates/rowt-cli/src/pool.rs` —
   `rebuild_servers`, `after_import`, `server add|rm|clear`,
   `sub add|rm|update|clear`. Little logic, but it is what writes
   `servers.json`, so `parity cli-diff` grew a config-tree comparison
   (content AND mode) first: every one of those arms is a file writer whose
   stdout is a courtesy, and the gate could not see the files.
3. ~~**`server import` / `sub import`.**~~ Done:
   `crates/rowt-cli/src/importer.rs` — `--detect`, the four `--from`
   extractors, `--apply`, and the two dump restores. The I/O the extractors
   need moved out of the gate binaries into `rowt-core::{foreignio,srio}`
   first, so the product calls a library rather than exec'ing a gate artifact.
4. **`config import`** — the odd one out, because it prompts on `/dev/tty`;
   the gate has no terminal, so this needs a scripted-input case first.

All 32 help pages render in Rust. Three of them only ever LOOKED computed:
`\$(` in an unquoted heredoc is two literal characters — how a page shows the
reader a command to type — and the build-time classifier tested for `$(`
without checking the backslash. The two that really are computed (`escape|corp`
is one page for two lanes, `block` names its log through `$(lane_log block)`)
go through a small evaluator that understands exactly the two shapes the text
uses. Anything else still returns `Detail::Shell` and delegates: a page the
evaluator does not understand is better rendered by the one interpreter that
certainly agrees with the shell.

Getting the escapes wrong is invisible without a comparison — a page with
`\$SHELL` expanded to the value of `$SHELL` still reads perfectly well — so the
gate now has a case per page rather than the seven that stood for 32.

The fallthrough is guarded against recursion (`ROWT_DELEGATED`), because
bin/rowt already reaches for Rust binaries and §6.6 has it becoming a wrapper.

**Python.** The decision (2026-08-09) is to port all of it — one language in
the repo. It splits into three groups with different urgency, not one job:
`corp-sync-reconcile.py` was already done and `fake-portal.py` is a test
harness; `net-detect.py` + `geosite-lookup.py` (~330 lines) are pure parsers on
hot paths and went next; the import pipeline (`vless-parse`, `foreign-import`,
`sr-import`, `import-merge` — 1,302 lines, 73% of the total) was last, because
it parses CREDENTIALS, where a subtle mis-parse yields a silently wrong
outbound rather than an error. Sequenced AFTER the bash port finished: two
rewrites converging on the same files is how a differential harness stops
telling you which side broke.

**All of it now has a Rust counterpart** — the last, `sr-import.py`, as
`rowt-core::{srimport,bplist}` behind `parity sr-diff`. What is left is not
translation but CUTOVER: `bin/rowt` still shells out to `config/*.py`, and the
Pythons stay in the tree until it does not, because they are the reference side
of every gate. Retiring `depends_on "python@3.12"` from the Formula is the
concrete prize and it lands with the cutover, not with the last port.

The cutover is happening arm by arm rather than at once. `rowt-rs` no longer
runs a Python for anything; `bin/rowt` still does, and that is what keeps the
two comparable. The scripts stay in the tree until the shell itself is retired,
because they are the reference side of every gate — and
`depends_on "python@3.12"` stays with them, since bin/rowt is what the Formula
installs.

`vless-parse.py` (421 lines) landed first of that group, as
`rowt-core::sharelink` behind `parity vless-diff`; `import-merge.py` (172)
followed as `rowt-core::importmerge` behind `parity merge-diff`;
`foreign-import.py` (456) as `rowt-core::foreign` behind `parity foreign-diff`.
Three notes.

The existing tests are a **checklist, not a corpus**: `config/test_parse.py`
calls the parser functions in-process, so nothing in it can be replayed through
two binaries, and it never touches `parse_vless` or `parse_anytls` at all — the
whole Reality path, every transport branch, and the `sni`→`peer`→host fallback
had no coverage on either side. `config/test_foreign.py` is the same shape.
`config/test_import_merge.py` is the better case: it shells out, so its six
checks ARE replayable. Either way the assertions were carried over as Rust unit
tests and the gate's evidence comes from a generated corpus, which is what
reaches the shapes a hand-written test never does.

The importers' inputs are **directories**, which changes what a corpus is. A
case is a whole synthetic `$HOME` — a Clash profile tree, a V2Box SQLite store —
plus the PATH the run should see, because whether `yq` is present, missing,
failing or answering with non-JSON selects between code paths that behave
completely differently. Nothing is ever seeded from a real client install on
the machine, even where one exists.

They also **crash on inputs a user can produce**: a `reality-opts:` that is a
string, a `ZURL` column holding a BLOB, a `profiles.yaml` whose top level is a
list. Those are reproduced rather than tidied into clean failures — a port that
succeeds where the Python dies imports garbage into a server pool — but the
gate compares the exception TYPE and not the traceback, which is interpreter
detail. Fixing any of them is a separate commit under §6.7.

Most of the work is not the protocol logic but **Python's semantics underneath
it**, which is why `rowt-core` now carries `pyurl` and `pyjson`. `urlsplit`
splits userinfo at the LAST `@` and the FIRST `:`; `parse_qs` drops a blank
value so `sni=` reads as absent; `_first` unquotes what `parse_qs` already
unquoted, so `%2520` arrives as a space; `u.port or 443` turns port 0 into 443;
`base64.b64decode` discards stray characters but REFUSES a non-ASCII one
outright; `json.dump` defaults to `ensure_ascii=True` while `net-detect.py`
passes `ensure_ascii=False`, so the two need different writers. None of that is
visible in the protocol code, and all of it changes what an outbound points at.

Rough effort at this repo's session cadence: P0 ≈ a day, P1 2–3 d, P2 2 d,
P3 3–4 d, P4 2–3 d, P5 4–5 d — order of three focused weeks total, spreadable.

## 6. Parity: proving the rewrite leaves no gaps

A rewrite of daily-driver infrastructure fails in three distinct ways, and
each needs a different instrument:

| Gap class | Example | Instrument |
|---|---|---|
| (a) known behavior, implemented wrong | render emits the corp outbound without `domain_resolver` | differential testing (§6.2–6.4) |
| (b) behavior nobody wrote down | `sort` order of a lane list; `\|\| true` swallowing a failure; what an empty list file does | harvested corpus (§6.1) + shadow mode (§6.5) |
| (c) behavior only visible in environments we can't stage | corp net with the VPN up; a hotel portal; the plane | shadow mode in production (§6.5) |

Class (b) is the real risk. It cannot be closed by reading the bash carefully,
because the thing you fail to notice is exactly the thing you fail to
reimplement. So the plan leans on *harvesting* observed behavior and on
*running both implementations against reality* rather than on inspection.

### 6.1 The corpus is generated and harvested, not invented

rowt has been recording its own behavior for months. Measured on this machine
2026-08-08:

| Artifact | Content | Serves as |
|---|---|---|
| **generated from the lane lists** | every suffix plus its near-misses (`sub.suffix`, `suffixsuffix`, partial prefixes), every CIDR at network / broadcast / ±1, the PRIVATE_CIDRS boundaries, IDN, malformed lines | **the primary classifier corpus** — exhaustive by construction, and committable when generated from synthetic lists |
| `log/lane-*.log` | 311 distinct domains seen in the un-rotated logs (7 rotations each behind them) | a source of *real* domain strings only. **Not an oracle**: these are error logs — the third field is a failure string (`NXDOMAIN`, `network is unreachable`, `operation not permitted`), not a match rationale, so the sample is biased toward what broke (one corp domain is 24k of 25k corp lines; 48k block lines span 9 domains) |
| `log/audit.log` | 104 mutating invocations, **23 distinct command shapes** (`proxy on` ×17, `corp sync` ×7, `server import` ×5, …) | which commands actually matter, ranked by real use — the priority order for transcript tests |
| `log/discovery.log` | real network signatures (office, home, plane, hotspot, VPN-up) | the watchdog `Observation` corpus |
| `~/.config/rowt/` live tree | real lists, state, servers | render input (sanitized) |
| `rowt report` | wide-surface snapshot | a single-command regression canary |

**Security constraint:** this corpus contains employer-internal hostnames. It
stays **local** — a gate run on this machine, never committed. Fixtures that
go into the public repo are synthetic domains plus hand-written edge cases
(empty file, comments, duplicates, IDN, CIDR, malformed line, missing file).

### 6.2 The differential harness

A `parity` script runs both implementations against the *same* sandboxed
`XDG_CONFIG_HOME`, captures stdout / stderr / exit code, and diffs all three.

`_is_readonly()` in the bash is already a machine-readable manifest of the
command surface, split exactly where the harness needs it: read-only commands
(24 families) run live and unshimmed on both sides — roughly half the surface
covered for free — while mutating ones go through §6.4's shims.

### 6.3 Gating the render — and why `explain` cannot do it

**`explain` is not a view of the rendered config.** `cmd_explain` walks the
list files itself (`_list_hit`, `_longest_domain_hit`, `_private_hit`) and
says so in its own output ("a geosite rule-set may still match this — not
shown"). It is a *parallel model* of routing. A Rust render that dropped the
corp outbound's `domain_resolver`, reordered rules, or lost a geosite would
produce byte-identical `explain` output on both sides — the gate cannot fail
for the bug class Phase 1 introduces. So:

- **Canonical-JSON equality is the Phase 1 blocking gate** (`jq -S`, so key
  order is not noise). It is the only gate that inspects the artifact being
  rewritten. An intentional difference is rare enough to deserve a per-instance
  allow-list entry with a reason. `sing-box check` must also pass on both.
- **End-to-end decision oracle:** boot each rendered config on its own
  throwaway port (the technique proven in the 3.1.1 `domain_resolver`
  investigation) and dial the destination corpus through both, comparing the
  outbound actually selected via the Clash API / lane logs. This is derived
  from the artifact rather than from a second model of it.
  **Not needed while the configs are canonically identical** — identical JSON
  produces identical routing by definition, so running it would be theater. It
  becomes required the moment an intentional structural difference is
  allow-listed, because from then on equality no longer implies equivalence.
- **`explain` parity moves to Phase 2**, where classify/explain *is* the code
  under test and it is exactly the right oracle.

Note: `explain` runs a live HTTP probe when the router is up, so the harness
needs the router down or a `--no-live` flag added for determinism.

### 6.4 Side effects become diffable data

Mutating commands can't be run live twice. Put a recorder shim directory first
on `PATH` — `networksetup`, `launchctl`, `sudo`, `scutil`, `ipconfig`,
`netstat`, `curl`, `sing-box` — where each shim appends its argv to a trace
file and returns canned fixture output. Run both implementations, diff the
traces. "Did the Rust version set all three proxy protocols with the same
bypass list, in the same order, and skip the sudo when already off?" becomes a
text diff.

Argv traces alone are not enough — also **diff the sandboxed config directory
itself** after each mutating command (state file, lane lists, servers, modulo
timestamps). That is what proves the "on-disk formats unchanged" invariant of
§4 instead of merely asserting it, and it is the failure that would break the
monitor and collector mid-migration.

One pure function deserves its own golden by name: `_watch_sudoers_body()`
emits a sudoers file whitelisting *exact* command strings. Get it subtly wrong
and the watchdog silently loses the ability to flip the proxy — captive
handling then fails in precisely the environment that cannot be staged.

This also gives `bin/rowt` its first real behavioral test suite — worth having
even if the port stalled after Phase 2.

### 6.5 Shadow mode — the centerpiece

For each ported subsystem, before cutover: **bash stays authoritative, and
also runs the Rust implementation and diffs the result**, journaling
divergences to `log/parity.log` using the discovery journal's change-only
signature trick so the file stays small. Shadow output is never applied, so
the risk is zero.

**Bash captures the `Observation` once and hands it to the Rust tick** — the
shadow must never run its own captive probe or net-detect. Otherwise the two
implementations observe different instants, probe traffic to the four captive
hosts doubles, and `parity.log` fills with divergences that are timing
artifacts; a log you have learned to ignore is worse than no log. The
`watch_tick(obs, st) -> (actions, st)` signature exists for exactly this.

The coverage is real: the watchdog ticks continuously
across office, home, foreign networks, captive portals and VPN-up states —
class (c) environments that no fixture can stage, and exactly where the §4.1
coexistence questions live.

**Promotion rule per phase:** flip only after a real-use window with zero
unexplained divergences — suggest 14 days including at least one corp-network
day and one foreign-network day.

### 6.6 Cutover keeps a live escape hatch

`ROWT_IMPL=bash|rust` selects the implementation at runtime; the bash ships
alongside as `rowt-legacy` for one full release cycle and the Formula carries
both. Rollback is an env var, not a reinstall. Phase 4 deletes the bash only
after a release cycle in which the fallback was never needed.

### 6.7 Bash bugs are behavior until deliberately retired

If the Rust version "fixes" something the bash did wrong, that is still a
divergence. Parity lands first; the fix lands as a **separate** commit
afterward, with the golden update in that commit. Otherwise "it's a fix"
becomes the channel through which accidental regressions get laundered.

The running list — each is reproduced in Rust today, with a comment pointing
here, and each is a shell-side commit waiting to be written:

| Behavior | Where | Why it is not "just fixed" |
|---|---|---|
| `domain_suffix` matching has no dot boundary, so `example.com` also captures `xexample.com` | `_longest_domain_hit`, mirrored in `classify::longest_domain_hit` | It matches what sing-box's `domain_suffix` actually does, so "fixing" the explainer alone would make it *disagree with the router*. Both sides move together or neither does. |
| `resolve_ip` is defined **twice**; the second wins, so `explain` uses dig-then-dscacheutil and `probe` uses a different one | bin/rowt:1843 and :2853 | Two call sites currently depend on the two different behaviors. |
| A host render probes the interface **twice** — `build_escape_outbounds host` runs `detect_iface`, then `assemble_host` runs it again | bin/rowt:1203, :1304 | Six subprocess calls where three would do. Collapsing them changes the argv trace, which is a gate; it needs its own commit and a golden update. |
| A domain whose failures tie across two categories gets whichever `for (k in cc)` reaches first | `lane_errors`'s awk | Genuinely unspecified, so there is no behavior to copy. rowt-rs takes the lexicographically first to be repeatable. Do not build a fixture that hits this — it would compare two implementations against a coin flip. |

### 6.8 Coverage ledger

Every arm of `run_command`'s `case` plus every entry in `_is_readonly` becomes
a checklist row (command × representative args). A phase cannot be declared
complete while rows in its scope are unchecked — this is what stops "the
commands I remembered to test" from masquerading as the command surface.

### 6.9 What parity deliberately does not cover

- **Help-text prose.** Command names, subcommands and flags are gated; the
  wording around them is free to change (the skill depends on the names).
- **Error-message wording**, except the specific strings quoted in the rowt
  skill's debugging section — those are gated, the rest may drift.
- **Timing and performance.** Rust will be faster; that is fine. But the
  watchdog's timeout constants must be carried over as *data*, not re-derived
  by judgement.
- **Interactive flows** (`onboard` prompts) — manual checklist, not automated.
- **Environments never visited during the shadow window.** Mitigated by
  fail-open design (§4.1.2) and the journal, not by tests.

## 7. Risks

- **sudoers coupling (macOS):** the watchdog's sudoers line whitelists exact
  commands. Cutover to a compiled binary must update it in lockstep — and a
  root-invoked compiled binary in the brew prefix is a *better* posture than
  a user-writable script (per the existing "root must not exec user-writable
  scripts" rule).
- **Render drift:** any silent divergence changes routing. Mitigated by
  canonical-JSON equality plus the throwaway-port outbound oracle (§6.3) —
  note that `explain` looks like a gate here and is not one.
- **Dual implementations mid-migration:** bounded by strangler order — each
  subsystem cuts over atomically behind a single call site; no dual-write.
- **Linux tun privileges:** cap-granting via systemd is standard but is a new
  operational surface; documented as part of Phase 5's install story.
- **VPN coexistence on Linux:** an aggressive VPN client can shadow rowt's
  capture rules. Mitigated by design (§4.1.2): capture exclusions + fail-open,
  never re-assertion. Residual risk is degraded coverage (un-laned traffic),
  which the journal records — not breakage of the VPN.

## 8. What this buys beyond Linux

- The render becomes typed serde builders — the quoting-trap class of bug
  (the `bash -n` apostrophe incident) is structurally gone.
- The captive/recovery machine gets a real unit-test suite instead of
  live-fire system-proxy toggles.
- State keys become an enum instead of stringly `sget`/`sset`.
- The monitor and collector read state through shared `rowt-core` types
  instead of re-parsing files.
- `cargo clippy` + tests replace `bash -n` as the strongest static gate.
