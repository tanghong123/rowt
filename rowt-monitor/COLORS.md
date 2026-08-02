# rowt-monitor — color palette (as shipped)

The palette the TUI actually renders, in both themes. This supersedes the design
handoff's `COLORS.md`, which describes an older model of the UX; where the two
disagree, this file and `src/theme.rs` are the truth. **`tests/theme.rs` parses the
table below and asserts it against `theme::DARK` / `theme::LIGHT`**, so the doc
cannot drift from the code — and the token list is checked for exhaustiveness at
compile time, so a new `Palette` field can't be added without appearing here.

Layout, glyphs, and the control contract are identical between themes; a golden
test asserts a light frame is cell-for-cell identical to a dark one. See
[DESIGN.md §4.1](DESIGN.md) for the engineering side (theme detection, the
direction-aware helpers, golden regeneration).

## Tokens

Ratios are WCAG contrast against the background each column was tuned for
(`#16171e` dark, `#f4f5f7` light) — measurement references only; see
*Backgrounds* below.

| Token | Dark | :1 | Light | :1 | Renders |
|---|---|---|---|---|---|
| `bright` | `#e9ecf3` | 15.1 | `#14161d` | 16.6 | any value that is neither a label nor a state |
| `dim` | `#8b90a4` | 5.6 | `#4a4f63` | 7.4 | secondary text, lane values, help prose, a state with no reading, the light focus ring |
| `dimmer` | `#656a82` | 3.3 | `#6a708a` | 4.5 | every label; table headers |
| `border` | `#4c5064` | 2.2 | `#a9aec0` | 2.0 | the frame, all box-drawing, every rule |
| `escape` | `#7c9df0` | 6.8 | `#3546b8` | 7.1 | escape lane; the ASCII logo; the active server's name |
| `corp` | `#56c7be` | 8.8 | `#0b6b67` | 5.8 | corp lane |
| `direct` | `#86c07a` | 8.4 | `#2f6b23` | 5.9 | direct lane; `on` / healthy; the `● LIVE` dot and its label |
| `block` | `#a98ad6` | 6.2 | `#6d43ad` | 6.3 | block lane; `blocked` errors |
| `up` | `#e0a35e` | 8.1 | `#8a5208` | 5.8 | ↑ rate; `dns` (transient) errors; `ERROR`; armed edits; `off` |
| `down` | `#56c7be` | 8.8 | `#0b6b67` | 5.8 | ↓ rate |
| `persistent` | `#e0655e` | 5.3 | `#b32a22` | 5.9 | timeout/reset/refused; `DOWN`; a down router; ≥140 ms |
| `up_table` | `#c9a06e` | 7.4 | `#a8721f` | 3.8 | ↑ inside the connections table (de-emphasized) |
| `down_table` | `#6fb8b0` | 7.8 | `#2d817c` | 4.2 | ↓ inside the connections table (de-emphasized) |
| `lat_ok` | `#86c07a` | 8.4 | `#2f6b23` | 5.9 | latency `<70 ms` |
| `lat_warn` | `#e0a35e` | 8.1 | `#8a5208` | 5.8 | latency `<140 ms` |
| `lat_bad` | `#e0655e` | 5.3 | `#b32a22` | 5.9 | latency `≥140 ms` |
| `selection_bg` | `#262b3e` | — | `#e8eaed` | — | the selected row / chip — the only background painted |

Four tokens are aliases, not separate values: `transient` = `up`, `blocked` =
`block`, `armed` = `up`, and `border_focus` = `bright` on dark / `dim` on light.

**Every light foreground clears WCAG AA (4.5:1)** at the 1-decimal rounding above
— `dimmer` is the boundary case — except `up_table` and `down_table`, which are
deliberately recessive: each is always redundant with an adjacent AA-passing value
or glyph. Don't promote them. `border` is structure, not content, and matches the
dark theme's equally recessive frame.

## The session-facts band: three tiers, and only three

