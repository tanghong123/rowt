# Changelog

Notable changes to rowt, newest first. A version heading is added when a
release is cut; everything under Unreleased ships with the next one.

## Unreleased

- **`rowt hotspot` runs on the Rust version** (`ROWT_IMPL=rust`) instead of
  being handed to the shell. 38 of the 41 commands now are. Nothing changes
  in what it does: 31 cases compare it with the shell — output, exit status,
  files, the proxy calls it makes and the audit log. Three smaller fixes come
  with it, for the Rust version only, each matching the shell:
  - `<lane> import` with no file aborts, and the audit records it as aborted;
  - `rm` or `clear` on a lane file that doesn't exist says the list is empty;
  - an `add` creates the lane file even when no entry goes in.

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
