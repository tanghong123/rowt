# rowt-monitor

A terminal UI for observing a running `rowt` proxy — live connections and
throughput, a rolling-window view of errors and blocked domains, and
outbound-server health. The companion to the `rowt` CLI (`htop`/`btop`). Beyond
observing, it offers a few **confirmed, reversible controls** — switch the active
server, route a domain to a lane, toggle the system proxy — each a front-end to
the same `rowt` command; everything else stays observe-only.

Invoked as `rowt monitor`; also runs standalone as `rowt-monitor`.

## Run

```sh
rowt-monitor              # live TUI (falls back to a demo fixture when the
                          # proxy / clash API isn't reachable)
rowt-monitor --fixtures   # force the offline demo data
rowt-monitor --render 150x38   # print one frame as plain text (dev/testing)
```

## Keys

**Navigate:** `↑↓`/`jk` move (first press locks a row by domain; leaving a pane
forgets it) · `←→`/`hl` switch pane / pick a server chip · `Tab` cycle focus
(conns → errors → health) · `v` flip the connections pane (live / ↑ upload /
↓ download history) · `s` span (metrics timescale band) · `f` lane filter
(`1`/`2`/`3` jump, `0` clear) · `/` search hosts (regex, filters both panes;
`↵` commit, `esc` clear) · `w` or `[`/`]` errors window · `y` yank selected
domain · `p` pause · `?` help · `q` quit. (`v`/`s`/`f`/`/`/`w` are all global.)

**Controls** (confirmed, reversible; each runs the matching `rowt` command):
`e`/`c`/`b`/`d` route the locked domain → escape/corp/block/direct (arm, then
re-press or `↵` to commit; batched into one reload ~7s later) · `u` use the
selected server · `o` toggle the system proxy.

Shifted — `E`/`C`/`B`/`D` — make the same four edits on the host's **parent
suffix** instead of the host: `x.y.z.com` → `z.com`, so one keystroke covers a
whole service rather than the one hostname that happened to surface in the pane.
Registry second levels stay whole (`x.y.z.co.uk` → `z.co.uk`, never `co.uk`).

The entry is bare, not dot-led — measured against the router's own matcher
(`sing-box rule-set match`, 1.13.14):

| entry | `z.com` | `a.z.com` | `xz.com` |
|---|---|---|---|
| `domain_suffix: ["z.com"]` | ✓ | ✓ | ✗ |
| `domain_suffix: [".z.com"]` | ✗ | ✓ | ✗ |

sing-box matches on a **label boundary either way**, so a leading dot would only
*lose* the apex — not what "cover the whole service" means. `rowt explain` uses
the same boundary rule, so what it reports is what the router does.

Where there is nothing broader to add — an IP, or a host that already *is* its
registrable domain (`x.com`) — the shifted key stays **inert** and says so in the
footer, rather than silently writing what the lowercase key would.

Undo an `E`/`C`/`B` with `D`, not `d`: lane removal is an exact-line match, so
`d` on `x.y.z.com` removes only that entry and reports success without touching
a `z.com` written by `E`.

**The confirm bar sits at the right of the footer and has two phases.** For the
first **½ second** it is a plain confirmation, in the normal foreground colour
with no cursor: press the same key again to apply, or another arm key to
re-target. After that it turns **amber and grows a block cursor** — the entry is
now a live field, and the arm keys type instead of committing.

Each phase names only the key that works in it — `→ press e again to apply`,
then `→ escape · ↵ apply · esc cancel`. The shorter hint is **padded** to the
width of the longer, so the phase change swaps text in place and the entry does
not move; typing likewise extends the entry *leftward*, leaving the cursor and
everything after it where they were.

(Phase 1 shows the key rather than the lane name. The key *is* the lane —
`press c again` means corp — and the lane is spelled out the moment the bar
becomes editable.)

In the editable phase: type to change the entry, `←→`/`Home`/`End` move the
cursor, `Backspace`/`Delete` cut, `Ctrl-W` drops one label at a time, `Ctrl-U`
clears, `↵` applies whatever is in the field. `Esc` cancels, an empty field
cancels, and an entry containing a space is refused rather than silently closed
up (`edit_list` would strip it and write something the bar never showed).

Two consequences worth knowing. Once editable, printable keys go to the field —
so `q` types `q` rather than quitting, and `?`/`y` likewise; press `Esc` first.
And an armed edit **auto-cancels after 10s idle** — measured from the last
keypress, so typing keeps it alive and a pause to think doesn't discard it. The
cancel is exactly what `Esc` does, and just as silent.

**Mouse:** wheel scrolls (and focuses) the list under the pointer; click a row /
lane / window-tab to activate, a server chip to select it in place, or `sys proxy`
to toggle it (hover-highlights).

## Layout

- One **outer frame** (the only rounded corners); everything inside connects to
  it with `├ ┤` rules — no inset boxes.
- **Identity band** (neofetch-style logo + session facts) on top.
- **`live connections`** and **`errors & blocked`** panes — side by side,
  split by a center rule (tab labels shorten on narrow terminals).
- Full-width **`server health`** strip, merged onto the closing `┴` rule. When the
  pool overflows the row it marquees, with the active `▶` server pinned at the left
  edge (` │ ` seam) so it never scrolls out of view.

## Data sources

Everything is derived on a 2-second tick from: the clash API
(`127.0.0.1:9090` — `/traffic`, `/connections`, `/proxies`),
`~/.config/rowt/host.json`, `~/.config/rowt/state/servers.json`,
`~/.config/rowt/log/lane-*.log`, and host system facts. Respects
`ROWT_CLASH_PORT` (default 9090) and `ROWT_PORT` (default 7890).

## Design

- **[DESIGN.md](DESIGN.md)** — the full design doc: architecture, the data
  pipeline (clash API, incremental log tailing, block-lane bucketing, the
  server-health prober), rendering, interactions, resource characteristics, and
  the testing strategy.
- **[`../ux-design/rowt_monitor/`](../ux-design/rowt_monitor/)** — the
  authoritative UX spec + byte-exact ground-truth renders + HTML prototype.

The layout, colors, and 130-column reflow reproduce those captures byte-for-byte
in width. `tests/golden.rs` renders each geometry via ratatui's `TestBackend` and
diffs against them; `--render WxH` is the same path exposed on the CLI.

## Themes

Two palettes — dark and light — with the same layout, glyphs, and keys; only the
colors change. **[COLORS.md](COLORS.md)** is the full palette: every token in both
columns, its contrast, and where it renders (a test keeps it matching `theme.rs`).

```
rowt monitor                       # auto-detect (default)
rowt monitor --theme light         # pin it; also ROWT_MONITOR_THEME=light
```

`--theme auto` reads the terminal's *actual* background — `COLORFGBG` first, then
an OSC 11 query with a 100 ms budget — and picks light only for a near-paper
background (relative luminance ≥ 0.75); anything dimmer, or no answer at all,
stays dark. `$TERM` is never consulted. Pin the theme if your terminal reports its
background wrongly, or if you switch light/dark mid-session.
