# rowt monitor — design

A terminal UI for observing a running `rowt` proxy: live connections and
throughput, errors/blocked over a rolling window, and outbound-server health —
plus a small set of **confirmed, reversible controls** (server switch, lane
routing, system-proxy toggle) layered on top. The `htop`/`btop` companion to the
`rowt` CLI.

This document is the full design reference — the product/UX design **and** the
engineering internals. The frozen, pixel-exact UX handoff (spec + ground-truth
renders + HTML prototype) lives separately in
[`../ux-design/rowt_monitor/`](../ux-design/rowt_monitor/); this doc summarizes
it and then goes under the hood.

---

## 1. Goals & principles

- **Observe + confirmed, reversible overrides.** The data path is pure
  derivation and never mutates anything. On top of it, a handful of keys apply
  **reversible** changes — server switch, lane routing, the system-proxy toggle —
  each a front-end to the exact `rowt` command the operator could type, gated by
  an explicit confirm for the routing edits (see §6). Lifecycle and
  server-management stay in the CLI. (The monitor also *actively measures* server
  latency — a delay probe through the tunnel, like `rowt ping` — which changes no
  routing but is real network work, so it runs off the UI thread on a gentle
  cadence.)
- **It is a terminal.** The whole thing renders into a fixed grid of
  identically-sized character cells. Emphasis is **bold + foreground color**
  only (no font sizes). Chrome is box-drawing; bars/sparklines are block glyphs.
- **Always renders something.** Missing/unreachable data degrades per-field; if
  there is no `rowt` config at all, it falls back to a demo fixture.
- **Responsive.** The 2s data tick and all I/O must never block the input/redraw
  loop. This drove several decisions below (no streaming `/traffic`, background
  prober, incremental log reads).
- **Cheap.** IO, CPU, and RAM scale with *new* data, not with log/pool size.

---

## 2. Product / UX design

The authoritative UX spec — layout & reflow, the two panes, the server-health
strip, the design tokens (the full color/glyph palette), the data-provenance
table, and the interaction contract — plus the byte-exact ground-truth renders
and the interactive HTML prototype, all live in
**[`../ux-design/rowt_monitor/`](../ux-design/rowt_monitor/)**. That is the
source of truth for *what it looks like and how it behaves*; this document does
not restate it.

For orientation, the frame is: a single **outer frame** (the only rounded
corners) enclosing an **identity band** (logo + session facts + status dot) on
top; **`live · connections`** and **`errors & blocked`** panes side by side,
split by a center rule; and a full-width **`server health`** strip. Panes and the
strip are not inset boxes — they connect straight into the frame with `├ ┤` rules
(`┬ ┼ ┴` at the column split), and server health rides the closing `┴` merge
rule with one breathing row above it. Two things the engineering below leans on
that the handoff introduces: the lane color language (escape/corp/direct/block)
and the error categories (dns=transient, timeout/reset/refused=persistent,
blocked). Behaviors added *after* the frozen capture — the LIVE/DOWN/ERROR/PAUSED
status dot, the lane filter also scoping errors, cumulative-bytes columns, etc. —
are described where they're implemented (§5, §6) and catalogued in §10.

---

## 3. Architecture

