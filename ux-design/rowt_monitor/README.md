# Handoff: rowt monitor — TUI monitoring interface

## Overview
`rowt monitor` is a **terminal UI (TUI)** for observing a running `rowt` proxy: live
connections and their throughput, a rolling-window view of connection errors and
blocked domains, and outbound-server health. It is the live companion to the
`rowt` CLI — think `htop`/`btop`/`bandwhich`. On top of the observe-everything view
it adds a small set of **confirmed, reversible overrides** (server switch, lane
routing, system-proxy toggle — see [Control layer](#control-layer)); everything
else stays observe-only.

This bundle is the design reference for that TUI.

## About the design files
The files here are **design references authored in HTML** — a prototype showing the
intended layout, information hierarchy, color language, and behavior. They are **not
production code to ship**. The implementation target is a **real terminal
application** (e.g. Rust with `ratatui`/`crossterm`, Go with `bubbletea`/`lipgloss`,
or Python with `textual`/`rich`). Recreate the design using the target stack's idioms
— a fixed character grid, SGR/truecolor styling, and box-drawing borders.

Two views of the same design are included:

- **`Rowt Monitor TUI.dc.html`** — the interactive HTML prototype (open in a browser).
  It simulates live data on a 2s tick so you can see motion and the window/filter
  controls. Needs `support.js` beside it.
- **`renders/rowt-monitor-<W>x<H>.ansi`** — the **ground-truth terminal renders**:
  the interface captured at three terminal geometries, as real ANSI (24-bit SGR)
  output. `cat` them in a truecolor terminal to see exactly what the TUI should look
  like. `.txt` companions are the same frames with color stripped (layout only).
  This mirrors the `WxH` golden-capture convention in `claude-replay-eval/data/`.

> **How to view:** `cat "renders/rowt-monitor-150x38.ansi"` in any truecolor terminal
> (iTerm2, kitty, WezTerm, modern gnome-terminal). Resize your terminal to the stated
> `WxH` first for a faithful comparison.

## Fidelity
**High-fidelity.** Colors, glyphs, column layout, and reflow are final. The `.ansi`
renders are byte-exact for width (every line is exactly `W` columns). Reproduce them
faithfully.

---

## The one rule that governs everything: it is a terminal
Every visual decision below follows from the fact that this renders into a grid of
**identically sized character cells**.

- **No variable font sizes.** There are no hero numbers or fine-print labels. Every
  glyph occupies exactly one cell. The *only* per-cell attributes available are
  **bold** and **foreground color** — use those (not size) for emphasis and severity.
- **Spacing is measured in cells**, not pixels. Alignment is by column.
- **Charts are made of block glyphs.** Where a bar is wanted, it is drawn with
  `▁▂▃▄▅▆▇█` (vertical) or `█…▉▊▋▌▍▎▏` (horizontal, 1/8-cell sub-precision) — the
  htop/btop idiom. (The current design leans on numeric values + sparklines rather
  than large bars; see history below.)
- **Box-drawing** for all chrome: outer frame `╭─╮ │ ╰─╯`, panel captions
  `┤ label ├────`.

---

## Layout & reflow

The screen is one full-terminal frame with an outer rounded border. Top to bottom:

1. **Identity band (neofetch-style)** — the `rowt` ASCII logo on the **left**, session
   facts inline to its **right** in a two-column key/value grid (mode, uptime, server,
   router, proxy, config). The logo stays top-left (terminal convention). There is no
   separate "session" or "throughput" panel — those were folded into this band and the
   connections header respectively. (The old top **header line** —
   `rowt monitor — ~/.config/rowt … HH:MM:SS · refresh 2s` — was **removed**: the path,
   clock, and refresh cadence were low-value chrome; the band now starts directly under
   the frame's top edge.)
2. **Main area — two panes**, each drawn as a **fully closed box** (box-drawing
   borders on all four sides, caption in the top edge `╭─┤ label ├──╮`):
   - **Left · `live · connections`** — instantaneous state.
   - **Right · `errors & blocked`** — rolling-window aggregate.
   When the two sit **side by side** they share one box split by a single vertical
   divider (`┬ … │ … ┴`) so there is a continuous rule between them — no gap.
3. **Bottom · `server health`** — full width, boxed, two rows.

**Reflow breakpoint:** at **≥ 130 columns** the two main panes sit **side by side**
(one split box); **below 130** they **stack** as two separate boxes (connections above,
errors below). Server health is always full width. When side by side, the
`errors & blocked` pane is held to **no less than ~1/3 of the width** so it stays
readable on very wide terminals (otherwise `live · connections` would swallow the extra
space). See the three renders: `96x30` (stacked), `150x38` and `212x52` (side-by-side,
the wider one showing more rows and more server chips).

---

## Panes in detail

### Left — `live · connections`
Instantaneous; nothing here is windowed.

**Header block — one aligned rate table.** A `total` row (`all`) sits on top of the
per-lane rows and **shares the same columns** so everything lines up vertically:

| name | ↑ up-rate | ↓ down-rate | #conn (right) |
|---|---|---|---|
| `all` | aggregate up | aggregate down | total conns |
| `escape` / `corp` / `direct` | lane up | lane down | lane conns |

- `↑` orange, `↓` teal; the `all` row is bold white (the single throughput + count
  summary for the whole proxy). Lane rows carry the lane name in its lane color.
- The aggregate `↑`/`↓` values align under the per-lane `↑`/`↓`, and the total `#conn`
  aligns under the per-lane `#conn` — read straight down any column.
- Clicking a lane / pressing **Tab** filters the table to that lane (clicking `all`
  clears the filter); the active filter shows as a chip in the header caption.
  Block-lane traffic is **not** listed here (it carries no real connection — see below).

**Table** (`LANE · HOST:PORT · #conn · UP · DOWN · RULE`), one row per active
connection:
- `LANE` — routing outcome, colored + bold.
- `HOST:PORT` — destination (SNI / requested host) + port.
- `#conn` — **concurrent** connections/streams open to that host:port. Realistic values
  are small (~1–8); in the prototype this is a **bounded random-walk**, never a
  monotonic counter (an early version wrongly let it climb forever — do not do that).
- `UP` / `DOWN` — per-connection byte-rate, sampled each tick.
- `RULE` — the matched rule kind (`domain_suffix`, `final`, …).

### Right — `errors & blocked`
Everything here is aggregated over a **rolling window**, selectable with the
`5m · 10m · 1h · 24h` control (default `10m`; key **`w`** cycles it). Changing the
window re-aggregates the whole pane.

**Header block — three category stat rows** (one line each, aligned columns):

| Category | Meaning | Color |
|---|---|---|
| `transient` | `dns` failures — usually self-resolving | orange `#e0a35e` |
| `persistent` | `timeout` / `reset` / `refused` — **candidates to add to the `escape` lane** (shown with a `→ escape` hint) | red `#e0655e` |
| `blocked` | matched the block rule (ad/tracker/telemetry sinkhole) | purple `#a98ad6` |

Each row: `<label>  <count>  · <n> dom`.

**Table** (`COUNT · TYPE · DOMAIN`), one row per destination domain, **failed and
blocked mingled** and sorted by count:
- `COUNT` — occurrences in the window (bold white).
- `TYPE` — `timeout`/`reset`/`dns` for failures, `blocked` for sinkholed. Colored by
  the category above (the color alone carries the category — the old `●` dot before the
  domain was **removed** as redundant).
- `DOMAIN` — destination.

Columns are set with generous inter-column spacing (breathing room) rather than packed
tight.

> **Blocked labeling:** the block lane is a single ad/tracker/telemetry sinkhole. We
> deliberately show one label — `blocked` — rather than sub-categories, and represent
> the whole class in purple.

### Bottom — `server health`
Two rows:
1. **Stats row:** `<N> servers · <up> up · <down> down · active <name>`. The active and
   any down servers are summarized *here only*.
2. **Individual servers row:** the **idle-but-up** servers, as an
   auto-scrolling (marquee) strip of `NAME  <ms>` chips, sorted by latency, colored by
   threshold (green < 70ms, orange < 140ms, red ≥ 140ms). The active server and down
   servers are intentionally **excluded** from this row (already covered by the stats
   row). No probe-age / "next probe" text — kept minimal.

---

## Data provenance — where every dynamic entity comes from
Everything on screen is **observed** — derived, on a fixed **2-second tick**, from
one of the sources below. The only writes are the explicit [control layer](#control-layer)
overrides, and those go through the same `rowt` CLI the operator could type; the
data path itself never mutates anything.

| Source | What it is |
|---|---|
| **clash API** | Local control plane (`127.0.0.1:9090`): `/traffic`, `/connections`, `/proxies` (delay) |
| **config** | `~/.config/rowt/host.json` — run mode, bind interface, rule set |
| **state** | `~/.config/rowt/state/servers.json` — outbound server pool + last probe results |
| **logs** | `~/.config/rowt/log/lane-*.log` — per-lane connection-failure events |
| **system** | Host OS — process uptime, system-proxy toggle, active interface |
| **ui state** | Operator-local view state (pause, lane filter, window) — never sent upstream |

**Per element:**

| Element | Source | Derivation |
|---|---|---|
| `● LIVE` / `PAUSED` | ui state | Whether sampling runs (key `p`) |
| filter chip | ui state | Active lane filter (Tab / lane click); hidden when none. Scopes the connections table only |
| mode · bind | config | Run mode + bound interface from `host.json` |
| server + latency | state / clash API | Selected outbound node (state) + its latest probe RTT (`/proxies`) |
| router | clash API | Proxy-process liveness + listen port |
| proxy | system | OS system-proxy toggle + active interface |
| config | config | Parse/validate result of `host.json` (`OK` or error) |
| uptime | system | Elapsed since process start |
| aggregate ↑/↓ + N conns | clash API | Sum of all connections' byte-rate (`/traffic`) and count (`/connections`) |
| lane ↑/↓ + conn count | clash API | Per-lane sums over connections whose matched rule is in that lane |
| connection row (lane/host/#conn/up/down/rule) | clash API | Per-connection metadata + byte-rate from `/connections` |
| error/blocked category counts | logs | Failure events in the window, classed transient/persistent/blocked |
| error/blocked row (count/type/domain) | logs | Aggregated by domain over the window |
| window tabs | ui state | Selected rolling window; filters which log events count |
| server health summary | state | Pool size, up/down counts, active node |
| server chips (name + latency) | state / clash API | Idle-up nodes + latest probe RTT |

A print-ready version of this table is in **`Rowt Monitor - Data Provenance.dc.html`**.

---

## Behavior & logic decisions (from the design conversation)
- **Observe + confirmed, reversible overrides.** The data path is observe-only; on top
  of it a handful of keys apply reversible changes (server switch, lane routing, proxy
  toggle) via the same `rowt` commands — see [Control layer](#control-layer). Lifecycle
  and server-management stay in the CLI.
- **2s refresh tick.** Instantaneous values (rates, latency) are sampled each tick;
  windowed values (errors/blocked) are re-aggregated from logs over the selected window.
  **`p`** pauses sampling so figures can be read.
- **`#conn` is concurrency, not a total** — small, bounded, random-walking. Never a
  monotonically increasing counter.
- **Block-lane connections are excluded from the live connections list** and from the
  aggregate throughput (they carry no real transfer); the block class surfaces only in
  the errors & blocked pane.
- **Errors split by persistence:** `dns` = transient (orange), `timeout`/`reset`/
  `refused` = persistent (red). **Persistent failures are the signal for "add this
  domain to the `escape` lane."**
- **Failed + blocked are shown in one mingled, count-sorted list** (not two separate
  sections), with category encoded by **color** (no dot); the header keeps the
  per-category totals.
- **Server health** shows active/down only in the summary row; the scrolling chip row
  is the idle-up pool. Kept deliberately minimal (no probe-age, no legend).
- **Neofetch identity band:** logo top-left, facts inline right. The separate session
  and throughput panels were removed to reclaim vertical space; throughput lives in the
  connections header.
- **Top header line removed.** The `rowt monitor — path … clock · refresh 2s` row was
  dropped; the identity band now begins directly under the frame edge.
- **Panes are closed boxes.** `live · connections`, `errors & blocked`, and
  `server health` each carry full box-drawing borders. Side by side, the two main panes
  share one box divided by a single vertical rule (`┬…│…┴`) — no visual gap between them.
- **Errors pane min width.** When side by side, `errors & blocked` is held to ≥ ~1/3 of
  the width so it never gets starved on very wide terminals.

---

## Interactions & keymap

This section is the **interaction contract** for the implementation. The prototype and
renders show *layout and state*; the real terminal app owns the *mechanism* (raw-mode key
events, terminal mouse tracking, clipboard, redraw loop). Build the mechanism in the target
stack — do not try to reproduce browser DOM event handling.

### Focus model
There is always exactly **one focused region** — `live · connections`, `errors & blocked`,
or the `server health` strip. Focus is shown by **brightening that region's caption** (the
single split box can't carry a per-pane border ring, so the caption is the cursor). `Tab`
cycles all three. Focus **persists across a resize/reflow** at the 130-column breakpoint.

Selection is **deferred, locked, and non-sticky**:
- A focused pane starts with **no** highlighted row. The first `↑`/`↓` locks onto a row **by
  its domain key** and tints the caption amber — the lock follows that domain even as the
  list re-sorts each tick (so a control acts on the domain you meant), and `Esc` releases it.
- **Leaving a region forgets its selection.** Move focus away (`Tab`, `←`/`→`, a click, a
  vertical fall-through) and the pane's selection clears, so re-focusing it starts fresh
  rather than restoring a stale highlight.
- **Server strip:** it keeps marqueeing until the first `←`/`→`, which **freezes the ring at
  its exact current scroll offset** (nothing jumps — a partially-shown chip may sit before
  the selection) and selects the first **fully-visible** chip. Moving `←`/`→` **wraps** at
  the ends and scrolls the frozen ring one cell at a time to keep the selection in view; the
  frozen ring is **circular**, wrapping past the last chip back to the first so the row stays
  filled.

### Keyboard
- **Arrows / `hjkl`** — movement.
  - **Up/Down** (`k`/`j`): move the selected row **within** the focused list; the list
    scrolls once content exceeds the visible height. Arrows are always live (they move the
    selection highlight even when everything fits) — they are **not** gated on overflow.
  - **Left/Right** (`h`/`l`): move focus **between** the two lists when they are side by
    side (≥ 130 cols). When **stacked** (< 130), Left/Right are a no-op; instead Down off
    the bottom of the upper list falls through into the lower list (and Up back up).
- **`Tab` / `Shift+Tab`** — cycle the focus region (forward / backward). This is the *only*
  job of Tab now (it no longer touches the lane filter).
- **`f`** — cycle the **lane filter** (all → escape → corp → direct → all). Scopes the
  connections table only; the active filter shows as a chip in the pane caption.
  - **`1` / `2` / `3`** — jump straight to escape / corp / direct.
  - **`0`** or **`Esc`** — clear the lane filter.
- **`w`**, or **`[` / `]`** — change the errors rolling window (`5m · 10m · 1h · 24h`).
  `w` cycles; `[` / `]` step down / up. Re-aggregates the whole errors pane.
- **`y`** — **yank** (copy) the selected row's **domain** to the system clipboard (both
  panes copy the bare hostname, no `:port`, so it drops straight into a browser / rule).
  This is the primary, precise copy path (see Clipboard below).
- **`p`** — pause / resume sampling (freezes the numbers so they can be read).
- **`?`** — toggle the help overlay.
- **`q`** — quit.

### Mouse
Requires the app to opt into terminal mouse tracking (**SGR 1006**, plus any-motion `1003`
for hover) and hit-test events against cell regions itself.
- **Wheel** scrolls the list **under the pointer** — and only that list. Wheel over a pane
  also **focuses** it, so keyboard and mouse never disagree about what's active.
- **Click a row / lane / window tab** to activate it — a row click focuses that pane and
  locks the selection (same as arrowing to it); a lane row / window tab click = `f` / `w`.
- **Click a server chip** to focus the strip and select that chip **in place** (the ring
  freezes exactly where it is — the clicked chip doesn't move). Both partially-shown edge
  chips are clickable.
- **Click `sys proxy on/off`** (identity band) to toggle the system proxy — the same action
  as `o`, and it **hover-highlights** (brighten + underline) while the pointer is over it.
- **Scroll indicator:** draw a thin position tick / track (`▐`, btop-style) on any list
  that overflows, so there is a visible cue that more rows exist.

### Clipboard (copy without the Shift workaround)
Enabling mouse tracking (above) **disables the terminal emulator's native drag-select** —
that is exactly why users fall back to holding **Shift**. To avoid that, copy is
implemented **in the app**:
- **Primary — `y` yank:** copies the selected row's key field. In a monitor almost every
  copy is "grab that one domain/host," so a precise yank beats drag-select and needs no
  pointer gymnastics.
- **Secondary — app-level drag-select:** the app tracks the mouse drag, **renders its own
  selection highlight** over the covered text, and copies the selection on release. This
  covers arbitrary-text copy without Shift.
- **Transport:** write to the clipboard via **OSC 52** (works over SSH, no X/Wayland
  dependency) with a **clipboard-crate fallback** (e.g. `arboard` / `copypasta`) for
  terminals that block OSC 52. Note OSC 52 payload-size limits and that some terminals
  disable it — degrade gracefully.
- **Paste** is **out of scope** for the read-only monitor (nowhere to paste). Revisit only
  if a text-entry field — e.g. a typed filter box — is added later.

### Overflow / marquee
Long values (a `domain` or `host:port` wider than its column) are **truncated with `…` by
default**. Only the **focused/selected row's** overflowing field **marquees** (horizontal
auto-scroll) — never every overflowing cell at once, which is noisy and burns redraws.
(The server-health chip strip already marquees as a whole, since it is a single passive
ticker.) A calmer alternative worth supporting behind config: a one-line **detail/footer**
that shows the full value of the selected row instead of animating it.

### Footer hint bar
Render a **single-line contextual key hint** at the bottom (htop / less style). Two states:
- **Normal** — a **global** group that's always present (`↑↓←→ navigate · f lane · w window ·
  o proxy · p pause · ? help · q quit`), then, **only when** a selection/strip makes them live,
  a **vertical-bar (`│`) divider** and a **contextual** group (`e·c·b·d route · y copy`, or
  `←→ select server` / `u use <tag>` on the strip). The contextual group is the **same colour**
  as the global keys (the bar, not colour, sets it apart). The right edge carries the
  pending-reload countdown chip or a transient status toast.
- **Armed** — the whole bar becomes the amber **confirm bar** (see [Control layer](#control-layer)).

This is better discoverability than the hidden `?` overlay; keep the overlay as the full
reference.

### Consolidated keymap
`↑↓`/`jk` move (first press locks the row) · `←→`/`hl` switch pane / pick server chip ·
`Tab`/`Shift+Tab` cycle focus (conns → errors → health) · `f` lane filter (`1`/`2`/`3`
jump, `0` clear) · `w` or `[`/`]` errors window · `y` yank selected domain · `e`/`c`/`b`/`d`
route selection → escape/corp/block/direct (confirm) · `u` use selected server · `o` toggle
system proxy · `↵` confirm · `Esc` cancel/unlock/clear · `p` pause · `?` help · `q` quit.

### Control layer
A small set of **confirmed, reversible overrides** layered on the read-only view. Each is a
front-end to an existing `rowt` command — the monitor issues exactly what the operator could
type, no new privileged surface. Contextual: a key is only live when there's something to act
on (a locked row for `e`/`c`/`b`/`d`, a selected non-active chip for `u`; `o` is always live).

| Key | Where | Action | `rowt` command | Reverse |
|---|---|---|---|---|
| `e` / `c` / `b` | locked conn/err row | route domain → **escape** / **corp** / **block** | `rowt <lane> add <domain> --no-reload` | `d` |
| `d` | locked conn/err row | remove domain → **direct** | `rowt escape/corp/block rm <domain> --no-reload` | `e`/`c`/`b` |
| `u` | selected server chip | switch active outbound server (live) | `rowt use <tag>` | `u` on the old server |
| `o` | global · or click `sys proxy` | toggle the macOS system proxy | `rowt proxy on` / `off` | `o` again |

- **Confirm model.** The lane edits (`e`/`c`/`b`/`d`) **arm** on the first press — an **amber
  confirm bar** replaces the footer previewing the change (`CONFIRM  x.com → escape · press
  the key again or ↵ to apply · esc cancel`) and auto-cancels after ~5s. A second press of the
  same key, or `↵`, commits; any other key or `Esc` cancels. So a quick **double-tap commits
  with no pause**. `u` and `o` are live and trivially reversible, so they **skip the confirm**
  and apply on a single press.
- **Optimistic proxy toggle.** `o` (and the `sys proxy` click) flips the displayed state
  **immediately** rather than waiting for the ~2s state re-read, so it feels instant; the real
  polled state then confirms it, or — if the underlying `rowt proxy` command failed — the
  display **reverts** after a short timeout with the error surfaced in the toast.
- **Debounced reload.** Lane edits write with `--no-reload`; a single router reload fires **~7s
  after the last edit settles** (a footer chip counts it down), so a burst of edits bounces
  sing-box once, not per keystroke. (The reload re-renders + restarts the router **without**
  re-asserting the system proxy, so it doesn't fight the `o` toggle / captive-portal flow.)
- **Feedback.** A control's outcome shows as a transient footer toast; a failed command (e.g.
  `proxy on` with the router down) surfaces its stderr rather than silently no-op'ing.

> **Note:** the interactive HTML prototype now **demonstrates** most of this contract —
> the focus model (a highlight ring on the active pane), arrow / `hjkl` + `Tab`
> navigation, click- and wheel-to-focus, row selection, `f` / `1`–`3` / `0` lane filter,
> `w` / `[` / `]` window, `y` yank-to-clipboard, the selected-row marquee, and the footer
> hint bar. What it **cannot** faithfully model — and what the real app must own — is
> raw-mode key handling, SGR-1006 mouse tracking, OSC-52 clipboard transport, and
> app-level drag-select rendering (the prototype falls back to the browser's native
> selection for arbitrary-text copy).

---

## Design tokens

**Palette (truecolor / hex):**

| Token | RGB | Hex | Use |
|---|---|---|---|
| border | 76,80,100 | `#4c5064` | frame, box-drawing |
| dim | 139,144,164 | `#8b90a4` | secondary text |
| dimmer | 101,106,130 | `#656a82` | labels, table headers |
| bright | 233,236,243 | `#e9ecf3` | primary values |
| escape lane | 124,157,240 | `#7c9df0` | escape (blue-purple = logo color) |
| corp lane | 86,199,190 | `#56c7be` | corp (teal) |
| direct lane | 134,192,122 | `#86c07a` | direct (green) |
| block lane | 169,138,214 | `#a98ad6` | block / blocked (purple) |
| up | 224,163,94 | `#e0a35e` | upload arrow/rate; also `transient` |
| down | 86,199,190 | `#56c7be` | download arrow/rate |
| persistent | 224,101,94 | `#e0655e` | persistent failures (red) |
| latency ok/warn/bad | green / orange / red | `#86c07a` / `#e0a35e` / `#e0655e` | `<70` / `<140` / `≥140` ms |
| surface | — | `#101116` / `#16171e` | pane / screen bg (HTML mock only) |

> **Lane colors:** escape = blue-purple (matches the ASCII logo), corp = teal,
> direct = green, block = purple. (We swapped escape/corp mid-design so escape carries
> the logo hue.)

**Glyphs:** arrows `↑ ↓`, status `● ▶ ✕ ·`, box `╭ ─ ╮ │ ╰ ╯ ┤ ├`, bars
`▁▂▃▄▅▆▇█` / `▏▎▍▌▋▊▉█`, ellipsis `…`, middot `·`.

**Type:** a single monospace family at one size. Emphasis via **bold** + color only.

---

## State (ui-local)
- `paused: bool` — freezes sampling (`p`).
- `laneFilter: 'escape'|'corp'|'direct'|null` — Tab-cycled; scopes connections table.
- `errWindow: '5m'|'10m'|'1h'|'24h'` — default `10m` (`w`); scopes errors & blocked.
- Ring buffers: ~26 samples for throughput/lane-history sparklines; window buffer for
  error events.

## Files in this bundle
- `Rowt Monitor TUI.dc.html` — interactive HTML prototype (+ `support.js`).
- `renders/rowt-monitor-{96x30,150x38,212x52}.{ansi,txt}` — ground-truth terminal
  renders at three geometries (stacked vs side-by-side reflow). `cat` the `.ansi`.
- `Rowt Monitor - Data Provenance.dc.html` — printable provenance table (+ `doc-page.js`).

## Assets
None external. The `rowt` wordmark is ASCII art (reproduce verbatim). All glyphs are
Unicode box-drawing / block characters. No images or icon fonts.
