# rowt — how DNS and routing actually work

This document explains, in detail, how `rowt` decides where each packet and
each DNS query goes **while the corporate VPN is connected**. If you only want to
use the tool, the [README](README.md) is enough; read this when you want to know
*why* it behaves the way it does, or to debug a site that goes the wrong way.

## 1. The problem it solves

You want a personal VLESS VPN for some sites (Google), the **corporate VPN** for
work sites (intranet), and a **direct** connection for the rest (Baidu) — all at
the same time. You can't just run Shadowrocket and the corp client together:
both are *packet tunnels* (`utun` interfaces) and both try to own the default
route, so macOS lets only one win.

`rowt` sidesteps that by **not being a tunnel**. It runs sing-box as a
plain **userspace proxy** on `127.0.0.1:7890`. A proxy doesn't touch the routing
table, so it never fights the corp client. Apps send traffic to the proxy (via
the macOS system proxy), and sing-box decides, per connection, which of three
"exits" to use.

## 2. The three exits (outbounds)

```
                      ┌────────────────────────── sing-box (127.0.0.1:7890) ──────────────────────────┐
   app ──HTTP/SOCKS──▶│  match the destination against rules, pick ONE outbound:                      │
                      │                                                                                │
                      │   escape ──▶ VLESS tunnel      (socket BOUND to en0 → home router → VPS)       │
                      │   corp   ──▶ direct, NO bind   (OS routing table → corp utun → intranet)       │
                      │   direct ──▶ direct, bind en0  (socket BOUND to en0 → home router → internet)  │
                      └────────────────────────────────────────────────────────────────────────────────┘
```

The trick that makes all three coexist under a full-tunnel corp VPN is **socket
interface binding** (`IP_BOUND_IF` on macOS, exposed by sing-box as
`bind_interface`):

- A socket **bound to `en0`** leaves through the physical NIC *regardless of what
  the routing table says* — so `escape` and `direct` bypass the corp tunnel even
  though corp owns the default route.
- A socket with **no bind** obeys the routing table — so `corp` traffic follows
  whatever routes the corp client installed (intranet CIDRs, and the default
  route if corp is full-tunnel).

## 3. Routing, step by step, with the corp VPN ON

Assume the corp client is connected full-tunnel: it owns `default → utunN` and
also installs routes for the intranet ranges (`10/8`, `172.16/12`). Your
home LAN (`192.168.x`) stays on `en0` because corp excludes the local subnet.

A connection arrives at the proxy. sing-box sniffs the destination host name and
walks the rules **top to bottom, first match wins**:

| rule | outbound | what physically happens (corp ON) |
|------|----------|-----------------------------------|
| `domain:<host>` in any lane (`<lane> add --domain`) | that lane | a sing-box `domain` rule — **whole-host equality**, no subdomains. Emitted ahead of every suffix rule, so it wins outright for that one host: `corp add --domain dev.g.alicdn.com` keeps just that name on corp while `escape add alicdn.com` covers the rest of the CDN |
| `domain_suffix` in `block-domains.txt` | **block** | refused instantly — no DNS, no dial (kills the ad/tracker retry storm) |
| `domain_suffix` in `corp-domains.txt` | **corp** | resolved to an intranet IP (see §4), sent via the **routing table** → matches the corp route → into `utunN` → corp network |
| `ip_cidr` in `corp-domains.txt` (e.g. `10.0.0.0/8`) | **corp** | literal intranet IP → routing table → `utunN` → corp |
| `domain_suffix` in `escape-domains.txt` | **escape** | VLESS socket **bound to en0** → home router → VPS → the VPS reaches Google |
| `geosite:<name>` in `escape-domains.txt` / `block-domains.txt` (e.g. `geosite:google`) | that lane | a maintained sing-geosite category, pulled as a `rule_set` — covers a whole service (all ccTLDs) without enumerating suffixes; runs after the hand lists, before the ad set. Escape + block only (corp is domains/CIDRs); a specific suffix always wins over it |
| `geosite-category-ads-all` rule-set | **block** | broad ad/tracker blocklist (opaque, so it runs *after* the hand lists) |
| unlisted IP in a **private/overlay range** (RFC1918, CGNAT `100.64/10`, link-local) | **corp** | unbound → routing table. Overlay hosts (Tailscale peers), LAN devices, and bare intranet IPs reach correctly with no config — some tunnel routed that range, and forcing it out `en0` would break it. `ROWT_PRIVATE_DEFAULT=direct` restores the old always-en0 behavior |
| no match → `final` (default `direct`) | **direct** | direct socket **bound to en0** → home router → the public internet, corp untouched |

**Longest suffix wins, not lane order.** rowt does *not* emit one rule per lane
(which would make an earlier lane shadow a later one). It flattens the three hand
lists into **one rule per suffix, ordered most-specific-first** (more labels, then
longer). So `api.foo.com` in escape beats `foo.com` in block for `api.foo.com`,
while `www.foo.com` still lands in block. A suffix lives in **at most one lane** —
`rowt escape/corp/block add` pulls the entry out of the other two — so there are
never two lanes fighting over the same suffix. The opaque geosite rule-set can't
be interleaved by specificity, so it sits after the hand lists (explicit intent
overrides the broad blocklist) but before `final`.

Two consequences worth internalizing:

- **Corp intranet host names must be listed as domains** (rule #1), not left to
  the IP-CIDR rule. The CIDR rule (#2) only fires for *literal IPs* (e.g.
  `ssh 30.221.4.5`), because a name has to be resolved first — and an intranet
  name only resolves through corp DNS, which rule #1 arranges (see §4).
- **`direct` genuinely bypasses corp.** Because its socket is pinned to `en0`,
  Baidu traffic never enters the corp tunnel — faster, and it keeps your
  personal browsing off the corporate network.

## 4. DNS, step by step, with the corp VPN ON

DNS is where most "why did this site go the wrong way?" bugs live, so the design
keeps each bucket's *name resolution* on the same path as its *traffic*:

```
dns:
  servers:
    - local        (type: "local"  → the macOS system resolver)
    - dns-direct   (DoH https://223.5.5.5, detour: "direct" → queried over en0)
  rules:
    - domain in corp-domains.txt (the `--domain` entries) → server: local
    - domain_suffix in corp-domains.txt → server: local
  final: dns-direct
outbounds:
  - corp: domain_resolver: local        # connection-time lookups — see below
route:
  default_domain_resolver: dns-direct
```

- **`escape` names are never resolved locally.** The route rule matches on the
  *sniffed* domain, and the VLESS outbound forwards that domain to the VPS, which
  resolves it at the exit. So `google.com` is **not** looked up by Chinese DNS
  (no poisoning) nor by corp DNS (no leak of what you browse). This is why
  domain-based rules + sniffing matter.
- **`corp` names → `local`.** `"local"` means sing-box uses the macOS system
  resolver configuration, which — on the corp LAN or with the corp VPN up — is
  corp DNS (from DHCP / the VPN). So intranet names resolve to intranet IPs, and
  rule #1/#4 send them into the corp tunnel. Off corp, `local` is simply
  whatever resolver the current network provides. (Avoid `/etc/resolver/<suffix>`
  pins for corp suffixes: `getaddrinfo` honours them *over* this arrangement, and
  a pin to a public resolver breaks internal-only zones for every app.)
- **Everything else → `dns-direct`** (AliDNS `223.5.5.5` over **DoH**), and
  crucially the query is sent through the **`direct`** outbound, i.e. **over
  `en0`**. So Baidu is resolved by a Chinese resolver on your home line —
  China-optimal IPs, and the lookup itself never touches corp DNS. DoH (not plain
  UDP) is deliberate: a persistent UDP DNS socket wedges on a network transition
  and the sing-box `UDPTransport.recvLoop` then busy-spins on the dead fd (~200%
  CPU, independent of traffic). DoH is connection-based (a wedged connection
  errors and re-dials instead of spinning) and encrypted; `223.5.5.5` serves DoH
  on 443 with an IP-valid cert, so no bootstrap DNS is needed. (Plain TCP:53 and
  DoT:853 are commonly blocked; DoH on 443 gets through.) Override the resolver
  with `ROWT_DNS_DIRECT` (e.g. `1.1.1.1`); it also feeds `resolve_ip` and the
  doctor DNS check, and is forwarded to the `watch` reload agent.

**Connection-time lookups follow the same map — by explicit arrangement.** In
sing-box ≥ 1.12, the domain of a proxied *connection* is resolved via the
outbound's `domain_resolver` (falling back to `route.default_domain_resolver`) —
the `dns.rules` above govern only DNS *queries*. That's why the corp outbound
carries `domain_resolver: local` itself: without it, corp names were resolved by
the public DoH resolver at connect time, which returns SERVFAIL for
internal-only split-horizon zones. (This was the rowt < 3.1.1 bug: proxied
corp names failed with `lookup …: SERVFAIL` in `host.log` while plain
`ping`/`dig` — which use the system resolver — worked fine.)

Net effect with corp ON: intranet lookups use corp DNS over the corp tunnel;
your personal/Chinese lookups use AliDNS over your home line; escaped sites are
resolved remotely by the VPS. Three worlds, no cross-contamination.

## 5. The corp lane fills itself (corp sync)

The corp lane would be tedious to maintain by hand — every employer has its own
suffixes, and a VPN's routed ranges change under you. `rowt corp sync` (run
automatically by the `watch` agent on every tick, and on demand) mirrors two
live signals into the lane, so on a fresh machine the corp lane mostly
configures itself:

- **DHCP search domains → corp domains.** A domain the physical NIC's network
  advertises (an office LAN's search domain) only resolves via the system's
  split-horizon resolver — which is exactly the corp lane's DNS behavior (§4) —
  so it belongs in corp. Persist-union across networks: a domain learned in the
  office is still internal later, reached over the VPN from home. Your explicit
  escape/block entry always wins over an auto-learned domain (a network must not
  be able to de-tunnel a site you chose to tunnel).
- **Live VPN route CIDRs → corp CIDRs.** A split-tunnel VPN installs routes for
  its ranges; `corp sync` reads them from the routing table (vendor-agnostic)
  and mirrors them into the lane, so proxied **by-IP** access rides the tunnel
  instead of leaking out `en0`. Which tunnels to mirror is a label list in
  `sync-ifaces.txt` — default the corp VPN (auto-detected: the busiest
  non-Tailscale tunnel); add `tailscale` to reach tailnet hosts through the
  proxy too.

Two properties keep it cheap and safe:

- **Superset reconcile, minimal reloads** (`config/corp-sync-reconcile.py`): the
  lane only has to *contain* every live route. While it does, nothing is
  rewritten and nothing reloads. When a live route is uncovered, the minimal
  edit restores the superset: add what's missing, drop kept CIDRs that overlap a
  live route (whole, never shrunk), and keep stale CIDRs from a now-down tunnel —
  they're still needed in-office, where the same ranges are on the LAN and no
  tunnel is up. Hand-added entries are never touched.
- **Private/overlay ranges are excluded from tracking.** Live routes inside
  RFC1918 / CGNAT / link-local are filtered out of the reconcile entirely: the
  private-range fall-through (§3) already sends that whole class to the corp
  lane, so tracking individual slices would be redundant data that can only
  drift. Only public-looking corp ranges — the kind corp clouds route
  privately — need mirroring.

### The discovery journal — data for future policy

Every watchdog tick also journals what the current network and VPN *advertise*
into `~/.config/rowt/log/discovery.log` — one JSON line per **change** (a
`discovery_sig` state key dedupes; steady state writes nothing): the network id,
the corp-VPN iface if up, the captive state at that moment, the physical NIC's
DHCP search domains, the scoped/VPN-registered domains beyond them, and the
internal nameservers. Portal episodes and the domains the venue advertised land
in the same line.

This is the longitudinal record behind corp-lane policy decisions — e.g. *does
the corp VPN register a superset of the office DHCP domains?* (decides whether
DHCP-learned domains still need persistence), *which venues advertise portal
domains?* — questions that are otherwise only answerable in the moment, on
networks we rarely revisit. `ROWT_DISCOVERY_LOG=0` disables.

## 6. Who does what: lanes vs DNS vs OS routes

Three planes decide a connection's fate. rowt owns the first two; the third is
deliberately out of scope:

| plane | question | owner | mechanism |
|---|---|---|---|
| **lane** | which egress a *proxied* connection takes | rowt | the §3 rules; corp lane auto-filled by §5 |
| **DNS** | which resolver answers a name | rowt | §4 — corp → system resolver, escape → VPS-side, rest → DoH over `en0` |
| **OS routes** | which *interface* unbound traffic leaves by | **not rowt** | the OS + the VPN clients — and, when an overlay claims a range your corp also uses, a small root daemon |

rowt refuses the third plane on purpose. Mutating the route table needs a
persistent **root** daemon, and a root daemon must never execute user-writable
code — rowt is brew-installed, user-writable, and frequently updated: exactly
the wrong shape for root. The route table also matters most for traffic rowt
never sees (raw `ping`/`ssh`, overlay peer traffic, anything that ignores the
proxy — §8). rowt's corp lane *consumes* the table (an unbound socket, §2); it
never writes it.

The one case that needs plane three actively managed: an overlay like
**Tailscale claims all of CGNAT `100.64.0.0/10`**, while a corp intranet hands
out service addresses from the same range. On the corp LAN with the VPN off,
nothing out-specifics the overlay's route and those services silently die into
the tailnet. That's a route-table problem, so it's solved by a route-table tool:
[`corp-route`](https://github.com/tanghong123/claude-toolbox/tree/main/corp-route),
a ~350-line self-learning root daemon that snapshots the CGNAT routes the corp
VPN installs (fingerprinting the corp by its DNS) and replays them onto the
physical NIC exactly when needed. The division is airtight by construction: the
two tools share **no files** and coordinate through nothing but the OS — rowt
tracks the VPN's *non-overlay* ranges for lane choice and filters the overlay
space out (§5); corp-route learns exactly the *overlay-shadowed* slices. One
dataset each, no overlap, nothing to drift.

Walkthrough — `gitlab.corp.example` → `100.64.75.10`, corp VPN off, on the corp LAN:

1. **A browser (proxied):** rowt matches the corp suffix → corp lane; the name
   resolves via the system resolver (§4) → `100.64.75.10`; the unbound socket
   consults the route table → corp-route's replayed `/16` beats Tailscale's
   `/10` → out `en0` into the corp LAN. ✓
2. **`ssh` (not proxied):** `getaddrinfo` → same system resolver → same route
   table → same wire. ✓ — planes two and three serve even the traffic rowt
   never touches, which is exactly why they don't live inside rowt.

## 7. Three modes: how (and whether) the escape uplink runs

Rules #3 (escape) and #4 (direct) both rely on the *bound-socket* trick. That
works only if the corp client enforces its tunnel with **routes**. Some
enterprise clients instead run a **packet filter** that drops any packet not
leaving through their `utun`, which defeats `bind_interface`. `rowt probe`
detects this by testing a VPS both via the default route and via `en0`:

- **mode `host`** — `bind_interface` works. The VLESS outbound runs on the host,
  bound to `en0`. Compact, nothing else needed, works while travelling.
- **mode `vm`** — `bind_interface` is filtered. A **bridged Lima VM** (its own
  LAN IP, its own network stack) runs the VLESS tunnel; the host's `escape`
  outbound becomes a SOCKS hop to that VM. The VM's packets are physically
  independent of the host, so no corp filter can catch them. The host→VM hop
  rides the home LAN, which corp excludes.

- **mode `local`** — there is no uplink at all. For when you are already
  *outside* the censored network (travelling, say): the escape lane's
  destinations answer on the physical NIC, so tunnelling them is pure overhead —
  an extra hop, a worse path, and a needless dependency on a server being alive.

In all three the app-facing proxy is still `127.0.0.1:7890`; only the `escape`
outbound's implementation differs — and in `local` it simply isn't there.

### 7.1 What `local` mode changes, and what it deliberately doesn't

`rowt up local` sets it; `rowt up host` goes back. With no argument, `rowt up`
picks it automatically when the canaries in `ROWT_GFW_CANARIES` answer over the
physical NIC (see below).

| | `host` / `vm` | `local` |
|---|---|---|
| escape lane rules | `outbound: escape` | `outbound: direct` |
| escape outbounds | members + urltest + selector | none — no server needed or contacted |
| direct-lane DNS | `ROWT_DNS_DIRECT` (AliDNS) | `ROWT_DNS_LOCAL` (Cloudflare) |
| block / corp / direct | — | unchanged |
| watchdog tunnel probe | on | off |

**The escape rules are still emitted.** Not dropping them is the whole subtlety:
those rules carry *precedence*. They are interleaved with block and corp by
suffix specificity, and they sit above the broad `geosite-ads` set. Remove them
and two things break — an escape entry like `api.foo.com` starts matching a
block entry for `foo.com`, and anything the ad/tracker set also covers gets
sunk. Retargeting the same rules to `direct` changes the destination and
nothing else.

**DNS moves for a reason.** AliDNS is the right resolver from inside China and
the wrong one from outside it: a long round trip that answers with
China-optimised addresses. `local` mode uses a public resolver instead
(Cloudflare by default — DoH is TCP-based and `1.1.1.1` presents an IP-valid
certificate, so no bootstrap lookup is needed, exactly like the AliDNS case).

**The watchdog stops probing the tunnel.** `_watch_health` delay-tests the
escape server every tick and, after `HEALTH_FAILS` consecutive failures, calls
`_watch_recover … cmd_reload`. With no tunnel that condition is permanent, so
leaving it on would reload the router forever. This is the part that bites if
you only retarget the rules.

**A full-tunnel corp VPN does not defeat it.** The `direct` outbound carries
`bind_interface: en0`, so direct traffic leaves through the physical NIC and
never rides the corp tunnel back into the censored network — which is exactly
the case this mode was built for: abroad, on a China corporate VPN.

### 7.2 Detecting "is the escape lane needed here?"

`_direct_reaches_escape` curls each canary with `--interface <en0>` and
`--noproxy '*'`, so it measures the *direct* path rather than whatever the
router is currently doing. A 2xx/3xx means TLS completed against the real host,
which the GFW cannot forge — a poisoned DNS answer or an injected reset fails
it. One strict hit is enough; requiring two only slows down the common in-China
case, where every canary times out.

Two properties matter:

- **Every canary must be blocked inside China.** A reachable-anywhere host would
  make the check answer "outside" everywhere. The defaults are Google and
  YouTube endpoints.
- **Failure is the safe direction.** Offline, a canary outage, a flaky link —
  all read as "keep the tunnel", which is the conservative answer. The check
  runs only for a bare `rowt up`; an explicit `rowt up host` is never
  second-guessed.

## 8. What the proxy catches — and what it doesn't

`rowt` routes traffic that reaches its **SOCKS/HTTP proxy**. That covers
the large majority of everyday apps, but **not** everything. A full packet
tunnel like Shadowrocket-direct captures *all* IP traffic; a proxy captures only
what is sent to it. Things a proxy typically **misses**:

- **Apps that ignore the system proxy.** Most GUI apps (Safari, Chrome, Arc,
  Electron apps) honour it; but many **CLI tools do not** — `curl`, `git`, `npm`,
  `brew`, `ssh`, `docker`. For those, export the proxy env vars:
  `eval "$(rowt proxy env)"` (this is why the `proxy env` command exists). `ssh` needs a
  `ProxyCommand`/`nc` wrapper; it won't pick up env vars alone.
- **UDP-heavy real-time apps.** Online **games**, **voice/video calls** (Zoom,
  Teams, WhatsApp/Telegram calls), WebRTC, and **QUIC/HTTP3** lean on UDP. macOS
  system-proxy support for UDP is weak, so these usually go **direct** (bypassing
  the proxy) even when the proxy is on. If you *need* those tunnelled, a full VPN
  is the right tool.
- **Non-HTTP protocols & P2P.** BitTorrent, game netcode, mail (SMTP/IMAP) in
  some clients, and other raw-socket protocols — only tunnelled if the app
  explicitly supports a SOCKS proxy.
- **System-level traffic.** OS updates, iCloud, Spotlight, push, telemetry — a
  proxy leaves these on the normal path.

**Rule of thumb (layman's terms):** if it's a *web page or a normal app talking
to a website*, the proxy handles it. If it's a *game, a video/voice call, a
torrent, or a command-line tool*, it either needs extra setup or is better served
by a full tunnel. Since a full tunnel (Shadowrocket-direct) is exactly what
conflicts with the corp VPN, the honest trade is: **rowt = coexists with
corp, covers proxy-aware apps; Shadowrocket-direct = covers everything, but can't
run alongside the corp client.** A middle path is mode `vm` — you can point a
whole app or device at the VM and get tunnel-like coverage for it without
touching the host's corp setup.

## 9. Mapping a Shadowrocket config onto escape

Shadowrocket uses the Surge-style config format. The pieces map like this:

Use `rowt server import` to do this automatically (it decodes Shadowrocket's
server store + rules into an editable review file). The same command imports from
other clients too — `rowt server import --from clash-verge|v2box|flclash` — all
writing the same source-independent review file, then `--apply`. The mapping:

| Shadowrocket | rowt |
|--------------|------------|
| `[Proxy]` entries / subscription URLs | servers — `rowt server add` / `subscribe` / `import` (VLESS / VMess / AnyTLS / hysteria2; SS/trojan etc. skipped) |
| `[Rule] …,PROXY` | `escape-domains.txt` (→ escape) |
| `[Rule] …,DIRECT` for **intranet** domains | `corp-domains.txt` (→ corp) |
| `[Rule] …,DIRECT` for **everything else** | nothing to do — escape's `final` is already `direct` |
| `[Rule] …,REJECT` | not ported (add a block rule manually if wanted) |
| `[General] tun-excluded-routes` / `skip-proxy` (intranet & LAN CIDRs) | `corp-domains.txt` CIDRs + corp's own routes |
| `[General] dns-server` | the `dns` block in §4 |
| `FINAL,…` | `ROWT_FINAL` (`direct` or `corp`) |

Note Shadowrocket lumps intranet and Chinese sites together as `DIRECT` because,
as the only VPN, "direct" just meant "off my tunnel". escape splits that
`DIRECT` pile in two — **corp** (intranet, must traverse the corp VPN) vs
**direct** (physical NIC) — which is the whole point of the three-way design.

## 10. Failure modes

- **Escape tunnel down.** Escape-listed sites use the `escape` outbound only;
  there is no fallback to `direct`, so they **fail closed** rather than leak onto
  the corp/home path. Switch servers with `rowt use <tag>` or `use auto`.
- **Corp VPN down.** `corp` traffic (no bound socket) follows the routing table;
  with corp down there's no route to intranet ranges, so those connections fail
  until you reconnect — expected.
- **Wrong bucket.** If a site loads over the wrong exit, check which list it
  matches (`escape-domains.txt` / `corp-domains.txt`) and remember first-match
  order: corp rules are evaluated before escape. `rowt status` prints the
  bucket counts and validates the generated config.
- **Wedged or crashed tunnel (auto-recover).** The `watch` LaunchAgent probes
  the escape tunnel every `ROWT_WATCH_INTERVAL`s (default 120) via the clash API
  **delay test** — traffic forced through the selected escape server, so the
  result reflects the tunnel itself, not how a probe domain happens to route
  (an HTTP probe through the mixed proxy gave false failures on a flaky
  direct-to-CDN path). Two failure classes self-heal, both via `rowt reload`
  (re-render + start-if-down/restart-if-up + re-proxy), gated by
  `ROWT_HEALTH_COOLDOWN` (default 600s) and **verified by a re-probe** so a
  start-then-crash isn't logged as success:
  - *Wedged* (router up, not carrying traffic): after `ROWT_HEALTH_FAILS`
    (default 3) consecutive probe failures.
  - *Crashed* (router down): only when the user's intent is **up** and it is the
    same boot the router was started in — a reboot or a deliberate `rowt down`
    never resurrects it (rowt has no auto-start). Intent is tracked in `state`
    (`intent`/`boot`), set by `up`/`reload`/`restart`, cleared by `down`.

  Network *changes* are handled separately and instantly by the same agent's
  `WatchPaths` reload. Requires `rowt watch install`.

  Every tick that **finishes** stamps `~/.config/rowt/watch.tick` on its way
  out (the shell's EXIT trap; the Rust tick's one `finish_tick`). That mtime is
  the only liveness signal the agent has: `launchctl list` says a job is
  *registered*, not that ticks complete; a quiet, healthy tick deliberately logs
  nothing to `watch.log`; and `watch.lock` exists only while a tick runs. `rowt
  monitor` reads it for the header's `watch` cell — `on · 1m`, or `stalled ·
  14m` once no tick has completed in twice the `StartInterval`. Stamped at exit
  rather than entry so "stalled" means exactly one thing. An agent the 3.5.5
  self-heal left unloaded (2026-09-21) sat invisible for an hour; this would
  have shown it within four minutes.

  That self-heal — a tick rewriting an agent plist an older rowt installed —
  re-bootstraps the agent from a detached child, because `bootout` kills the
  tick's own process group. The child waits for the old job to be *gone*
  before it bootstraps (`bootout` is asynchronous; a bootstrap over a job still
  tearing down fails), gives up after 10 s rather than bootstrap over a job
  that will not leave, verifies with `launchctl list`, and writes the real
  outcome to `watch.log` itself — the tick that spawned it is dead by then.
  `rowt status` carries a `watchdog:` line (loaded / NOT loaded / not
  installed, plus whether the plist is from an older rowt), since a status that
  said nothing about the agent read as healthy through that hour.
- **Captive portals (hotel/airport Wi-Fi).** Pre-login walled gardens make the
  login popup vanish and the portal page unloadable while the proxy is on. rowt
  handles this automatically — probe hosts on the proxy bypass so the popup
  appears, and the `watch` agent drops/restores the system proxy around the
  login. Full design, state machine, and the offline test harness: **§11**.
- **A corporate auto-proxy (PAC) in front of rowt.** macOS resolves an
  auto-proxy configuration **before** the manual proxy settings, so while one is
  enabled the system is not using rowt however correctly rowt's three protocols
  are set. Every check rowt had read the manual getters and none read this one,
  so `rowt proxy check` could print "✓ system proxy fully configured" and be
  wrong: the failure is silent by construction. `proxy check` and `status` now
  say when a PAC is in front, and print its URL.

  **rowt does not turn it off, and nothing automated treats it as broken.** A
  PAC is normally pushed by a corporate client on VPN connect, and disabling it
  is the "silently modify what MDM/EDR manages" that FUTURE.md §7 rules out.
  More narrowly, `_proxy_pointing_ok` ignores it *on purpose*: the watchdog's
  remedy is `cmd_reload`, a reload does not turn a PAC off, and so treating it
  as "proxy wrong" would reload every tick forever. For the same reason the
  warning stays out of `proxy check`'s exit status — `install.sh` answers
  non-zero with `proxy on` plus a router restart, neither of which touches a
  PAC, so it would buy a pointless restart on every upgrade.

  To route through rowt with a PAC enabled, either disable the auto-proxy
  (System Settings › Network › Details › Proxies), or use `rowt proxy env` for
  CLI tools: those exports name `127.0.0.1:$PORT` directly and ignore the system
  setting entirely.

  **A watchdog reload names which of its two causes fired.** The netcheck phase
  reloads when the bound interface moved *or* when the system proxy stopped
  pointing at rowt, and it used to call both "network change" — so a reload
  caused by something re-pointing the proxy logged
  `network change (iface 'en0' -> 'en0')`: an interface that had not moved, in a
  message about the interface moving. Four divergence bundles in three weeks read
  that way and were investigated as a network problem. The proxy case now says
  `system proxy on '<service>' no longer points at rowt (iface '<if>' unchanged,
  …)`, in `watch.log` and in the audit log. The two implementations build the
  same three strings; `parity watch-shadow`'s `watch-proxy-repoint` case holds
  them to it.

- **A lane that is reachable but far too slow (`rowt speed`).** Every other
  signal here is reachability — a small request completes or it does not — and
  that is structurally blind to a lane answering 200 on small responses while
  sustaining a few hundred KB/s. `rowt speed` fetches a capped range twice, once
  through the router and once with the proxy **bypassed**, and prints a verdict
  per row. The second fetch is the whole point: it is the one measurement that
  separates "rowt's relay is slow" from "this path is slow", and its absence is
  how a corp tunnel's own 1294-byte mtu was reported as a rowt bug
  (archive/BUG-corp-lane-throughput.md — the same object was no faster with
  rowt out of the picture entirely).

  **The threshold is a ratio, and there is deliberately no absolute floor.**
  Below half the bypassed rate is called out as rowt's own bottleneck. The 2x
  comes from that report's repeats — 226/213/212 KB/s proxied against
  253/208/177 bypassed, so one lane's samples already span ~1.4x — and anything
  tighter would report noise. A fixed "too slow" number is wrong in both
  directions: 226 KB/s trips every intuition and rowt was innocent, while on a
  hotel uplink every lane would trip it and the warning would mean nothing. The
  absolute rate is already in the column; what a human cannot do at a glance is
  the attribution.

  **And it never acts.** rowt does not disable a lane, or move its traffic, for
  being slow — however slow. Fail-closed exists to stop traffic taking the
  *wrong* path: escape down means the request would leave unproxied, so dropping
  it is the safe answer. A slow lane is still the *right* path, and refusing it
  has only two outcomes, both worse than slow — strand a user whose only route to
  that host is the tunnel, or push the traffic onto a lane their own policy
  excluded (corp traffic out the physical NIC is a leak, not a fallback).
  Degraded beats broken, and beats a silent policy violation. `speed` therefore
  exits 0 on every verdict: a non-zero status is refusal by the back door, since
  scripts would gate on it.

- **Diagnosing after the fact (audit log).** Every mutating operation — a CLI
  command you ran or an action the `watch` agent took — appends a line to
  `~/.config/rowt/log/audit.log` (`rowt audit`): `BEGIN`/`END`/`ABORT`, timing,
  and a `by=<parent>(<tty>)` field. That field disambiguates a hands-on
  `rowt down` (`by=zsh`) from a watchdog reload (`by=launchd`) — the exact
  question that was unanswerable when a router went down mid-incident. `BEGIN`
  is written before the work, so a command that *hangs* (e.g. proxy teardown
  stalling on a dead network) still leaves a trace. Read-only commands and the
  high-frequency `watch tick` no-op aren't recorded. Included in `rowt report`.

## 11. Captive portals: detection, drop, restore

Public hotspots (airports, hotels, lounges) gate internet access behind a login
page. This is the one situation where rowt's own strengths turn against it, so
the handling is worth spelling out — you will next need this months from now,
standing in a lounge.

### The trap: why proxy-on makes the login page unreachable

Pre-login, the hotspot's gateway blocks everything except its own whitelist and
announces the portal by **hijacking DNS / plain HTTP** — the redirect it injects
is the whole signaling mechanism. macOS shows the Captive Network Assistant
(the login popup) only when its probe of `captive.apple.com/hotspot-detect.html`
returns **hijacked content**; a network *error* does not count. Three rowt
behaviors then compound, each correct everywhere else:

1. **The probe honours the system proxy**, so it arrives at rowt and rides the
   direct lane.
2. The direct lane resolves via **DoH to 223.5.5.5:443** — blocked pre-login, so
   the probe dies with an error, not a redirect → **no popup**. Worse, DoH is
   *immunity to DNS hijack* — and the hijack is how the portal announces itself.
3. The portal page itself often lives on a **public hostname** the gateway
   whitelists (observed in the wild at an airport lounge, and on every United
   flight: `www.unitedwifi.com`) — public, so the private-range bypass doesn't
   cover it; through the proxy it's as dead as everything else. And the browser
   asks for it *before* the watchdog has had a tick to notice the portal, so
   the first request — the one that would have loaded the login page — dies on
   the proxy, and there is no second one to rescue.

Pre-login, the only working path is *direct socket + the hotspot's own DNS* —
i.e. exactly what "system proxy off" restores. Hence the design: get the popup
to appear, automate the proxy-off/-on dance around the login — and for venues
you know, take the race out of it altogether.

### Three defenses, independent and complementary

1. **Probe hosts on the proxy bypass list** (`_proxy_bypass_want`): 
   `captive.apple.com`, `connectivitycheck.gstatic.com` (Chrome),
   `detectportal.firefox.com`, `www.msftconnecttest.com` (Windows). The OS/browser
   probes always go direct, the hijack reaches them, and **the popup appears even
   with the proxy on**. These endpoints carry nothing but "am I online", so the
   bypass costs no privacy. The bypass *setter* builds its arguments from
   `_proxy_bypass_want`, so the checker and setter cannot drift.
2. **The hotspot lane** (`rowt hotspot add unitedwifi.com`,
   `~/.config/rowt/hotspot-domains.txt`): the venues' portal hostnames, appended
   to the same bypass list — as `unitedwifi.com` *and* `*.unitedwifi.com`,
   because macOS matches a bare entry exactly. The OS and every system-proxy
   app then reach the portal directly, proxy on or off, so the login page loads
   on the browser's *first* request and defense 3 never has to win a race. It is
   the fix for point 3 above, and the only one that is not timing-sensitive.
   Not a routing lane: nothing in it is rendered, an edit re-applies the bypass
   list (where rowt owns the proxy) rather than restarting the router, and the
   single-lane invariant covers it — adding a name to a routing lane pulls it
   out of hotspot, since the OS-level bypass would make that routing entry dead.
   Two things that used to go wrong with a venue's domain are closed by it:
   `corp sync` no longer mirrors a DHCP-advertised domain that sits in the lane
   (a venue advertising its own domain is the usual case — it kept landing in
   the corp lane, where it is not just useless but harmful once a corp VPN is
   up), and `proxy env` / `rowt run` export the list as `no_proxy`, so the CLI
   channel — which ignores the macOS list — gets the same exemptions.
3. **The watchdog's captive state machine** — each tick starts with a **direct**
   probe (`_captive_state`: no proxy, 6 s cap) whose host is resolved at **the
   physical NIC's DHCP resolver** — `ipconfig getoption en0 domain_name_server`,
   then `dig` at that server, pinned into curl with `--resolve` (macOS curl has
   no `--dns-servers`). That resolver is the hotspot's own DNS, which is where
   the hijack lives; the *system* resolver is not that server once a VPN is up
   or the user pinned one, and a probe resolved elsewhere sails past the hijack
   and reads "clear" from inside a walled garden. No DHCP resolver, no answer,
   or a probe URL given by IP → the plain probe, as before. Right after a
   **network change** (net_id ≠ the one netcheck last wrote) an `unknown` is
   re-probed after `ROWT_CAPTIVE_RETRY` seconds each (default `5,10`), so the
   drop lands on the tick WatchPaths fired and not two minutes later.
   And when the named probe cannot get a request out at all, a **DNS-free
   fallback** asks again without any lookup: a portal's gateway intercepts
   port 80 for an unauthenticated client whatever the destination, so a request
   to `ROWT_CAPTIVE_FALLBACK` (default `http://192.0.2.1/`, TEST-NET-1, which is
   never routed) still draws the redirect. This is the case a real hotel
   produced on 2026-09-14: it refused every lookup until login, so the probe
   never sent a request and the verdict sat at `unknown` for six minutes while
   the portal waited. The fallback is deliberately stricter than the named
   probe — **only a redirect or 511 counts, never a 200** — because without a
   name behind it a 200 could be a router admin page or a hotel TV, and acting
   on that would drop the proxy on a network that was merely slow.
   On the drop it also **opens the portal's login page in the browser** — the
   redirect target the probe was sent to, or the probe URL itself when the
   portal served its page under it — because the request the OS made before
   the drop died on the proxy and nothing retries it. Once per episode, http(s)
   only, logged and audited like the drop:

```
              probe result each tick (WatchPaths fires one on network join)
   ┌─────────┐  captive: 3xx | 511 | 200-that-isn't-Success  ┌──────────────────┐
   │ normal  │ ────────────────────────────────────────────▶ │ captive          │
   │  tick   │   drop system proxy ONCE (intent untouched),  │  every tick:     │
   │         │   open the login page, sset captive=1, audit  │  captive → exit  │
   └─────────┘                                               │  unknown → exit  │
        ▲                                                    └──────────────────┘
        │            clear: the genuine Success page                  │
        └─────────────────────────────────────────────────────────────┘
          restore proxy (only if the router is up), sset captive="",
          audit, then FALL THROUGH to a normal tick (reconciles drift)
```

### The decisions, and why

| decision | why |
|---|---|
| probe **direct**, never via the proxy | the question is "is a portal between `en0` and the internet", not "does the proxy work" |
| a **DNS-free fallback** when the named probe cannot send at all | the pin above assumes the garden HIJACKS DNS; plenty of them simply refuse to resolve until you authenticate, and then there is no request to classify and no redirect to see. An intercepted request to an address that is never routed needs no name and still draws the portal out. Strict on purpose (redirect or 511 only): on a healthy network nothing can answer that address, so it cannot invent a portal, and a 200 from an unidentified box is not evidence of one |
| resolve the probe host at the **NIC's DHCP resolver**, not the system one | the portal announces itself by hijacking *its own* DNS; a system resolver pinned elsewhere (a VPN's, a manual `8.8.8.8`) answers truthfully and the probe reads "clear" behind the wall. Falls back to the plain probe when there is no DHCP resolver or it does not answer, so nothing that worked before is lost |
| re-probe an `unknown` only **right after a network change** | the link is still settling and the portal's DNS may not answer yet; three tries (15 s of waiting, ≈33 s worst case with every probe timing out, and a probe that could not be pinned re-resolves before each retry) put the drop on THIS tick instead of the next timer tick. Not mid-episode or in steady state: there `unknown` is the hands-off answer it always was, and one probe per tick is the budget. (Mid-episode `net_id` stays "moved" — netcheck never runs while captive — but a captive tick answers `captive`, not `unknown`, so it does not burst.) The burst is silent: a log line would be an action the planner never took |
| `unknown` (timeout/offline) is **never** captive | a flaky network or a dead probe host must not be able to drop your proxy; only definite portal evidence acts |
| `511` is captive, `403` is **not** | 511 is RFC 6585's "Network Authentication Required" — a portal saying so; a 403 comes from a corp web filter as often as from a portal, and acting on it would hold the proxy off for the whole office day (the episode never clears there) |
| open the login page **on the drop**, once | the browser's own first request died on the proxy before this tick ran; opening the page after the drop is the retry the OS does not make. Inside the transition, so a manual `proxy on` mid-login never re-opens it; http(s) only, so a hijacked probe cannot open anything else |
| drop **once per episode** (`captive` state guards the transition) | if you manually `proxy on` mid-login, the watchdog must not fight you |
| `proxy_intent` is **never touched** | intent is *the user's wish*; the `captive` state key records *why reality differs*. This also keeps the intent-off early-out intact: a deliberately-off proxy skips all of this |
| recovery/reload **suppressed** while captive | tunnel probes and reloads all dead-end against the wall; they would burn the recovery cooldown and re-assert the proxy over the login page |
| recovery needs a **`clear`** verdict, not merely "not captive" | recovery is for a tunnel wedged *while the network works*. `unknown` means the probe reached nothing at all, and no amount of restarting sing-box fixes a broken uplink. Observed 2026-09-14: three failed tunnel probes in a hotel garden fired a reload that could not possibly succeed, burned the 600 s cooldown, and logged a failure that reads as a tunnel problem |
| an `unknown` must be **corroborated** before it suppresses recovery | `unknown` is ambiguous — a garden that blocks DNS, a dead uplink, and a genuinely wedged tunnel all produce it, and only the first wants recovery held. Either the **default gateway answers** while the probe host does not (the local link is up, something upstream is gating us), or the **network changed within 15 min** (a portal is likely then; a wedge is not). With neither, the old behaviour stands |
| the suppression **expires** at 3× the health threshold | a probe host that becomes permanently unreachable must degrade back to the old behaviour rather than wedge self-healing for good. Only reachable while the tunnel is also failing, so a dead probe host on a healthy tunnel never gets here |
| `ROWT_CAPTIVE_CHECK=0` **exempts** all of the above | with the check off every verdict is `unknown` by construction, so keying on the verdict alone would read "never recover" — silently disabling self-healing for anyone who turned portal detection off, which is far worse than one wasted reload. Carried as its own observation field, not as a missing verdict: a missing verdict is what gates the **discovery journal** |
| restore **only if the router is up** | never point the system proxy at a dead port; the normal recovery path handles that case after the fall-through |
| restore **falls through to a normal tick** | anything that drifted while walled off (bypass list, bind iface, proxy pointing) gets reconciled immediately, not at the next tick |

### What you observe at a hotspot

Join Wi-Fi → WatchPaths fires a tick within seconds → proxy drops (`watch.log`
logs it, `rowt audit` records it, `rowt status` shows `captive: portal detected…`)
→ the popup appears (bypassed probe) and the portal page opens in the browser →
log in → the next tick (≤ `ROWT_WATCH_INTERVAL`, default 120 s, or sooner if the
network re-signals) sees Success → proxy restored, `captive` cleared, audit
closes the episode.

Knobs: `ROWT_CAPTIVE_CHECK=0` disables all of it; `ROWT_CAPTIVE_URL` /
`ROWT_CAPTIVE_TIMEOUT` re-point/re-pace the probe; `ROWT_CAPTIVE_RETRY` is the
post-change burst (comma-separated seconds, default `5,10`; empty = one probe);
`ROWT_CAPTIVE_FALLBACK` is the DNS-free target (default `http://192.0.2.1/`).
All four are baked into the LaunchAgent by `rowt watch install` if they are set
in the shell that installs it (they were NOT before 3.5.0, so setting
`ROWT_CAPTIVE_CHECK=0` had no effect on the installed watchdog); change one and
re-run `rowt watch install`. Without `rowt watch install` none of this runs —
the manual dance is `rowt proxy off` → log in → `proxy on`.

### Debugging it

- `rowt status` — a `captive:` line means the watchdog is holding the proxy off.
- `rowt proxy status` — the bypass list as macOS holds it; the venue's portal
  host (and its `*.` twin) must be in it, or the hotspot lane is not applied
  (`rowt proxy on` re-applies it; `rowt hotspot list` shows the lane).
- `~/.config/rowt/log/watch.log` — "captive portal detected/cleared" lines.
- `~/.config/rowt/log/captive.log` — **why** a probe came back the way it did:
  one line per non-clear probe, naming the attempt (`first`, `retry+5s`,
  `dns-free`), curl's exit status (6 could not resolve, 7 could not connect,
  28 timed out) or the HTTP status, and the pinned resolver when there was one.
  `unknown` used to be three words wide and indistinguishable — a name that
  would not resolve, a black-holed SYN and a six-second timeout all looked the
  same, which is why diagnosing the 2026-09-14 hotel took log archaeology and
  produced a reading of the evidence rather than a fact. Telemetry only: it is
  deliberately NOT `watch.log` (watch-diff reconstructs bash's *actions* from
  that file) and NOT the discovery journal (its line is hashed into
  `discovery_sig`), so it sits outside every compared surface.