Rust + [`ratatui`](https://ratatui.rs) + `crossterm`. A single binary,
`rowt-monitor`, invoked by the `rowt monitor` subcommand (and runnable
standalone).

```
src/
  main.rs      entry, arg parse, terminal setup/teardown, the event loop
  lib.rs       library surface + render_text() (used by tests and --render)
  app.rs       App: UI-local state + the update(Action) reducer
  input.rs     crossterm KeyEvent/MouseEvent -> Action
  ui.rs        the renderer: paints the whole frame into a ratatui Buffer
  paint.rs     small cell-painting helpers (put, put_right, hfill, truncate)
  theme.rs     truecolor design tokens + helpers (pulse, latency_color, …)
  format.rs    byte-rate formatters (rate_parts, compact)
  clipboard.rs OSC 52 + arboard fallback
  model.rs     data model (Snapshot, Conn, ErrRow, Server, Identity, Lane, …)
  source/
    mod.rs     the Source trait
    fixtures.rs FixtureSource — reproduces the golden renders (+ jitter for demo)
    live.rs    LiveSource — the real adapters
    parse.rs   pure parsers/aggregators (clash JSON, lane logs, buckets)
```

**Data flows one way.** A `Source` produces a `Snapshot` (an immutable
observation). `App` holds the latest snapshot plus UI-local state (focus,
selection, filter, window, pause). `ui::draw(buffer, area, &app, present)` is a
pure function of `(App, geometry)` → cells. Input events become `Action`s that
`App::update` folds into state; the next frame re-renders.

```
        ┌─────────── Source (fixtures | live) ───────────┐
        │  poll(window, lane_filter) -> Snapshot          │
        └───────────────────────┬─────────────────────────┘
   2s data tick / on-demand      │
        ┌───────────────────────▼─────────────────────────┐
        │  App  { snap, focus, selection, filter, window } │
        │  update(Action)                                  │
        └───────────────────────┬─────────────────────────┘
   input (keys/mouse -> Action)  │  every frame
        ┌───────────────────────▼─────────────────────────┐
        │  ui::draw(buf, area, &app, present) -> Hit        │
        └──────────────────────────────────────────────────┘
```

### 3.1 The event loop (`main.rs`)

Two cadences:

- **Data tick — 2s.** Re-`poll()` the source (unless paused).
- **Animation tick — ~70ms (~14 fps).** Redraw for the breathing dot and
  marquees. `ratatui` diffs the buffer, so an idle redraw repaints only the few
  changed cells.

`event::poll(ANIM_TICK)` provides the frame clock; a key/mouse event wakes it
sooner. **Animation is time-based, not iteration-based** — the pulse and
marquee offsets derive from `App::started.elapsed()`, not a per-iteration
counter. (An earlier counter-based version sped up when a burst of mouse-scroll
events woke the loop rapidly.)

Terminal setup: raw mode + alternate screen + SGR-1006 mouse capture, with a
panic hook and normal teardown that always restore the terminal.

---

## 4. Rendering

The renderer paints **absolute cells** into the `ratatui` Buffer (via `paint.rs`)
rather than composing high-level widgets. This is the only way to hit the
byte-exact column layout the design demands (section captions `─┤ label ├` on the
connecting rules, right-anchored numeric columns, the center divider, etc.).
Column offsets in `ui.rs` were measured from the ground-truth renders.

- **Single-frame topology.** `draw()` renders one outer frame (the only rounded
  corners); the two panes and the server strip connect into it with `├ ┤` rules.
  The identity band is followed directly by the split divider `├─┤ live ·
  connections ├─┬─┤ errors & blocked ├─┤`, then the summary rows, a `├─┼─┤` header
  cross, the data lists, one breathing row, and the `├─┴─┤ server health ├─┤`
  merge rule that closes the split and labels the strip in one go. The column
  split (connections/errors leftover ~5:2, errors held ≥~1/3) is two columns at
  every width — narrow terminals shorten the tab labels, not the topology. List
  row counts fill the available height. Focus + selection persist across reflow.
- **`present` vs interactive.** `draw(.., present)` takes a bool. `present=true`
  is a neutral "screenshot" state (no focus-brightening, no selection highlight,
  no marquee, full-brightness dot) — this is what `--render` and the golden
  tests use, so they match the frozen capture. `present=false` layers on the
  live affordances.
- **Selection** is a subtle background + brightened text + a `▎` accent bar in
  the row's semantic color (not reverse-video).
- **Overflow:** long values truncate with `…`; only the *selected* row's
  overflowing field marquees. A thin scroll tick marks overflowing lists.

### 4.1 Golden-render verification

`tests/golden.rs` renders the still fixture at each geometry via
`ratatui::backend::TestBackend`, extracts the plain-text grid, and asserts it
equals the captures in `ux-design/rowt_monitor/renders/*.txt` — byte-for-byte in
width. A `mask()` blanks the few regions that intentionally diverge from the
frozen capture (see §10) so the rest stays exact. A separate `colors_spot_check`
asserts key cells carry the expected fg/bold. `--render WxH` on the CLI is the
same path, for eyeballing.

---

## 5. Data pipeline

`Source::poll(window, lane_filter) -> Snapshot`. Two implementations:
`FixtureSource` (demo/tests) and `LiveSource` (real). `LiveSource` derives each
field from one of: the clash API, config/state files, the lane logs, or system
facts — degrading per-field.

### 5.1 clash API — connections & throughput

`GET http://127.0.0.1:{ROWT_CLASH_PORT}/connections` (Bearer = `clash_secret`
from the state file), short timeouts so a dead API never hangs the tick.

- **Per-connection rate** = delta of the cumulative `upload`/`download` byte
  counters between two polls, divided by the real elapsed time (`Instant`).
  Connection `id`s are stable across polls, so the delta is well-defined; a new
  connection reads 0 until the next sample.
- **Lane classification** from the connection's outbound `chains`: `block` /
  `corp` / `escape` (or any escape-selector member) / else `direct`. Block-lane
  connections are excluded from the list and from throughput.
- **Header rows = instantaneous rates** (summed per lane, `B/s`); **the
  per-domain table row shows cumulative bytes**, not a rate. Rationale: idle
  keep-alive connections move 0 bytes between 2s samples, so an instantaneous
  per-connection rate reads 0 almost always — cumulative totals ("what actually
  moved data") are the useful view, and match the design's large numbers.
- **Per-domain byte history** (so short-lived connections still count). clash
  drops a connection's cumulative counters the moment it closes, so a row built
  only from the *live* set loses the traffic of bursty domains. Instead the live
  source keeps a per-domain (`lane, host, port`) accumulator: each poll, any
  connection that vanished since the last one is *closed* — its last-seen
  cumulative bytes are folded into that domain's history (`accumulate_history`,
  before the rows are built). A row's total is then **live cumulative + history**.
  Domains with history but **no live connection** become **dormant rows** —
  concurrency `0`, greyed, sorted after the live rows (both groups by `up+down`
  desc). The history is session-scoped and capped (top 200 by bytes; smallest
  evicted) to bound RAM and the dormant-row count.
- We deliberately **do not** read clash's streaming `/traffic` endpoint: its
  first line can take ~1s to arrive, which blocked the UI thread every tick.
  Aggregate throughput is summed from per-connection deltas instead (a 2s
  average; a burst that starts and ends between samples is missed).

### 5.2 config / state (mtime-gated)

- `host.json` gives the run mode's interface, the config-OK check, and the
  escape server pool. It's ~19 KB, so it is parsed **only when its mtime
  changes** (a `stat` each tick is nearly free); the parsed view (`HostInfo`:
  escape set, members, iface, config-ok) is cached.
- `state` (key=value) gives `mode`, the `selected` server (or `auto`), and the
  `clash_secret`.
- `servers.json` is the pool source of record.

### 5.3 errors & blocked

This is the most engineered subsystem, because the lane logs are append-only and
can be large (the block lane is megabytes), and the pane must stay fresh (~6s) to
surface new blocked/failing domains without amplifying I/O.

**Incremental tailing.** Each `lane-*.log` is read by byte **offset**. The first
look seeds from the last 512 KB (recent history without reading the whole file);
every refresh reads only the bytes appended since (`[offset, EOF)`), advancing
the offset past the last newline (a partial trailing line is re-read next time).
`size < offset` means rotation/truncation → reset to 0. mtime/size-gated, so idle
logs cost nothing. Net: a 1 KB append costs a ~1 KB read and parses only the new
lines — IO and CPU scale with new data, not log size.

**Two stores, by lane cardinality:**

- **Sparse lanes** (escape / corp / direct) → a bounded rolling `Vec<ErrEvent>`
  (`{secs, domain, kind, lane}`), pruned to the widest window (24h) with a 40k
  hard cap.
- **Block lane** (high-volume, low-cardinality — thousands/day, a couple of
  domains) → **per-minute, per-domain counters**
  (`BTreeMap<minute, {domain -> count}>`). One-minute buckets are the coarsest
  that still support the 5-minute window (5 buckets, ≤20% boundary error); larger
  buckets would make the 5m window all-or-nothing. RAM: ~256 KB for a full 24h at
  2 domains, vs a 2.8 MB raw cap that held only ~2.5h. Block counts are
  bucket-accurate (~1 min) at the window boundary — fine for the block lane.

**Aggregation** (`parse::aggregate_split`) combines the sparse events (exact) and
the block buckets (bucketed) over the selected window into the three category
totals + a count-sorted per-domain row list. It runs **every 2s poll**, and the
window is referenced to **wall-clock now** — expressed in the same local
civil-seconds frame the lane logs use (system clock + local UTC offset from
`date +%z`, read once) — *not* the newest event timestamp. That is deliberate: a
newest-event reference froze the cutoff during a quiet spell, so entries older
than the window never aged out until a new event happened to arrive. Re-running
each poll against an advancing wall clock retires them in real time (in-memory,
no I/O). Log **reads** stay throttled to ~6s (fresh enough for new domains, cheap
enough not to re-read on every tick).

**Lane filter.** Each event carries its lane; `aggregate_split` filters the
sparse events to the active filter and drops the block category under a specific
lane (block is its own lane). This makes the connection-pane lane filter also
scope the errors pane — filter to `direct` to see exactly the timeout/reset/dns
domains that are escape candidates (the live equivalent of `rowt direct errors`).

### 5.4 server health — the prober

Manual-selector mode leaves clash's `/proxies` delay history empty, so reading it
would mislabel every server "down". Instead a **background thread** actively runs
clash delay tests through the tunnel (like `rowt ping`) and writes results into a
shared map; the UI reads the latest.

- **Target:** `https://www.gstatic.com/generate_204` (overridable via
  `ROWT_PING_URL`). Google's endpoint is blocked when direct, so it 204s only
  *through* a working escape — this tests real escape reachability, matching
  rowt's auto-select urltest (Cloudflare, the old default, is reachable even
  without a working escape and only proved the server was "up").
