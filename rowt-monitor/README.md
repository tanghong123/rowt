# rowt-monitor

A read-only terminal UI for observing a running `rowt` proxy — live connections
and throughput, a rolling-window view of errors and blocked domains, and
outbound-server health. The companion to the `rowt` CLI (`htop`/`btop`, not a
settings screen). It **never mutates** routing, servers, or the proxy.

Invoked as `rowt monitor`; also runs standalone as `rowt-monitor`.

## Run

```sh
rowt-monitor              # live TUI (falls back to a demo fixture when the
                          # proxy / clash API isn't reachable)
rowt-monitor --fixtures   # force the offline demo data
rowt-monitor --render 150x38   # print one frame as plain text (dev/testing)
```

## Keys

`↑↓`/`jk` move · `←→`/`hl` switch pane · `Tab` cycle focus · `f` lane filter
(`1`/`2`/`3` jump, `0`/`Esc` clear) · `w` or `[`/`]` errors window · `y` yank
selected domain/host · `p` pause · `?` help · `q` quit. Mouse wheel scrolls the
list under the pointer; click a lane row / window tab / row to activate it.

## Layout

- **Identity band** (neofetch-style logo + session facts) on top.
- **`live · connections`** and **`errors & blocked`** panes — side by side at
  ≥130 columns (one split box), stacked below 130.
- Full-width **`server health`** strip at the bottom.

## Data sources

Everything is derived on a 2-second tick from: the clash API
(`127.0.0.1:9090` — `/traffic`, `/connections`, `/proxies`),
`~/.config/rowt/host.json`, `~/.config/rowt/state/servers.json`,
`~/.config/rowt/log/lane-*.log`, and host system facts. Respects
`ROWT_CLASH_PORT` (default 9090) and `ROWT_PORT` (default 7890).

## Design fidelity

The layout, colors, and 130-column reflow reproduce the design captures in
`../design_handoff_rowt_monitor/renders/` byte-for-byte in width. The
`tests/golden.rs` suite renders each geometry via ratatui's `TestBackend` and
diffs against those captures; `--render WxH` is the same path exposed on the CLI.
