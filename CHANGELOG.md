# Changelog

Notable changes to rowt, newest first. A version heading is added when a
release is cut; everything under Unreleased ships with the next one.

## Unreleased

- `server import` from Shadowrocket converts Hysteria2 entries (auth string,
  ALPN, allow-insecure, Salamander obfs with its password in the obfs
  argument; the certificate pin has no sing-box field and is dropped, as
  `server add` drops it from a link). They were counted under `skipped`.
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
- Lane entries compare without case in `add`, `rm` and the single-lane rule,
  and the rule-sets hold them lowercased — a mixed-case entry, which matched
  nothing before, now matches. The render warns on a case twin across lanes and
  on the one eight-label pair the deepest band ranks by lane order.