- **Cadence:** every 10 min (`ROWT_MONITOR_PROBE_INTERVAL` secs); first round
  runs immediately. The thread re-reads the pool + secret from config each round,
  so `server add` / `sub update` / a rotated secret are picked up without a
  restart. It waits on a channel with the interval as a timeout, so a **force
  signal** (below) wakes it instantly.
- **Force / self-heal.** `r` forces a re-probe; a pool-membership change forces
  one automatically; and — key for network switches — the monitor forces a
  re-probe when the **router transitions down→up** (reload / Wi-Fi change) and
  keeps re-probing ~every 60s while the active server is failing, so a stale
  `ERROR` from a pre-switch probe clears within seconds instead of waiting for
  the 10-min cycle.
- **Freshness.** A result is valid for 2× the interval (+margin); older than that
  is treated as *pending* (prober likely dead), never as "down". (Was a fixed
  90s, which emptied the strip between 10-min probes.)
- **Display.** `up` = last probe succeeded, `down` = tested and failed, pending =
  not yet probed (shown as `probing…` while the first round runs). All up servers
  appear in the strip, the active one marked `▶` and sorted first; it's not
  repeated in the stats line (it's already in the identity band). Latency is
  colored by threshold.

### 5.5 system facts

Process uptime (best-effort via `ps` on the sing-box process), the active
interface, the system-proxy state, and router liveness/port.

---

## 6. Interaction & input

`input.rs` maps crossterm events to `Action`s; `App::update` is the reducer.

- **Focus model:** exactly one focused region — the two lists **and** the server
  strip (`Focus::{Conn,Err,Health}`), cycled by `Tab`. Focus is shown by
  brightening that region's caption text (the single frame can't carry a
  per-pane border ring); the `┤ ├` connectors stay border-colored so they never
  look out of step. All focus changes route through `set_focus`, which **forgets
  the leaving region's selection** — re-focusing always starts fresh.