The head pane reads as exactly three kinds of thing. There is no fourth text
weight, and a cell belongs to exactly one tier:

| Tier | Token | Cells |
|---|---|---|
| **label** | `dimmer` | `MONITOR`, `uptime`, `mode`, `server`, `sys proxy`, `router`, `collector`, `watch` |
| **state** — working / failed / warning | semantic | `● LIVE`/`DOWN`/`PAUSED`/`ERROR` (dot *and* label), `on`/`off`, CPU %, latency, `down · <reason>`; `dim` when there is no reading |
| **everything else** | `bright` | `host · en0`, `running`, the uptime value |

One exception: the **active server's name** is `escape` purple, because it marks
*which* server, not how it's doing. Its latency carries the health.

Two consequences worth stating, because both diverge from the frozen capture:

- **The status label takes its dot's color in every state, healthy included.** The
  capture drew a green dot beside a white `LIVE` but a red dot beside a red `DOWN`,
  which made the healthy case the odd one out. `● LIVE` is one signal in two
  glyphs. Only the dot breathes; the label holds the un-pulsed color.
- **`router`'s value is plain `bright`.** It carries no state — the CPU % beside
  it does.

## Backgrounds

**A terminal owns its background.** The TUI paints foregrounds only, plus
`selection_bg` on a selected row or chip. It stores no screen color at all, and a
test scans every cell to prove the handoff's six "mock-only surfaces" (`#16171e`
`#101116` `#1c1e26` `#f4f5f7` `#fbfbfd` `#eef0f4`) never reach one, as foreground
or background.

That has two knock-on effects, since "toward the background" has no fixed value:

- **`emphasize`** (selected rows, hover) pushes a foreground *away* from the
  background — brighter on dark, **darker on light**. The brightening that lifts
  text off a dark terminal washes the same text out on paper.
- **`fade`** (the `● LIVE` pulse) sinks it *toward* the background — toward black
  on dark, toward **white** on light. White, not the mock's screen color: we never
  assume a specific paper.

The handoff's light selection, `rgba(20,25,50,.05)` over the screen, is composited
into the flat `#e8eaed` above, because a terminal cannot blend cell backgrounds.

## Themes

`--theme dark|light|auto` (or `ROWT_MONITOR_THEME`), default `auto`. `auto` reads
the terminal's *actual* background — `COLORFGBG` first (last field only; a
non-numeric `default` is no answer, not a guess), then an OSC 11 query with a
100 ms budget — and falls back to dark. `$TERM` is never consulted: it says which
escape codes a terminal understands, not what color it is.

**Near-paper guard.** The light column is tuned for a background at relative
luminance ≥ 0.75 (`#eaeaea`–`#ffffff`, warm paper like `#faf9f5` included). A
dimmer background stays on dark rather than washing out, so a mid-grey terminal
never gets the light palette.

## Tokens the handoff has that this doesn't

Five, each folded into a survivor — a terminal cell is not a CSS box, and every one
of them was an affordance that doesn't survive the translation:

| Deleted | Folded into | Why |
|---|---|---|
| `rule` | `border` | A 1.2:1 hairline is a CSS affordance. A terminal draws a rule as a full `─` cell row; at 1.2:1 on light that reads as a rendering fault. One frame color. |
| `dimmest` | `dimmer` | Three grey steps is one too many at a single terminal font size, and on light it landed at 3.4:1 — under AA. |
| `body` | `dim` | Only the help overlay wanted it; `dim` reads fine as prose in both themes. |
| `refused` | `persistent` | Mislabeled upstream — `#d3788c` was never an error kind. **One red** means "this is failing": persistent errors, DOWN servers, and ≥140 ms alike. |
| `value` | `bright` | A fourth text tier that only one cell ever used. "Everything else" is one weight. |

The handoff's HTML prototype also colors the **block** lane rose (`#d3788c`) in
`_laneCol` while coloring it purple in the errors header. Purple is correct and is
what ships, in both render sites; a test fails if the rose reappears.
