# rowt metrics — per-domain traffic history

Lightweight, always-on collection of per-domain bytes in/out over time, with
tiered rollup from 5-second resolution (1 hour) up to daily (1 year), surfaced
in the monitor by flipping the connections pane.

## 1. Data source

The only per-domain byte data sing-box exposes is the clash API's connection
tracker. This build (`with_clash_api`, no `with_v2ray_api`) offers:

- `GET /connections` — snapshot; **also upgrades to a websocket that pushes the
  full snapshot ~1×/second** (verified). Each connection carries a stable `id`
  and **cumulative** `upload`/`download` counters, plus `metadata.host`
  (domain) and `chains` (lane).
- Top-level `uploadTotal`/`downloadTotal` — **monotonic, loss-free** grand
  totals.

There is no close-event with final bytes: a closed connection simply vanishes
from the next frame. So per-domain accounting is inherently snapshot-diff based.

## 2. Collector (`rowt-collector`)

A separate binary (second `[[bin]]` in the rowt-monitor crate — **not** folded
into the TUI), run as a **sidecar with the router's lifecycle**: `bin/rowt`
spawns it right after `cmd_router up` reports healthy and kills it in
`_router_stop`/limbo. No LaunchAgent, no opt-in; present iff the router is up.

Mechanism — holds the `/connections` **websocket** (push, no poll-scheduler
gaps; idle backoff-reconnect while the router is down = ~zero overhead):

1. Per frame, per connection: `lane = classify_lane(chains)` (skip `block`);
   `domain = host || destinationIP || "?"`. Diff cumulative counters against the
   in-memory per-`id` cursor: `Δ = current − last` (saturating, so a sing-box
   restart / id reuse can't go negative). Accumulate `Δ` into the current 5-second
   bucket keyed `(domain, lane)`.
2. **Reconcile** against the monotonic grand totals: `unattributed = ΔgrandTotal
   − Σ Δconn`, bucketed under `("(unattributed)", "-")`. Captures the sub-second
   tail of connections that opened+closed inside one frame, so the books always
   balance against `uploadTotal`/`downloadTotal`.
3. On 5s bucket rollover, flush the bucket to SQLite (`bin += Δ` upsert).
4. Every 60s, run rollup (§4).
5. On ws disconnect: flush the pending bucket, clear the per-`id` cursor (counters
   reset when sing-box restarts), backoff-reconnect (1s→30s cap).

Because the counters are cumulative and the cursor lives in a persistent process,
a dropped frame loses nothing for still-open connections — the next frame's Δ
covers the gap. Only bytes moved by a connection entirely between two frames are
unattributed (and still land in the reconciliation bucket).

`ROWT_METRICS=off` skips spawning it.

## 3. Storage — SQLite (`~/.config/rowt/metrics/traffic.db`)

macOS ships `sqlite3`; the crate uses `rusqlite` (bundled). WAL mode. Four tiers,
same shape, `WITHOUT ROWID`:

```
sample_5s / sample_1m / sample_1h / sample_1d
  (ts INTEGER, domain TEXT, lane TEXT, bytes_up INTEGER, bytes_dn INTEGER,
   PRIMARY KEY (ts, domain, lane))
meta (k TEXT PRIMARY KEY, v TEXT)   -- schema_version, pid, started, last_write
```

`bytes_up` = upload (↑), `bytes_dn` = download (↓). `ts` = bucket start (epoch s).

## 4. Rollup / retention (RRD-style consolidation)

| tier | resolution | retention | folds into |
|------|-----------|-----------|-----------|
| 5s   | 5 s       | 1 hour    | 1m |
| 1m   | 1 min     | 24 hours  | 1h |
| 1h   | 1 hour    | 90 days   | 1d |
| 1d   | 1 day     | 1 year    | (deleted) |

Fold = `INSERT … SELECT (ts/step)*step, domain, lane, sum(up), sum(dn) … WHERE
ts < cutoff GROUP BY bucket,domain,lane ON CONFLICT DO UPDATE SET += …; DELETE
… WHERE ts < cutoff`, in one transaction. At a few hundred rows/min this stays
well under ~15 MB steady-state.

## 5. Surfacing in the monitor

No second UX. The connections pane is one conceptually-wide table with a pinned
`host` column; `v` pans the visible columns across `[live | ↑ upload | ↓ download]`
and wraps.

**One unified row list across all views** (so order is stable as you pan): the
live connections (in the source's throughput order) plus the top historical
domains not currently connected, appended and ranked by history. Hosts with no
live connection render **greyed** (`conns == 0`). Selection/route/yank/`e c b d`
key off the domain, so they work in every view. Both directions are loaded at
once, so `v` never re-queries or reorders.

- **`v`** pans `live → ▲ upload → ▼ download → live`; the caption shows
  `connections · ▲ upload · <band>`.
- **`w`** (+ `[` `]`) is **pane-scoped**: it cycles the *timescale band* when the
  connections pane is focused in a flipped view, or the errors window when the
  errors pane is focused — and is a **no-op** in the Live connections view.
  - recent: `1m  5m  1h  24h`   · days: `1h  6h  24h  7d`   · year: `24h 7d 30d 1y`
- **`f`** (+ `1/2/3/0`) unchanged — lane filter, scopes both panes incl. metrics.

The per-lane bandwidth rate table (`all / escape / corp / direct`) shows in the
header of **every** view. Column semantics: the two short columns render as
**rate** (`/s`), the longer two as **total bytes**; the `↑`/`↓` direction is the
prefix on each column label. A window spans tiers, so each column sums a UNION of
all four tiers filtered by `ts` (recent hour in `sample_5s`, older in the coarser
tiers — time-disjoint post-rollup). Magnitude is the number+unit (B→T); rows keep
the connection order, so no per-row axis is needed.

### Header identity grid

`uptime` moves to the top-right corner; `server` moves up into the top row of the
right column; `collector` status joins the left column under `sys proxy` (both
9-char labels align). Collector status: `on` (pid alive + fresh write) / `off`
(router up, no collector = failed to spawn) / `—` (router down / never run),
color-coded like `watch`.

Also: the `live · connections` caption drops its spurious mid-word dot → `live
connections` (the `·`-as-separator convention stays for real field joins).

## 6. Keys summary (no overloads)

| key | scope | effect |
|-----|-------|--------|
| `v` | connections pane | flip live / ↑ upload / ↓ download |
| `w` `[` `]` | focused pane | window (metrics: timescale band) |
| `f` `1/2/3/0` | both panes | lane filter |