- **Deferred, locked selection:** a focused pane starts unselected; the first
  `↑/↓` locks a row **by its domain key** (not index) and tints the caption
  amber. `tick()` re-resolves the key to its current index each poll and drops it
  if the domain leaves the list — so the acted-on domain can't shift under the 2s
  re-sort. `Esc` releases it, and it also **auto-clears after 15s of input
  inactivity** (`SELECTION_IDLE_TIMEOUT`, checked in `on_frame`; any key/click
  resets the timer, hover doesn't) so a held selection / frozen strip doesn't
  stay stuck if the operator walks away — the panes then resume live scrolling.
- **Server strip:** focusing keeps the marquee running; the first `←/→` (or a
  click) **freezes it at the exact offset the renderer last drew** — the renderer
  feeds its marquee offset back each frame as `Hit::strip_render_off`, and App
  freezes to that value, so the frozen view is precisely the snapshot on screen
  (no jump; a partial chip may sit before the selection). It then selects the
  first fully-visible chip. Moves wrap at the ends and scroll the frozen ring one
  cell at a time to keep the selection visible; the frozen ring renders circularly
  (wraps past the last chip to fill the row). Strip viewport width also comes back
  via `Hit`. The marquee runs off a **resettable baseline** (`marquee_off0` at
  `marquee_t0`), not raw elapsed time: on **unfreeze** (Esc / focus-leave / idle
  timeout) the baseline is set to the frozen offset and the clock restarted, so it
  **resumes scrolling from where it stopped** rather than jumping to where a
  free-running clock would be.
- **Control layer** (§1): contextual keys act on the current selection —
  `e`/`c`/`b`/`d` route the locked domain to escape/corp/block/direct, `u`
  switches to the selected server, `o` toggles the system proxy. Each shells out
  to `$ROWT_BIN` (exported by `rowt monitor`) **off the UI thread**; outcomes are
  drained each frame into the footer toast (a failed command surfaces its
  stderr). Lane edits **arm** on first press (an amber confirm bar; re-press or
  `↵` commits, any other key / `Esc` cancels — so a double-tap commits with no
  pause) and are written with `--no-reload`; a single `render`+`router restart`
  fires ~7s after the last edit settles (a footer chip counts down). `u`/`o` skip
  the confirm (live + trivially reversible). The proxy toggle is **optimistic**:
  the displayed state flips immediately, then reconciles with the real polled
  state or reverts on timeout if the command failed.
- **Lane filter** is global (both panes) and shows as a `· <lane>` chip in both
  captions. Changing it re-polls immediately (cheap in-memory re-aggregation).
- **Mouse:** wheel scrolls (and focuses) the list under the pointer; clicking a
  row / lane / window-tab activates it; clicking a **server chip** focuses the
  strip and selects it *in place* (both partial edge chips are hit-tested);
  clicking **`sys proxy`** toggles it. Hover over `sys proxy` highlights it — this
  needs any-motion reporting (xterm `1003`, enabled alongside SGR-1006 capture and
  disabled on exit). Clickable regions are recorded into `Hit` each draw.
- **Clipboard:** `y` yanks the selected row's **domain** (bare hostname, both
  panes) via OSC 52 (works over SSH) with an `arboard` fallback; app-level
  single-row drag-select reads the covered glyphs back out of the buffer on
  mouse-up. Paste is out of scope. `ROWT_MONITOR_NO_CLIPBOARD=1` disables the real
  clipboard (tests).
