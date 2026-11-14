# Changelog

Notable changes to rowt, newest first. A version heading is added when a
release is cut; everything under Unreleased ships with the next one.

## Unreleased

- Traffic metrics record the port each byte came in on: `main`, or a named
  port from `rowt port add`. `traffic.db` moves to schema 2, which adds a
  `port` column to every tier; the upgrade migrates the store in place, and
  rows from before it read as port `-`. `rowt metrics path` shows the new
  column and a per-port query. A downgrade to 3.8.40 or earlier leaves the
  store at schema 2, which the older collector cannot write to: nothing is
  recorded until rowt is upgraded again.
- The monitor's `P` key shows one port's traffic: it cycles all ports, `main`,
  then each named port, and back. The header's rates, the live connections and
  their history follow the choice; the errors pane stays on all ports and says
  so. With only `main`, `P` says how to add a port.

## 3.8.40 — 2026-11-10

- The monitor's control keys (`R`, `u`, `a`, `o`, and the lane keys) keep
  working after `brew upgrade`. Under `ROWT_IMPL=rust`, `rowt monitor`
  told the monitor to run its keys through rowt-rs by its versioned Homebrew
  path, which the upgrade deletes; a monitor left open (it switches itself to
  the new build) then failed every key with "…/rowt-rs: No such file or
  directory". rowt-rs now hands over Homebrew's stable `opt/rowt` path, and
  the monitor finds the installed rowt itself when the path it was given is
  gone or belongs to an older install. The shell version was not affected.

## 3.8.39 — 2026-11-07

- `rowt status` says `no answer at HH:MM:SS` of an escape server whose last
  delay test failed, and `untested` only of one never tested. sing-box's
  clash API forgets a server's delay history when a test fails, so a server
  that had just stopped answering read as "untested" next to members with
  numbers, as if it had never been tried; status now reads the watchdog's
  latency table where the API has nothing. `status --json` fills `alive`,
  `last_delay_ms` and `last_test_at` the same way; the schema is unchanged.
- `rowt server import --from shadowrocket` converts Trojan, VMess and TUIC
  entries too, with their TLS settings and, for Trojan and VMess, a WebSocket
  or gRPC transport; a TUIC entry's UDP relay mode (native or quic) comes
  along. They used to be counted as "not imported" although `rowt server
  add` takes the same servers from a share link. The mapping comes from real
  Shadowrocket entries made from rowt's own links, not from guesswork; an
  entry with a transport the importer does not know is still skipped and
  named in the summary. (A Trojan entry Shadowrocket made from an h2 link
  has no transport in Shadowrocket's own store, so it imports as Shadowrocket
  has it: plain TLS.) Both the shell and the Rust version.
- A VLESS entry with a WebSocket transport imports with its path and its Host
  header in the right places. The importer read the field Shadowrocket keeps
  the Host in as the path, so such a server came out with its Host as its
  path and no Host header, and would not connect; a gRPC service name was read
  from the same wrong field. The older store layout, which has no separate
  path field, keeps the reading it had. A VLESS entry with a transport the
  importer does not know is now skipped and named in the summary, as a Trojan
  or VMess one is, instead of imported without it. Both the shell and the
  Rust version.
- `rowt server import --from shadowrocket` now reads the copy of the server
  store that Shadowrocket keeps in iCloud Drive. A newer Shadowrocket writes
  its entries there as archived dictionaries, a layout the importer did not
  read, so on a Mac that could not open the app's own container the import
  found no servers and said nothing. Both the shell and the Rust version.

## 3.8.35 — 2026-11-03

