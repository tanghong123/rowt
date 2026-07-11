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
| `domain_suffix` in `block-domains.txt` | **block** | refused instantly — no DNS, no dial (kills the ad/tracker retry storm) |
| `domain_suffix` in `corp-domains.txt` | **corp** | resolved to an intranet IP (see §4), sent via the **routing table** → matches the corp route → into `utunN` → corp network |
| `ip_cidr` in `corp-domains.txt` (e.g. `10.0.0.0/8`) | **corp** | literal intranet IP → routing table → `utunN` → corp |
| `domain_suffix` in `escape-domains.txt` | **escape** | VLESS socket **bound to en0** → home router → VPS → the VPS reaches Google |
| `geosite-category-ads-all` rule-set | **block** | broad ad/tracker blocklist (opaque, so it runs *after* the hand lists) |
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
    - local        (address: "local"  → the macOS system resolver)
    - dns-direct   (223.5.5.5, detour: "direct" → queried over en0)
  rules:
    - domain_suffix in corp-domains.txt → server: local
  final: dns-direct
```

- **`escape` names are never resolved locally.** The route rule matches on the
  *sniffed* domain, and the VLESS outbound forwards that domain to the VPS, which
  resolves it at the exit. So `google.com` is **not** looked up by Chinese DNS
  (no poisoning) nor by corp DNS (no leak of what you browse). This is why
  domain-based rules + sniffing matter.
- **`corp` names → `local`.** `address: "local"` means sing-box calls the macOS
  system resolver, which — with the corp VPN up — is corp DNS (plus any
  `/etc/resolver/<domain>` entries the `networking/` tool installs). So intranet
  names resolve to intranet IPs, and rule #1/#4 send them into the corp tunnel.
- **Everything else → `dns-direct`** (AliDNS `223.5.5.5`), and crucially the
  query is sent through the **`direct`** outbound, i.e. **over `en0`**. So Baidu
  is resolved by a Chinese resolver on your home line — China-optimal IPs, and
  the lookup itself never touches corp DNS.

Net effect with corp ON: intranet lookups use corp DNS over the corp tunnel;
your personal/Chinese lookups use AliDNS over your home line; escaped sites are
resolved remotely by the VPS. Three worlds, no cross-contamination.

## 5. Two ways the escape uplink bypasses corp (modes)

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

In both modes the app-facing proxy is still `127.0.0.1:7890`; only the `escape`
outbound's implementation differs.

## 6. What the proxy catches — and what it doesn't

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

## 7. Mapping a Shadowrocket config onto escape

Shadowrocket uses the Surge-style config format. The pieces map like this:

Use `rowt server import` to do this automatically (it decodes Shadowrocket's
server store + rules into an editable review file). The mapping:

| Shadowrocket | rowt |
|--------------|------------|
| `[Proxy]` entries / subscription URLs | servers — `rowt server add` / `subscribe` / `from-sr` (VLESS + AnyTLS; SS etc. skipped) |
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

## 8. Failure modes

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
```

## 9. Ideas / TODO (not built yet — need design)

- **TUI monitoring view.** A live terminal UI, launchable from the CLI (e.g.
  `rowt monitor` / `rowt top`), that shows real-time state in one screen: active
  connections per lane (escape/corp/direct/block) with throughput, the selected
  server + latency, recent per-lane errors, router/proxy/mode status, and CPU.
  Would build on the data we already have — the clash API (`/connections`,
  `/traffic`) that `rowt connections` reads, the per-lane `lane-*.log` files that
  `rowt <lane> errors` summarizes, and `rowt status`. Design questions: refresh
  model + dependency footprint (pure-bash/ANSI redraw vs. a dep like a Go/Rust
  TUI or Python `textual`/`rich` — rowt currently only needs `jq`/`python3`);
  keyboard actions (switch server, toggle proxy, pause a lane); and whether it's
  a subcommand vs. a separate small binary the CLI launches.

- **Desktop app / menu-bar widget.** A native-feeling GUI (menu-bar item or small
  window) to see status at a glance and drive the common operations without the
  terminal: current mode (host/vm) + selected server + latency, router/proxy
  on-off with a toggle, per-lane connection counts + throughput, and quick actions
  (proxy on/off, switch server, restart, add a domain to a lane). Same data
  sources as the TUI idea above (clash API, `lane-*.log`, `rowt status`). Design
  questions: framework + dependency footprint (SwiftUI menu-bar app vs. a
  cross-toolkit like Tauri/Electron vs. something lightweight like `xbar`/SwiftBar
  plugins that just shell out to `rowt`); how it talks to rowt (shell out to the
  CLI vs. read the clash API directly vs. a small local rowt daemon/socket); how
  it handles the sudo-gated `networksetup` calls (helper tool vs. reuse the
  existing scoped sudoers); packaging/signing/notarization and auto-start; and
  whether it replaces or complements the TUI view.
