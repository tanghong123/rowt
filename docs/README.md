# `docs/` — the product introduction, as a page

`index.html` is a self-contained slide deck introducing rowt: the problem, how it
differs from a VPN, the architecture, onboarding, daily operations, a tour of the
monitor TUI, and the engineering numbers. Open it locally, or serve it from
GitHub Pages (Settings → Pages → Source: `main` / `/docs`).

Everything is inlined — no CDN, no build step, no fonts to fetch — so the file
works offline and from `file://`.

## Regenerating

The terminal screenshots are real frames rendered from the monitor's **demo
fixture**, never a live session: a live capture would carry real server tags,
corp hostnames and internal addresses into a shareable page. They are produced by
`rowt-monitor --fixtures --render-ansi WxH` (and, for the states `--render` cannot
reach — help overlay, lane filter, metrics flip, an armed edit — a short throwaway
test that calls `ui::draw` plus `ui::draw_footer`), then converted to styled HTML.

The palette is not invented: it is `rowt-monitor/COLORS.md`, so the deck matches
the tool it describes.
