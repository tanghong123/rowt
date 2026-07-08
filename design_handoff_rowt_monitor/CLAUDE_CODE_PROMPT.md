# Claude Code kickoff prompt — build `rowt monitor` (TUI)

Paste everything below into Claude Code, with this `design_handoff_rowt_monitor/` folder
present in the repo (or attached).

---

You are implementing **`rowt monitor`**, a read-only terminal UI (TUI) for observing a
running `rowt` proxy. A complete design handoff is in `design_handoff_rowt_monitor/`.
**Read it first**, in this order:

1. `README.md` — the authoritative spec: layout & reflow, the two panes, server-health
   strip, data provenance, design tokens, and the **Interactions & keymap** section.
2. `renders/rowt-monitor-{96x30,150x38,212x52}.ansi` — ground-truth terminal renders.
   `cat` them in a truecolor terminal; every line is exactly `W` columns. Reproduce the
   layout, colors, glyphs, and the 130-column side-by-side ↔ stacked reflow faithfully.
   The `.txt` files are the same frames with color stripped (layout only).
3. `Rowt Monitor TUI.dc.html` — an interactive HTML prototype (open in a browser). It
   simulates live data and **demonstrates the interactions** (focus ring, arrow/hjkl nav,
   click/wheel focus, `y` yank, lane filter, window cycle, selected-row marquee, footer
   hint bar). Treat it as behavior reference, **not** as code to port.

## What to build
A real terminal application — **not** HTML. Pick the stack that fits the repo; if it's
greenfield, default to **Rust + `ratatui` + `crossterm`** (alternatives: Go
`bubbletea`/`lipgloss`, Python `textual`/`rich`). Render into a fixed character grid with
truecolor SGR and box-drawing borders. The HTML/CSS in the prototype is a picture of the
target, not the implementation — do not translate DOM/flexbox into anything.

## Ground rules (from the design)
- **Observer only.** Never mutate routing, servers, or the proxy. All data is *derived*
  on a **2-second tick** from: clash API (`127.0.0.1:9090` — `/traffic`, `/connections`,
  `/proxies`), `~/.config/rowt/host.json`, `~/.config/rowt/state/servers.json`,
  `~/.config/rowt/log/lane-*.log`, and host system facts. See the provenance table in the
  README for per-element sourcing.
- **Layout:** identity band (ASCII logo + facts) on top — **no** top header/clock line;
  two boxed panes (`live · connections`, `errors & blocked`) side by side ≥130 cols
  (one box split by a vertical divider) / stacked below 130; full-width boxed
  `server health` at the bottom. A `├──┼──┤` rule separates each pane's header from its
  list.
- **Lanes:** escape = `#7c9df0`, corp = `#56c7be`, direct = `#86c07a`, block = `#a98ad6`.
  Error categories: transient/`dns` = orange `#e0a35e`, persistent/`timeout|reset|refused`
  = red `#e0655e`, blocked = purple. `#conn` is *concurrency* — small, bounded,
  random-walking — never a monotonic counter. Block-lane traffic is excluded from the
  connections list and aggregate throughput.

## Interaction contract (implement fully — see README "Interactions & keymap")
- **Focus model:** exactly one focused list, shown by brightening that pane's border +
  caption. Focus + selection persist across the 130-col reflow.
- **Keyboard:** `↑↓`/`jk` move selection in the focused list (scrolls on overflow);
  `←→`/`hl` switch pane focus (fall-through between stacked panes); `Tab`/`Shift+Tab`
  cycle focus; `f` cycles lane filter (`1`/`2`/`3` jump, `0`/`Esc` clear); `w` or `[`/`]`
  change errors window; `y` yanks the selected row's key field; `p` pause; `?` help;
  `q` quit.
- **Mouse:** opt into **SGR 1006** tracking; wheel scrolls the list *under the pointer*
  and focuses it; clicking a lane/window/row activates it; draw a thin scroll indicator on
  overflowing lists.
- **Clipboard:** `y` yank is primary (copy selected domain / host:port). Also support
  app-level **drag-select** with your own highlight render. Transport via **OSC 52** with
  a clipboard-crate fallback (`arboard`/`copypasta`). **Paste is out of scope** (read-only).
- **Overflow:** truncate long values with `…`; **marquee only the selected row's**
  overflowing field (optionally a config to show a footer detail line instead).
- **Footer:** one-line contextual key-hint bar (htop/less style).

## How to proceed
1. Confirm the target stack and where the app should live in the repo; scaffold it.
2. Stub the six data sources behind a `Source` trait/interface so the UI can run against
   **fixtures first** (use the values in the renders), then wire real adapters.
3. Build the layout + reflow to match the renders byte-for-byte in width; diff your output
   against the `.txt` files at all three geometries.
4. Layer in the interaction contract; verify against the prototype's behavior.
5. Keep it observer-only and dependency-light; document any deviation from the spec and
   why.

Ask me before adding scope (new panes, columns, writes, or config surface) — the design is
intentionally minimal.