- **Toast:** a transient footer message (copied… / re-probing… / control outcome)
  auto-clears after ~4s (timestamped).

---

## 7. Resource characteristics

Design target: work scales with *new* data, not with the size of logs or the
pool.

- **IO:** idle → only `stat`s. Active → incremental reads proportional to bytes
  appended (not the 512 KB tail). clash calls are local HTTP with short timeouts;
  the blocking stream endpoint is avoided.
- **CPU:** only newly-appended log lines are parsed each ~6s refresh (not the
  whole tail); the errors aggregation re-runs each 2s poll but is a cheap
  in-memory filter/group over the bounded event buffer (so the window can age
  out against the wall clock).
- **RAM:** sparse-lane events pruned to 24h + 40k cap; block lane collapsed to
  per-minute per-domain counters (sub-MB for a normal block lane); `host.json`
  parsed once per change. The rest of the snapshot is small.

---

## 8. Integration & packaging

- `rowt monitor` (`bin/rowt`) `exec`s the `rowt-monitor` binary, resolved next to
  `bin/rowt` in the Homebrew `libexec` (or a local `cargo` build), passing args
  through. Listed in `rowt help`; tab-completion surfaces it via the live command
  registry.
- **Homebrew:** the tap formula `depends_on "rust" => :build` and builds the
  crate into `libexec/bin` (also symlinked as `rowt-monitor`). Guarded on the
  `rowt-monitor/` dir existing so older tarballs still install.
