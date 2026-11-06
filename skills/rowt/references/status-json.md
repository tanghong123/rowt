# `rowt status --json` — schema 1

One JSON object on stdout, for agents and fleet checks. Rules:

- `schema` is an integer. Changes are **additive**: a key is never renamed or
  removed, an enum gains a value only with a new `schema`.
- Every key is always present; `null` where it does not apply.
- Stamps are UTC `YYYY-MM-DD HH:MM:SS`. There are no age fields: compute an age
  from `generated_at`.
- Never the clash secret. No server addresses (servers.json has them).
- Passive: nothing is probed when a record is fresh (within three watchdog
  intervals, 360 s by default). With no fresh record, `status` makes one try
  of the watchdog's delay test itself (`probed_now: true`,
  `last_probe.source: "status"`), which can take up to `ROWT_HEALTH_TIMEOUT`
  + 3 s (11 s). `--probe` forces it.
- `watchdog.interval_s` and `escape.fails_limit` are the installed watchdog's
  (read from its plist, where `watch install` bakes `ROWT_*` in); with no
  watchdog installed, this environment's, else the defaults.
  `follower.every_s` is this environment's.
- No sudo, no TTY, no network when a record is fresh; about a second over a
  non-interactive ssh. Exit 0 whenever it could report, `down` included. Only
  "rowt-rs missing or not answering" exits 1, with one line on stderr and
  nothing on stdout — never half an object.

The one key a fleet check needs is `.escape.state`: alert on `down` and
`holding`; `unknown` means "ask why" (`.escape.reason`), not "down".

```json
{
  "schema": 1,
  "rowt": "3.8.5",
  "generated_at": "2026-10-30 07:20:11",
  "mode": "host",                       // host | vm | local
  "intent": "up",                       // what the last up/down asked for: up | down | null
  "router":   {"running": true, "pid": 4242, "port": 7890, "engine": "1.13.14", "engine_pinned": true},
  "proxy":    {"system": true, "service": "Wi-Fi", "pac": false},   // null each with no active service
  "captive":  false,                    // the watchdog dropped the proxy for a portal
  "watchdog": {"loaded": true, "installed": true, "interval_s": 120, "last_observation_at": "2026-10-30 07:19:40"},
  "follower": {"running": true, "every_s": 15, "last_observation_at": "2026-10-30 07:20:05"},
  "escape": {
    "state": "ok",                      // ok | degraded | holding | down | unknown
    "reason": null,                     // see the table below; null when ok
    "since": "2026-10-30 06:02:11",     // when the current state began; null when nothing recorded it
    "selection": "failover",            // pinned | failover | auto | best
    "preferred": "tokyo-1",             // the pin (pinned, failover), else null
    "active": "osaka-2",                // the member the selector is on (auto: urltest's pick), else null
    "fails": 0, "fails_limit": 3,       // the watchdog's streak and where it recovers
    "last_probe": {                     // the freshest record, or the probe made now; null when none
      "at": "2026-10-30 07:20:05", "source": "follower",   // watchdog | follower | status
      "target": "osaka-2",              // a server tag, "auto", or "escape" (the selector)
      "ok": true, "result": "ok",       // result: ok | slow | fail | offline
      "delay_ms": 83
    },
    "probed_now": false,                // status had to probe (nothing fresh), or --probe
    "last_ok_at": "2026-10-30 07:20:05",// the lane last seen ok, by anyone
    "last_switch": {"at": "2026-10-30 06:02:11", "from": "tokyo-1", "to": "osaka-2", "by": "failover"},  // by: failover | auto; or null
    "hold": null,                       // {"since": "...", "tag": "..."} while the watchdog holds
    "servers": [                        // servers.json order, each as last tested: the clash API's history, else the watchdog's latency table
      {"tag": "tokyo-1", "type": "vless", "alive": false, "last_delay_ms": null, "last_test_at": "2026-10-30 07:19:25"},
      {"tag": "osaka-2", "type": "vless", "alive": true,  "last_delay_ms": 83,   "last_test_at": "2026-10-30 07:19:25"}
    ]                                   // alive false: its last test got no answer; alive null: never tested; empty when the router is down
  },
  "lanes": {"escape": 212, "corp": 48, "block": 9, "ads": true, "geosites": ["google"], "final": "direct"},
  "ports": [{"name": "vm", "port": 7891, "unmatched": "direct", "rendered": true}]
}
```

A named port's escape traffic shares the one lane: its verdict is
`.escape.state`; `unmatched` is where its unmatched traffic goes.

## `.escape.state` and `.escape.reason`

Settled in this order; the first that applies wins.

| state | reason | when |
|---|---|---|
| `unknown` | `local-mode` | local mode: no tunnel, the lane routes direct |
| `down` | `router-stopped` | the router is not running (`intent` says whether that was asked for) |
| `down` | `captive-portal` | the watchdog dropped the proxy for a portal |
| `holding` | `pinned-dead` | the watchdog holds instead of reloading onto a dead pinned server (`hold`) |
| `unknown` | `no-observation` | reserved: nothing recorded and no probe made — not produced today, since a probe that gets no answer is `down` |
| `down` | `offline` | the watchdog found the NIC without an address |
| `down` | `all-dead` | the follower found no server answering |
| `down` | `probe-failed` | the streak reached `fails_limit`, or status's own probe failed |
| `degraded` | `probe-failing` | the probe is failing, under the limit |
| `degraded` | `slow` | only the patient probe (3× the timeout) answered |
| `ok` | `null` | the lane answered |

`since` is the record's `ok_since` or `fail_since`, or the hold's stamp.

## Where it comes from

The watchdog writes `~/.config/rowt/watch.last` at each tick and the follower
`follow.last` at each round — eight `key=value` lines: `observed_at`, `source`,
`target`, `result`, `delay_ms`, `last_ok_at`, `ok_since`, `fail_since`. They
are rowt's own records, not a contract; read this JSON instead.