- `rowt audit` — the drop/restore pair with `by=launchd` attribution.
- Reproduce the probe by hand:
  `curl -s --noproxy '*' --max-time 6 -w '\n%{http_code}\nredirect=%{redirect_url}' http://captive.apple.com/hotspot-detect.html`
  — `Success` body = clear; anything else = what the watchdog saw, and the last
  line is the page it would open. To see it the way the tick does, resolve at
  the NIC's DHCP server first — `ns=$(ipconfig getoption en0 domain_name_server)`,
  `ip=$(dig +short @$ns captive.apple.com | grep -E '^[0-9.]+$' | head -1)` —
  and add `--resolve captive.apple.com:80:$ip`. If the two disagree, the
  system resolver is not the hotspot's, and that is the case the pin exists for.
- Stuck in `captive` wrongly (e.g. a middlebox rewrote the probe)? `rowt proxy on`
  re-asserts immediately (the state clears on the next clear tick), and
  `ROWT_CAPTIVE_CHECK=0` in the environment of `watch` disables detection.

### Re-verifying without an airport

`config/fake-portal.py` serves the three portal behaviors on `127.0.0.1:8099`
(`/portal` = 200 login page, `/redirect` = 302, `/success` = the Apple body).
Run real ticks against it — **note each toggles the real system proxy for a few
seconds**:

| step | command (`ROWT_CAPTIVE_URL=…`) | expect |
|---|---|---|
| portal appears | `…8099/portal bash bin/rowt watch tick` | proxy drops, `captive=1` in `state`, watch.log + audit lines, `rowt status` shows `captive:`, **the browser opens the fake portal page** |
| portal redirects | `…8099/redirect bash bin/rowt watch tick` (after a `…/success` tick) | as above, but the browser is sent to the redirect target (`http://portal.fake/login` — expect a DNS error there; the `open` is the point) |
| still captive | same again | exit 0, **zero** new log lines |
| probe dies | `…127.0.0.1:9/x …watch tick` | hands-off: state and proxy unchanged |
| login clears | `…8099/success …watch tick` | proxy restored, `captive=` empty, audit closes |

This is exactly the sequence the feature shipped with (verified live 2026-08-08).

## 12. Ideas / TODO (not built yet — need design)

- **Desktop app / menu-bar widget.** A native-feeling GUI (menu-bar item or small
  window) to see status at a glance and drive the common operations without the
  terminal: current mode (host/vm) + selected server + latency, router/proxy
  on-off with a toggle, per-lane connection counts + throughput, and quick actions
  (proxy on/off, switch server, restart, add a domain to a lane). Same data
  sources as `rowt monitor` (clash API, `lane-*.log`, `rowt status`). Design
  questions: framework + dependency footprint (SwiftUI menu-bar app vs. a
  cross-toolkit like Tauri/Electron vs. something lightweight like `xbar`/SwiftBar
  plugins that just shell out to `rowt`); how it talks to rowt (shell out to the
  CLI vs. read the clash API directly vs. a small local rowt daemon/socket); how
  it handles the sudo-gated `networksetup` calls (helper tool vs. reuse the
  existing scoped sudoers); packaging/signing/notarization and auto-start; and
  whether it replaces or complements `rowt monitor`.