- Shipped in **rowt v2.0.0**; the crate versions independently (`Cargo.toml`).

---

## 9. Testing

- **Golden diff** (`tests/golden.rs`): byte-exact plain-text match at 96/150/212
  vs the frozen captures, with masked divergences (§10) and a color spot-check.
- **Parsers** (`source/parse.rs`): clash JSON → connections/lanes/rates,
  timestamp/civil math, rule normalization, error classification, window
  aggregation, and the split (sparse + block-bucket) aggregation with lane
  filtering.
- **Interactions** (`tests/interaction.rs`): every key/action drives the expected
  state transition (movement/scroll, lane filter cycle/jump/clear + scoping,
  focus switch, stacked fall-through, window cycle/step/set, yank, pause/help,
  persistence across reflow).
- **Headless smoke:** `tmux` drives the real binary (send-keys / capture-pane) to
  confirm the frame renders, the filter chip appears, and it exits cleanly.

---

## 10. Intentional deviations from the frozen capture

The `ux-design/rowt_monitor/renders/` captures are a snapshot; a few things were
deliberately changed after review (each masked in the golden test and covered by
a dedicated assertion):

- **Logo bottom row** shifted one space left so its stems align with the rows
  above.
- **Identity right column** moved right (67/75 → 70/78) for breathing room, and
  the server-name column is reserved from the pool's longest name so the ms
  column is stable and never collides with `router`; latency shows `—` when
  there's no reading (name greys out).
- **Errors TYPE colored by category** (dns=orange, timeout/reset/refused=red,
  blocked=purple) — the color carries the category.
- **Server strip** shows the active server marked `▶` (the capture excluded it);
  the stats line dropped `active <name>` since it's in the identity band.
- **Connections table** drops the per-row `↑`/`↓` (redundant with the UP/DOWN
  column headers; header rate rows keep theirs), and shows cumulative bytes
  (§5.1). Header/lane rows show `—` when a row has no connections.
- **Status dot** distinguishes LIVE/DOWN/ERROR/PAUSED (the capture only had LIVE).

---

## 11. Open items / future

- **`brew install` under the sandbox** — `brew fetch` validates the tarball, but
  a full install compiles the crate under Homebrew's build sandbox, which may
  block crates.io. If so, vendor the crates (`cargo vendor` + committed vendor
  dir / resources).
- **Pin the active server** in the chip strip so it stays visible when the strip
  marquees, instead of scrolling out of view.
- **Domain interning** for the block buckets, if a block lane ever has dozens of
  always-on domains (would cut the per-bucket string duplication ~6×).
