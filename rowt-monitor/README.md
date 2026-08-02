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