- `rowt status` and `rowt watch status` no longer report the watchdog as
  "NOT loaded" when run over SSH while launchd is running it. The check now
  asks launchd's gui domain for the agent by name (`launchctl print
  gui/<uid>/<label>`, the domain `watch install` bootstraps into) instead of
  `launchctl list`, which only answers for the caller's own session. The
  same question now backs `status --json`'s watchdog field, the `use best`
  warning and the tick's self-heal verify. A box checked remotely used to
  read as broken and be told to run a needless `watch refresh`.

## 3.8.34 — 2026-11-03

- **The metrics collector runs again on the Rust version** (`ROWT_IMPL=rust`).
  It looked for the collector in the wrong folder of a Homebrew install, so
  from the first reload on the Rust version it stopped the running collector
  and never started a new one: `rowt monitor` and `rowt metrics` had no fresh
  data. It now looks where the shell version does: next to rowt, in a local
  build, then on your PATH. The shell version was never affected.

## 3.8.33 — 2026-11-01

- `rowt config export --to shadowrocket` no longer stops, silently and with
  no file, when one of your lanes has no entries. This one is a fix for
  everyone, not only for the Rust version.
- **`rowt hotspot` runs on the Rust version** (`ROWT_IMPL=rust`) instead of
  being handed to the shell. 38 of the 41 commands now are. Nothing changes
  in what it does: 31 cases compare it with the shell — output, exit status,
  files, the proxy calls it makes and the audit log. Three smaller fixes come
  with it, for the Rust version only, each matching the shell:
  - `<lane> import` with no file aborts, and the audit records it as aborted;
  - `rm` or `clear` on a lane file that doesn't exist says the list is empty;
  - an `add` creates the lane file even when no entry goes in.
- `rowt doctor` runs natively in rowt-rs (`ROWT_IMPL=rust`), row for row and
  FLAG for FLAG with the shell, compared by cli-diff over six scenarios of the
  Mac around rowt. The `apps` row lists the clients it finds in a fixed order;
  it used to follow awk's hash order.
- **`rowt speed` and `rowt port` run on the Rust version** too, so all 41
  commands do. Behaviour is unchanged. 46 cases compare the
  two with the shell, `speed` over a fixed set of throughput samples (every
  cell, median and verdict) and `port` down to the undo when a running router
  will not take the change.
- `rowt config import` and `rowt config export --to shadowrocket` run natively
  in rowt-rs too, the last two `config` paths that went to the shell. The merge
  folds each lane through rowt-rs itself against a staging copy, as before
  through the shell; the Shadowrocket export writes the same rules. Nothing
  rowt-rs answers reaches for the shell any more.

## 3.8.28 — 2026-11-01

- A hysteria2 **certificate pin is kept** — `pinSHA256` in a link given to
  `server add`, `hpkp` in a Shadowrocket entry, a Clash proxy's `fingerprint`.
  sing-box pins a public key (`tls.certificate_public_key_sha256`) and has no
  field for a certificate hash, so the add fetches the server's certificate once
  over QUIC, refuses the server if its SHA-256 differs from the pin, and records
  the public-key hash, which sing-box checks on every connection. A certificate
  it cannot fetch refuses too, with the reason: no QUIC-capable `openssl`
  (Homebrew's `openssl@3`, now a dependency of the formula), an unreachable
  server, Salamander obfs. The pin used to be dropped in silence: the server
  connected, and the one check a self-signed server had was gone. A pinned
  link inside a subscription is not fetched; a `note:` says so.
- `server import` from Shadowrocket converts Hysteria2 entries (auth string,
  ALPN, allow-insecure, Salamander obfs with its password in the obfs
  argument). They were counted under `skipped`. An entry with a plugin or an
  SSR-style proto set is skipped, as a Shadowsocks one is; settings in
  `pluginParam` are not read, and a note says so.
- **The watchdog can run rowt-rs, and remembers it across upgrades.**
  `ROWT_IMPL=rust rowt watch install|refresh` records the choice in state as
  `watch_impl`, and the LaunchAgent plist carries `ROWT_IMPL=rust` only when
  state says so; a plist nobody switched is unchanged. The way back is
  `ROWT_IMPL=bash rowt watch refresh`. A watchdog on rowt-rs ends the tick
  shadow on that machine.
- `use <tag>`, `use best`, a lane edit and `corp sync` exit 1 when the router
  does not come back from the restart after them. They exited 0 before, with
  their own "escape -> …" or "added:" line, over a router that was down (the
  proxy already turned off by the no-limbo step); a fleet play that read 0 now
  sees the failure.
- **Lane edits are live.** `escape|corp|block add|rm|import|clear` and a
  `corp sync` that changed the lane reach a running router without a restart
  and drop no connection. Every lane rule is a local sing-box rule-set under
  `rule-sets/` (one directory per set; 48 banded sets that keep
  longest-suffix-wins for every entry of up to eight labels, plus the corp
  CIDR set), and `host.json` only
  points at them. What still restarts: servers and subscriptions, `use` and
  auto's knobs, ports, mode and interface changes, `config import`, a
  `geosite:` add or rm, and the first edit after upgrading. `ROWT_LANES_HOT=0`
  restarts every time, as before.
- `corp sync` applies live through rule-sets (a contributor's change), and its
  synced DHCP domains rank by depth with the hand entries again.
- `rowt render` alone can change a running router's lane rules at once: it
  writes the rule-sets the router watches — when the router runs the rendered
  `host.json`; otherwise, as before, only `reload` reaches it.
- A lane file with a byte that is not UTF-8 no longer renders as an empty lane
  in rowt-rs (3.8.21). One invalid byte emptied the whole file on the Rust
  side, and with live rule-sets the empty lane reached the running router on
  the next render; the shell kept the entries. Every reader now decodes
  lossily, so both sides render the same set.
- The monitor's batched lane edits go live too: the render after a batch of
  TUI lane edits already reaches the router, so the monitor no longer restarts
  it afterwards (which dropped every connection on the Mac for nothing); it
  restarts only when the router does not run what was rendered.
- Lane entries compare without case in `add`, `rm` and the single-lane rule,
  and the rule-sets hold them lowercased — a mixed-case entry, which matched
  nothing before, now matches. The render warns on a case twin across lanes and
  on the one eight-label pair the deepest band ranks by lane order.
