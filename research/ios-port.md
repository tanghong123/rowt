# rowt on iPhone — feasibility, architecture, core reuse, UX

*Design survey, 2026-09-24. Nothing here is built. The survey
covers rowt 3.5.8 with sing-box pinned at 1.13.14. The question it answers:
can rowt keep its multi-lane routing (escape / corp / direct / block /
hotspot) on an iPhone **as a proxy**, the way it runs on macOS, next to a
corporate VPN?*

Every platform claim carries a tag. **[P]** means verified from a primary
source: Apple documentation or technotes, an Apple staff answer on the
developer forums, a WWDC session, or sing-box source and docs. **[S]** means
a secondary source, such as a vendor KB, a blog, or a paper. **[I]** means
inference, usually a claim to settle with a device test (listed in §6.3).
The bracketed keys (`A1`, `F3`, `B5`, and so on) point into §7.

---

## 1. Verdict

### 1.1 The answer

**Conditional go.** In one sentence each:

- **No-go: the macOS model does not exist on iOS.** On macOS, rowt runs as a
  proxy *next to* the corp VPN client, and the OS routing table hands the corp
  lane to the corp tunnel. On iOS, rowt and an app-provided corp VPN cannot
  both be up. They become a toggle, not coexistence. No shipping proxy app
  gets around this either (§1.2).
- **Go: rowt's routing engine runs on an iPhone.** It becomes a
  NetworkExtension packet-tunnel app with sing-box embedded as a library
  (libbox). That is the architecture of sing-box's own iOS client [P B1].
  Surge, Shadowrocket, Stash and the other rule-based proxies on iOS also
  install a VPN configuration to do their work [I].
  The escape, direct, block and hotspot lanes all survive, along with auto
  server selection and local mode. It works on Wi-Fi and cellular, and it
  covers every app, UDP/QUIC included, not only the apps that honor a proxy.
- **The corp lane survives only if it rides *inside* rowt's tunnel.** That
  means one of three things:
  - (i) the corp VPN speaks a protocol sing-box implements;
  - (ii) rowt relays corp traffic to a machine that already has corp access;
  - (iii) the corp VPN is IKEv2/IPsec in iOS's one sanctioned second slot, the
    Personal VPN.
  For the proprietary corp client this repo's author uses, (i) and (iii) are
  ruled out. (ii) is technically ready today and is a **policy** decision.
- **Two partial alternatives keep the corp client connected.** Both run the
  engine off the phone:
  - (g) iOS 17 **network relays** can carry the escape lane without taking the
    VPN slot. This is unproven and needs a one-day device spike.
  - (f) **Wi-Fi proxy/PAC** pointing at rowt on the Mac or a home box. It
    covers home Wi-Fi and proxy-aware apps only.

### 1.2 Why: four constraints, each sufficient on its own

1. **One enterprise VPN at a time.** Packet-tunnel configurations are
   "enterprise VPN" configurations, and *"Only one enterprise VPN
   configuration can be enabled on the system at a time"* [P A1]. rowt's
   configuration and any app-provided corp client's configuration are both
   in that class. DTS confirmed it: *"only one NEPacketTunnelProvider can run
   on the system at one time because they are considered Enterprise VPNs"*
   [P F1, F2]. The one sanctioned exception is a **Personal VPN**, a configuration
   an app creates with `NEVPNManager` using the *built-in* IKEv2/IPsec client.
   It may stay connected alongside an enterprise VPN, but it only gets traffic
   the enterprise VPN does not route [P A1, A2, F1].
   - **Split routes do not help.** The limit is on *enabled configurations*,
     not on overlapping routes. Enabling rowt's configuration disables the
     corp client's, whatever `includedRoutes` either one declares [P A1].
     Claiming only the escape CIDRs changes nothing.
   - **No shipping app gets around it.** The rule applies to every app VPN
     alike [P A1, F1]. Tailscale documents it: *"iOS and Android enforce a
     limit of running only one VPN at a time"* [S X1]. Surge's FAQ covers
     running beside another VPN only for Surge **Mac** [S X8]. The field
     workaround is absorption: users fold the other VPN, as WireGuard, into
     the proxy app [S X7]. That is option (b) below.
2. **No place to run a proxy outside that slot.**
   - iOS suspends an app *"shortly after the user has moved it to the
     background"*. There is *"no general-purpose mechanism for … running code
     continuously in the background"* [P F5].
   - A suspended app's listening socket still accepts connections but never
     serves them, and the system can reclaim its resources [P A20].
   - Using background audio or location to stay alive breaks App Review
     guideline 2.5.4 [P A18].
   - The sanctioned long-running network processes are NetworkExtension
     providers, and on iOS every one except the packet tunnel is gated.
     App proxy needs a *managed* device. DNS proxy and content filter need a
     *supervised* device [P A5, F6]. The Global HTTP Proxy payload needs
     supervision [P A12].
3. **A tunnel just to keep a proxy alive is unsupported, and it still takes
   the slot.** Apple's technote says: *"Do not use a packet tunnel provider to
   host a network listener or proxy server"* [P A4]. DTS in April 2026: such
   uses *"tend to be very brittle"* and are not supported [P F8]. WWDC25
   gives the same guidance: packet tunnels are for tunnelling IP traffic
   [P W2]. A no-route
   tunnel is still an enterprise VPN configuration, so constraint 1 applies
   anyway.
4. **There is no `bind_interface` for traffic rowt did not originate.** On
   macOS, `direct` and `escape` bypass a full-tunnel corp VPN because
   sing-box binds their sockets to `en0` (DESIGN.md §2). On iOS only the
   tunnel provider's *own* sockets can be steered that way. System-level
   mechanisms like relays, Wi-Fi proxies and PAC `DIRECT` follow the routing
   table. So with a full-tunnel corp VPN up, "direct" goes into the corp VPN
   [I, from A6].

### 1.3 Check these first, on the phone, before writing any code

These four facts decide more than any option row in §2.

| # | Check | How | What it flips |
|---|---|---|---|
| 1 | Where does the corp client show up? | Settings › General › VPN & Device Management › VPN: under **VPN** or under **Personal VPN**? | If under *Personal VPN*, rowt's tunnel can coexist with it [P A1]. The corp lane then goes unbound into it, as on macOS (to confirm with Q5, Q8). If under *VPN* (expected for a proprietary TLS client [I]), it cannot. |
| 2 | Is the corp client split- or full-tunnel on iOS? | Connect it and load an egress-IP page. If the egress is corp's, it is full tunnel. | Full tunnel kills option (g) (the relay connection itself rides corp) and option (f5). With split tunnel, both stay alive. |
| 3 | Is the phone MDM-managed or supervised? | Settings › General › VPN & Device Management | Supervised or managed reopens app proxy, DNS proxy and Global HTTP Proxy (§2 row d). It may also *forbid* installing rowt's VPN. |
| 4 | What does corp policy say about (a) third-party clients for the corp VPN and (b) relaying corp access through a personal machine? | Ask. | Transports (i) and (ii) in §3.7 are only as good as the answer. |

---

## 2. Feasibility matrix

### 2.1 The matrix

"Lanes" means which of escape / corp / direct / block / hotspot survive.
Coverage "all apps" means TUN capture. "proxy-aware" means apps built on
URLSession/WebKit, which honor system proxies; lower-level stacks do not
[P F7].

| Option | How it works | Lanes | Coexists with the corp client? | Wi-Fi / cellular | Survives background? | MDM / supervision | App Store | Verdict |
|---|---|---|---|---|---|---|---|---|
| **(a)** packet tunnel + libbox (TUN) | NE extension runs sing-box on the tunnel's utun fd | escape, direct, block, hotspot; corp only via (b) | **No**, one enterprise slot [P A1] | both, all apps | yes: the NE is the sanctioned host; on-demand should restart it [I] | none | yes in principle (org account); volatile: sing-box's own client is back as of 1.14.0 [P B5], while its docs still say it can't update [P B6] | **GO**: the engine |
| (a′) packet tunnel, proxy-only | `auto_route` off + `platform.http_proxy` → the in-extension mixed inbound [P B7] | as (a), proxy-aware apps only | No, still the slot | both | yes | none | risky: TN3120 names it [P A4] | **Don't build**: all of (a)'s cost, less coverage |
| **(b1)** corp VPN *as a sing-box endpoint* | OpenConnect (AnyConnect, GlobalProtect, Fortinet, F5, Pulse, Juniper NC), OpenVPN, WireGuard, Tailscale endpoints inside (a) [P B5, B8] | all five | by absorption: the official client is not running | both | yes | none | yes | **Conditional**: only for those protocols, and only if policy permits. **Not** the proprietary client |
| **(b2)** corp lane relayed to the Mac | corp outbound = proxy to the Mac's rowt over a tailnet, via sing-box's *in-process* Tailscale endpoint; `rowt-share-on` already publishes it | all five | the phone needs no corp VPN | both (wherever the tailnet reaches) | yes | none | yes | **Technically ready; policy-gated**; opt-in |
| (b3) corp VPN as a Personal VPN | the corp's IKEv2/IPsec in the second slot; rowt excludes corp CIDRs from its tunnel | all five | **Yes** [P A1, F1]; routing to be confirmed (Q5, Q8) | both | yes | none | yes | **Conditional**: only if the corp offers IKEv2/IPsec |
| **(c)** local proxy in the app, no VPN | app listens on 127.0.0.1; Wi-Fi proxy points at it | while foregrounded only | yes | Wi-Fi only | **no**: suspended, socket dead [P F5, A20] | none | no (2.5.4 forbids keep-alive tricks) | **NO-GO** |
| (d) app proxy / DNS proxy / Global HTTP Proxy / proxy payloads | NE app proxy; NE DNS proxy; `com.apple.proxy.http.global`; Wi-Fi payload `ProxyType`; APN proxy | depends on the engine behind it | app proxy is per-app VPN | Global proxy: both; Wi-Fi payload: Wi-Fi | n/a (config only) | app proxy **managed**; DNS proxy / filter / global proxy **supervised** [P A5, A12]; Wi-Fi and APN payloads need neither [P A13, A14] | — | **Dead on a personal phone**, except the Wi-Fi/APN proxy payloads, which only *front* (e)/(f) |
| **(e)** PAC-only lanes | PAC generated from the lanes: escape → `PROXY <engine>`, corp/direct → `DIRECT`, block → a dead proxy | escape and block need an engine *somewhere*; direct cannot bypass full-tunnel corp [I] | yes | Wi-Fi (cellular only via the supervised global proxy or the fragile APN proxy) | n/a | none | n/a | **A front end for (f)**, not an engine |
| **(f1)** Wi-Fi proxy/PAC → rowt on the Mac/home box (LAN) | the phone's Wi-Fi proxy is `mac:port` or a PAC URL served there | all five, **computed on the Mac**; the Mac's corp lane serves the phone's corp traffic | yes; the phone needs no VPN at home | that Wi-Fi only; proxy-aware apps | n/a | none | n/a | **GO as a stopgap**. Needs a LAN listen + auth in rowt (§4) |
| (f2) … over the Tailscale app | Tailscale's iOS app, then proxy to the Mac's tailnet IP | as (f1) | **No**: the Tailscale app *is* a VPN [S X1] | per-network proxy settings | yes | none | n/a | Strictly worse than (b2), which does it in-process |
| (f3) … over the corp VPN | proxy on the Mac's corp-side address | as (f1) | yes | both | n/a | none | n/a | **No**: publishes a proxy onto the corp network |
| (f4) rowt on a router | sing-box TUN on OpenWrt (Linux, Phase 5) | escape, direct, block | yes | home Wi-Fi, all apps | n/a | none | n/a | **GO for home**; outside this doc |
| (f5) Personal VPN (IKEv2) → rowt gateway | the rowt app configures IKEv2 via `NEVPNManager` to a domestic strongSwan + sing-box TUN box | all but corp, computed at the gateway; corp stays on the phone's client | **Yes, if the corp client is split-tunnel** [P A1, F1]; DTS calls this *"a very rare case"* | both, all apps | yes | none | yes | **Conditional**: split tunnel plus reachable infra plus Linux TUN |
| **(g)** network relays (`NERelayManager`) | escape suffixes → an HTTP/3 or HTTP/2 relay (CONNECT, CONNECT-UDP) [P A10]; everything else follows the OS | escape only; no block, and no interface-bound direct | **unknown**: undocumented; one third-party report says relays work next to VPNs [S X4] | both | n/a: no process to keep alive | none (entitlement value `relay` [P A11, F11]) | unknown | **SPIKE**: the only lane that doesn't take the VPN slot |

### 2.2 Notes on the rows that decide things

**(a) The engine is proven, and it is also "unsupported".**
- sing-box's Apple client builds `NEPacketTunnelNetworkSettings` from the
  sing-box TUN options. That covers routes, `route_exclude_address` →
  `excludedRoutes`, DNS, and an optional `NEProxySettings` with `matchDomains`
  / `exceptionList` [P B1, A9].
- It takes the utun file descriptor through undocumented KVC,
  `packetFlow.value(forKeyPath: "socket.fileDescriptor")`, and falls back to
  scanning descriptors [P B1].
- It monitors the default interface with `NWPathMonitor` [P B1].

Two caveats:
- TN3120 lists *"selectively claim traffic for the packet tunnel and proxy all
  other traffic elsewhere"* as unsupported [P A4]. That describes every
  rule-based proxy. App Review accepts these apps in practice, but DTS will
  not help, and iOS releases can break them.
- Memory: sing-box's OOM killer and its Go memory limit assume a **50 MiB**
  ceiling under an iOS Network Extension
  (`DefaultAppleNetworkExtensionMemoryLimit = 50 * 1024 * 1024`) [P B4].
  DTS measured 50 MiB for packet tunnels on iOS 16 and says the limits are
  *"unchanged on iOS 26"*. It also warns they are undocumented and *"may well
  change in the future"* [P F3, F4]. OOM kills of sing-box on iOS are a live
  issue [S B11].

**(b) Coexistence by absorption.**
- sing-box 1.14.0 (2026-08-31) added an OpenConnect client (AnyConnect,
  GlobalProtect, Fortinet, F5, Pulse Connect Secure, Juniper NC, with SSO)
  and OpenVPN client and server endpoints. They come with DNS servers that use
  the *pushed* split-DNS resolvers [P B5, B8]. WireGuard and Tailscale
  endpoints already existed.
- With one of these, the corp lane is a route rule to the endpoint inside
  rowt's own tunnel, and the official client never runs. Users of other
  proxy apps do the same thing by folding a WireGuard VPN into Surge
  [S X7].
- Limits:
  - posture checks implemented as wrapper scripts (the endpoint's `csd`,
    `hip` and `tncc` fields) cannot run on iOS, which spawns no processes
    [I];
  - two protocol stacks share the 50 MiB;
  - a proprietary client (this author's) has no endpoint at all.

**(b2) is the macOS design, extended.**
- `rowt-share-on` already runs `tailscale serve --tcp=17890
  tcp://127.0.0.1:7890` and warns that *"tailnet peers allowed by your policy
  can use every rowt lane, including corp"* (`bin/rowt`, shell-init block).
- On the phone, sing-box's Tailscale endpoint joins the tailnet
  *in-process*, so there is no second VPN. The corp outbound becomes a SOCKS
  or HTTP hop to `rowt-mac:17890`.
- Corp names travel as FQDNs and resolve at the Mac against corp DNS. That
  is DESIGN.md §4's "corp names → local" with the Mac as the local.
- It depends on the Mac being awake, rowt up, and the corp client connected.
- It bridges corp access onto a device the corp never enrolled. That is a
  policy question, not an engineering one.

**(c) is closed by the platform, twice.** Background suspension kills the
listener [P F5, A20]. The one process that may stay up is a tunnel, and
hosting a proxy in it is TN3120's explicit example [P A4].

**(d), one footnote: a block lane without the VPN slot exists in
principle.** iOS 26 URL filters work on unmanaged devices [P W2]. But they
see only WebKit and URLSession requests, and they need a Bloom filter plus
a Private Information Retrieval server [P A23]. That is too heavy for a
personal tool.

**(e)/(f) Proxy settings reach proxy-aware apps only.** Per Quinn,
*"NSURLSession takes care of proxies for you (unlike lower-level CFNetwork
APIs)"* [P F7]. That matches DESIGN.md §8 on macOS. Whether a Wi-Fi proxy or
PAC still applies while a **full-tunnel** VPN is connected is not documented.
The SystemConfiguration model suggests the VPN's own proxy settings win once
it owns the default route [I]. That is device test Q1.

**(g) Relays are the only lead that is "a proxy without the VPN slot".**
- **What they are:**
  - iOS 17+ [P A10]; no extension process, so no 50 MiB limit and no
    background problem;
  - `matchDomains` matches a domain *and its subdomains* [P A15];
  - DTS says `matchDomains` also takes CIDRs [P F9], and knows of no fixed
    size limit [P F10];
  - relays speak HTTP/3 with CONNECT-UDP or fall back to HTTP/2 CONNECT
    [P A10, W1];
  - optional DoH at the relay, and synthetic DNS answers so names resolve
    remotely [P A10];
  - saving a relay makes the system contact it to validate [P F11];
  - it needs the `relay` value of the Network Extensions entitlement
    [P A11]. That value's description reads *"when signed with a Developer
    ID profile"*, which is macOS wording. But an iOS developer's save got as
    far as server validation [P F11], so iOS apps can use it. What App
    Review requires of a relay app is not documented.
- **Server side:** sing-box 1.15-alpha's HTTP inbound serves HTTP/2 and
  HTTP/3 with CONNECT-UDP [P B5, B9]. Whether Apple's client works against it
  is unknown (Q4). Envoy is reported to work [S X4].
- **What it costs:**
  - no block lane (two relays may not match the same domain [P F10]);
  - no interface-bound direct;
  - with a full-tunnel corp VPN, the relay connection itself rides corp [I];
  - Apple documents that an active VPN displaces *iCloud Private Relay*
    [P A22]. Whether it also displaces NE relays is exactly the unknown (Q2).
  - A relay dialled straight from the phone to a server abroad is standard
    TLS-in-TLS or QUIC. The GFW demonstrably fingerprints encapsulated TLS
    handshakes and censors QUIC by SNI [S X5, X6]. So put the relay server
    **in-country** (home box or Mac) and let rowt's VLESS/hysteria2 cross
    the border [I].

---

## 3. Recommended architecture

Build **(a) as "rowt for iPhone"** with a pluggable corp transport:
(b1) where the protocol fits, (b2) where policy allows, *off* otherwise. Run
the **(g) spike** in parallel, because it decides whether a
"corp client stays on" mode exists. Document **(f1)** as the zero-app
stopgap, since it only needs a small rowt change on the Mac.

### 3.1 Shape

```
 ┌──────────────── rowt.app (SwiftUI, containing app) ─────────────────┐
 │  lanes · servers · subscriptions · settings   ──►  rowt-core (FFI)  │
 │  import (links, QR, files, "from Mac")        ──►  render → JSON    │
 │  status / Activity via libbox command client       (host + local)   │
 └──────────────┬───────────── App Group container ────────────────────┘
                │ config-host.json, config-local.json, lanes/*.txt, audit.log
 ┌──────────────▼──── PacketTunnel.appex (NEPacketTunnelProvider) ─────┐
 │  libbox (sing-box, gomobile)  — tun fd, NWPathMonitor, OOM killer   │
 │  command server (unix socket in App Group): status, logs, groups,   │
 │    connections, select-outbound, urltest                            │
 │  rowt tick (rowt-core watch FSM, via FFI) on timer + path changes   │
 └─────────────────────────────────────────────────────────────────────┘
   Widgets / Control Center control / App Intents / Share extension:
   read status from the App Group; start and stop via NETunnelProviderManager
```

- **The app renders; the extension only runs.** On iOS, sing-box binds its
  outbound sockets to whatever `NWPathMonitor` reports as the default
  interface [P B1]. So a network change needs no re-render, unlike macOS,
  where `bind_interface: en0` is baked in and the watchdog reloads on
  `iface_moved`. The exception is the `System` corp transport, which may
  force the macOS scheme back (see "The binding catch" in §3.7). The app pre-renders both a host config and a local config.
  When on-demand starts the extension with no app running, the extension
  picks one after its canary probe (§3.5).
- **IPC** reuses libbox's command server over a Unix socket in the App Group,
  as sing-box's client does [P B2]. `sendProviderMessage` carries the few
  rowt-specific verbs: tick now, report, reload.

### 3.2 The memory budget

The 50 MiB covers the Go runtime, sing-box, the TCP/IP stack, rule-sets, and
any Tailscale or OpenConnect endpoint [P B4, F3].

- Keep geosite sets few and binary.
- Adopt sing-box 1.15's new TUN stack once it is stable. The changelog claims
  lower memory [P B5].
- Leave libbox's OOM killer on (the iOS default) [P B4].
- Keep Rust in the extension to the watchdog FSM. The render stays in the
  app.
- (b2)'s Tailscale endpoint and (b1)'s OpenConnect are the budget risks.
  Measure before promising them (Q6).

### 3.3 Lanes on iPhone

| Lane | macOS today | iPhone |
|---|---|---|
| escape | urltest + selector over VLESS/VMess/AnyTLS/hy2 members, each `bind_interface: en0` | same `group()` output, no `bind_interface`; libbox auto-detect binds to Wi-Fi or cellular. DNS: **FakeIP**, because TUN apps resolve before they connect. The fake answer keeps the lookup off Chinese resolvers, and the sniffed domain goes to the VPS. |
| corp | `direct`, unbound, `domain_resolver: local` (routing table → corp utun) | a **`CorpTransport`** (§3.7): `System` (unbound + local DNS; should work **in the office on corp Wi-Fi** with no VPN, and with a Personal-VPN corp [I; Q7, Q8]); `Endpoint` (b1); `Relay` (b2, FQDN forwarded); `Off` (fail closed: *"corp traffic out the physical NIC is a leak, not a fallback"*, DESIGN.md §10) |
| direct | `direct` bound to `en0`; DoH 223.5.5.5 via direct | `direct` (auto-bound); same DNS plan. It can bypass a full-tunnel corp VPN only when rowt *is* the VPN. |
| block | `block` + ads rule-set | unchanged |
| hotspot | not rendered; macOS proxy **bypass list** | becomes a **route rule**: hotspot suffixes → `direct` with DNS via the network's own resolver, so a portal page opened in Safari resolves where the hijack lives. iOS already routes captive negotiation *outside* any tunnel [P A6, A7]. |
| private ranges | `private_default=corp` | the local subnet never enters the tunnel, because local routes supersede `includedRoutes` [P A6]. Other RFC1918, CGNAT and link-local traffic follows the corp transport, or `direct` when corp is `Off` |
| local mode | escape rules retargeted to `direct` | identical render; the extension runs the canary probe on the physical interface |

### 3.4 DNS

- The TUN hijacks port 53.
- `dns-direct` (DoH to 223.5.5.5 via `direct`) stays the `final`, with
  Cloudflare in local mode.
- New pieces: a `fakeip` server for escape (and `Relay`-corp) domains, and
  the `local` server for hotspot and `System`-corp.
- `Endpoint`-corp names use the endpoint's pushed-resolver DNS server
  [P B8].
- sing-box's Apple client returns no platform `localDNSTransport` [P B1]. So
  whether `local` inside the extension reaches the Wi-Fi's own resolver is
  device test Q7.

### 3.5 The watchdog's jobs

| Job (DESIGN.md §10, §11) | iPhone |
|---|---|
| network change → reload (WatchPaths, `iface_moved`) | gone: libbox follows `NWPathMonitor` [P B1]. The FSM still stamps `last_net_change` for the captive heuristics |
| tunnel health streak → `Recover` (cooldown, verify by re-probe) | **reused**: urltest delay via the command server; the same streak, cooldown and `hold_for_network`; recovery = libbox `startOrReloadService` |
| crashed router → recover if intent is up and it is the same boot | expected to be the OS's job: an on-demand `Connect` rule should restart a jetsam-killed extension [I; device test]. *Intent* = `isOnDemandEnabled`. Surface repeated OOM kills in the UI |
| stale system proxy after reboot | gone: tunnel settings die with the extension |
| captive FSM: drop proxy, open login page, restore | **shrinks**. iOS excludes captive negotiation from every tunnel [P A6, A7]. Keep detection (probe on the physical interface) to *hold recovery* and to show "Wi-Fi needs sign-in". `OpenPortal` becomes a local notification with the portal URL; libbox's platform interface already posts notifications from the extension [P B1]. Drop `CaptiveProxyOff`/`On` |
| `corp sync` (DHCP search domains, VPN route CIDRs) | **not portable**. An iOS app can read neither another VPN's routes nor DHCP search domains [I]. Corp entries come **from the Mac** instead (§5.4) |
| discovery journal | optional: `NWPath` (interface type, gateways, expensive/constrained) and SSID via `NEHotspotNetwork.fetchCurrent`, which apps with a VPN configuration may call [P A17] |
| audit log | keep it (App Group file); export it from Settings › Diagnostics |

On-demand rules can also match SSID, DNS search domain, DNS server and
interface type [P A16]. "Disconnect on the office Wi-Fi" is therefore a
config choice, not code.

### 3.6 Servers, subscriptions, auto selection

- **Import.** Paste, scan a QR code (VisionKit `DataScannerViewController`),
  share sheet, or a `rowt://` URL scheme for `vless:`, `anytls:`, `hy2:` and
  `vmess:` links. These run through `rowt-core::sharelink::parse_many`, and
  subscriptions through `decode_subscription`.
  - A refresh runs when the app opens, plus a best-effort
    `BGAppRefreshTask`.
  - iOS apps cannot read other apps' containers, so the macOS "import from
    Shadowrocket/Clash Verge" scanners do not port. What does port is the
    parsing of **user-exported files**: a Surge-style `.conf`, a Clash YAML, a
    sing-box JSON, or `rowt config export` bundles from the Mac.
  - The Mac bundle is the best path, because it also brings the lanes.
- **Auto selection.** The same `render::group()`: urltest `auto` behind
  selector `escape`. The toggle maps to live `selectOutbound` calls with no
  reload:
  - on → select `auto`;
  - off → pin whatever `auto` currently resolves to, the monitor's behavior
    as of today.
  - The chosen tag persists as `selected` for the next render.

### 3.7 Corp transports

| Transport | When | Render | Risk |
|---|---|---|---|
| `System` | corp Wi-Fi in the office; corp VPN is a Personal VPN (check 1); macOS | `direct`, unbound, `local` DNS; corp CIDRs in `route_exclude_address` when another tunnel owns them | none new; a device test for the Personal-VPN case (Q5, Q8) |
| `Endpoint` | corp VPN is AnyConnect, GlobalProtect, Fortinet, F5, Pulse, Juniper NC, OpenVPN, WireGuard or Tailscale | `endpoints: [openconnect …]`, corp rules → endpoint, endpoint DNS | policy (non-official client), posture checks, memory |
| `Relay` | a trusted machine has corp access (the Mac) | `endpoints: [tailscale]` + a `socks` outbound to `rowt-mac:17890`; corp names are not resolved locally | **policy**; the Mac must be up; memory |
| `Off` | none of the above | corp rules → `block`, with a UI explanation | the user flips to the corp client when needed |

**The binding catch for `System`.** With `auto_detect_interface` on, sing-box
binds every outbound that lacks its own `bind_interface` to the default NIC.
A per-outbound `bind_interface` overrides that, but there is no per-outbound
"stay unbound" [P B12]. sing-box's own docs disagree on scope: the route page
says `auto_detect_interface` is *"Only supported on Linux, Windows and
macOS"*, while `network_strategy` requires it on Apple graphical clients
[P B12]. `System` needs one of two fixes:
- the macOS scheme: auto-detect off, an explicit `bind_interface` on escape
  and direct, and a re-render when the interface changes (the netcheck
  `iface_moved` path comes back);
- proof that the extension's unbound sockets already stay out of its own
  tunnel.

Device test Q8 decides which.

### 3.8 The relay spike (option g), about one day

1. Run sing-box ≥1.15-alpha on the Mac or a home box with an `http` inbound,
   `version: [2, 3]`, TLS and a user. Point a test app's `NERelayManager` at
   it. Set `matchDomains` to the escape lane (suffixes plus a flattened
   `geosite:google`).
2. With the corp client **disconnected**, measure:
   - which apps route through it (Safari, a URLSession app, a WebKit app, a
     game or other BSD-socket app);
   - cellular versus Wi-Fi;
   - how saving behaves (the system validates the relay [P F11]).
3. **Connect the corp client** and repeat. Does the relay stay in force? Does
   the relay connection leave on the physical interface or ride corp? Test
   split and full tunnel.
4. If step 3 passes, the "corp client on" mode exists. rowt-core then gets a
   `relay_config()` target (§4), and the iPhone app gets a second mode (§5.6).

### 3.9 Distribution

- **Own devices: the likely case.**
  - Needs a paid Apple Developer Program membership. An individual account
    is fine. The *Network Extensions* and *Personal VPN* capabilities are not
    available to free accounts [P A19].
  - Development-signed builds and internal TestFlight need no App Review
    [I].
  - GPL obligations are not triggered until the app is distributed [I].
- **App Store.**
  - Guideline 5.4: *"Apps offering VPN services must utilize the
    NEVPNManager API and may only be offered by developers enrolled as an
    organization."*
  - Also 5.4: *"if you choose to make your VPN app available in a territory
    that requires a VPN license, you must provide your license information in
    the App Review Notes field"* [P A18].
  - Footnote: a summarizing fetcher returned a different, wrong 5.4 text.
    The quote above is from the raw page, "Last Updated: June 8, 2026".
- **China storefront.**
  - In 2017, Apple removed VPN apps that "do not meet the new regulations"
    [S X2].
  - sing-box's own client lists *"An Apple account outside of mainland
    China"* as a requirement [P B6].
  - rowt's users would install the way they install Shadowrocket today: with
    a non-China Apple account [I].
- **GPL.**
  - libbox is GPL-3.0-or-later, plus a clause: *"no derivative work may use
    the name or imply association with this application without prior
    consent"* [P B10].
  - An app linking libbox ships as GPLv3. rowt's MIT code combines fine, but
    the binary is GPL [I].
  - App Store terms versus GPL "no further restrictions" is an old and
    unlitigated conflict (FSF on GPLv2 §6, 2010; GPLv3 §10 is the
    analogue) [S X3].
  - The practical risk is a copyright-holder complaint. SagerNet ships its
    own client there as the copyright holder.
- **Availability is volatile.**
  - sing-box's client docs still say *"We are temporarily unable to update
    sing-box apps on the App Store because the reviewer mistakenly found that
    we violated the rules"* [P B6].
  - The client returned as *sing-box MT* under a new developer account in
    1.14.0 [P B5].

---

## 4. Core reuse plan: changes to this repo, ranked by value

`rowt-core` is already the right shape: pure functions, platform facts
passed in (PORTING.md §4). Seven changes follow, ranked. Rows 1–5 are
prerequisites for an iPhone build. Row 6 serves the (f1) stopgap, and row 7
is a nice-to-have.

| # | Change | Where | Why it pays beyond iOS |
|---|---|---|---|
| 1 | **Render from an in-memory model.** New `rowt-core::model` holds `Model {lanes, servers, selected, mode, settings}`; `fn host_input(&Model, &Facts) -> HostInput`. `Settings` carries port, DNS, `final`, `private_default`, `private_cidrs`, the auto interval and the log level. `Facts` carries iface, clash and secret. | `crates/rowt-cli/src/lifecycle.rs::build_host` and `crates/rowt-core/src/bin/rowt-render.rs` both assemble `HostInput` from files and env today, with the literal `listen: "127.0.0.1"` and the private-CIDR list **duplicated**. | One assembly instead of two; `render-matrix` proves it byte-identical. The on-disk format stays (`Model::load_dir` lives in the CLI). |
| 2 | **Render targets.** Split `render_host` into shared rule and DNS builders plus wrappers: `Inbound::Mixed{listen,port}` (today) or `Inbound::Tun{address, mtu, route_exclude, fakeip}`; a `CorpTransport` enum (§3.7); `iface: Option` (None = auto-detect); hotspot as a rule for TUN targets. From the same `Lists`: `pac()` for (e)/(f1), `relay_config()` for (g), and `bypass_entries()` (exists). | `crates/rowt-core/src/render.rs` hardcodes the mixed inbound, `auto_detect_interface: false`, and `bind_interface` on `direct`. | **The TUN target is PORTING.md Phase 5's step 3.** Linux and iOS share it: `route_exclude_address` fed by `corp_sync`, `strict_route: false`. Do it once. |
| 3 | **An FFI crate, `crates/rowt-ffi`** (UniFFI proc-macros → XCFramework for `aarch64-apple-ios` and the simulator). It exposes `render(model_json, target_json)`, `classify(model, dest, resolved_ip?)`, `lanes::apply`, `sharelink::parse_many` / `decode_subscription`, `importmerge::merge`, `watch::{guard, netcheck}`, and `geosite::candidates`. JSON strings cross the boundary wherever `serde_json::Value` is involved. | new crate; `Cargo.toml` workspace member | The same façade could later back a macOS menu-bar app (DESIGN.md §12). |
| 4 | **Purity gates.** A `host-tools` default feature fences off every process spawn and `$HOME` walk; a `Fetcher` trait (bytes in) replaces `curl`; Clash YAML moves to a Rust YAML parser instead of `yq` (gated by `foreign-diff`); V2Box's `rusqlite` moves behind its own feature. | `geosite.rs:184,219` (`sing-box rule-set decompile`, `curl`); `foreignio.rs:108` (`yq`); `pycli/net_detect.rs:25` (`scutil`); `pycli/vless_parse.rs:84` (`curl`); `srio.rs` (Shadowrocket container paths); `pycli/*` (stdout/exit CLI surfaces) and the gate bins | macOS loses the `yq` runtime dependency; the core compiles for any target. |
| 5 | **Split the watchdog FSM.** Separate `recovery` (streak, cooldown, `hold_for_network`, captive-hold: shared) from `proxy_plane` (`proxy_pointing_ok`, `proxy_bypass_ok`, `active_service`, `CaptiveProxyOff/On`, `ClearStaleProxy`: macOS). The iOS `Observation` is {intent, running, captive, gateway_ok, net_id, health_ok, now, mode}. | `crates/rowt-core/src/watch.rs` (`Observation`, `Action`) | `watch-diff` / `watch-shadow` keep holding macOS; Linux TUN needs the same split, because its "drop the proxy" becomes "pause the tun route" (PORTING.md §4.1). |
| 6 | **LAN listen + auth** for (f1): `ROWT_LISTEN`, `users` on the mixed inbound, and a served PAC. | `lifecycle.rs`, `rowt-render.rs`, `render.rs` | Any second device (Android, a tablet, a TV) gets rowt with no app. |
| 7 | **A metrics library.** The collector's diff-into-buckets and tier consolidation become a `rowt-metrics` crate fed by libbox connection updates. | `rowt-monitor/src/metrics.rs`, `src/bin/rowt-collector.rs` (separate workspace; Phase 4 already plans the merge) | Lower priority: history on the phone is a nice-to-have. |

**Shareable as-is:** `classify`, `lanes` (the single-lane invariant, the
corp-region insertion rule), `sharelink`, `importmerge`, `hotspot`,
`reconcile`, `geosite::candidates`, `laneerr` (if fed libbox's log lines),
and the `py*` semantics helpers.

**Not shareable:**
- `bin/rowt`;
- `rowt-platform`'s `Mac` impl (networksetup, launchctl, scutil, ipconfig,
  dig, netstat, ping, curl), where iOS builds its `Observation` in Swift;
- the log splitter (libbox streams logs over the command server);
- `vm.rs` and Lima;
- `corp_sync`'s inputs;
- launchd and sudoers;
- shell-init and the remote helpers;
- the ratatui TUI and its goldens.

**Gates.** The new TUN and relay targets need renders held in goldens plus
`sing-box check`, in the spirit of PORTING.md §6.3. The iOS paths cannot run
under the parity harness's shims, so they need device drills (§6.3). Pin
libbox in lockstep with `SINGBOX_VERSION`. The versions matter:
1.13.14 today; ≥1.14 for OpenConnect/OpenVPN; ≥1.15 for a sing-box relay
server.

**The ≥1.14 options collide with the pin.** rowt refuses every 1.14.x as
known-bad (`sb_known_bad` in `bin/rowt`). Its DNS transport's shared UDP
receive loop treats a 0-byte read as a malformed datagram and retries with no
backoff. Once something kills the idle socket under it, which a macOS EDR
network filter does 10 s after a reply, sing-box spins at 100%+ CPU forever
while every probe still answers. 1.13.x is immune. (b1)'s endpoints and a
sing-box relay server for (g) therefore wait on an upstream fix, verified the
way the bug was found, before `sb_known_bad` can narrow. Whether anything on
iOS kills that socket is unknown [I]. A spinning extension would drain the
battery whatever the trigger, so it has to be fixed, not just avoided.
(b2)'s Tailscale endpoint (since 1.12) is available on the current pin.

---

## 5. UX

### 5.1 Principles

1. **A phone app, not a TUI port.** One primary action (Connect) and three
   things people edit: sites, servers, and occasionally settings. rowt
   *explains* routes but never makes you type commands.
2. **Say what iOS allows.** The one-VPN rule is the single most confusing
   fact for users. When the corp client takes over, the app says so plainly
   instead of showing a dead toggle.
3. **Fail closed, visibly.** Tunnel sites that cannot load say why and offer
   *Switch server*. rowt never silently sends them direct (DESIGN.md §10).
4. **Automatic by default.** Reload, restart, watchdog and captive
   handling are invisible unless they need the user.

### 5.2 Vocabulary on a phone

| rowt (macOS) | iPhone | Note |
|---|---|---|
| escape lane | **Tunnel** | "sites that go through your server" |
| corp lane | **Work** | hidden entirely when the corp transport is `Off` and no Work rules exist |
| direct | **Direct** | "everything else" |
| block | **Block** | |
| hotspot lane | **Wi-Fi sign-in pages** | Settings › Advanced |
| lane / list | **route** / **rules** | |
| `geosite:<name>` | **Service**, e.g. "Google — all domains" | searchable catalog with domain counts |
| `use auto` / urltest | **Automatic (fastest)** | |
| mode `host` / `vm` | hidden | vm does not exist on iOS |
| mode `local` | **Abroad mode** | auto by default |
| `up` / `down` | **Connect** toggle | |
| reload, restart, watchdog, system proxy, clash API | hidden | no user-facing meaning on iPhone |
| `explain` | **Test a site** | |
| `corp sync` | **Sync Work sites from Mac** | |
| `report`, audit log | Settings › Diagnostics › Export | |

### 5.3 Navigation and screens

The app uses a tab bar with four single-word tabs: **Status · Rules ·
Servers · Activity** [HIG H1]. Settings opens from a toolbar button on
Status, because a tab bar is for navigation, not actions [H1].

- **Status.** The hero shows state (§5.4) and the current server: "via hk-2 ·
  86 ms · Automatic". Below it, one chip per route (Tunnel / Work / Direct /
  Block) with live throughput. Cards appear only when they need action: a
  server is down, Wi-Fi needs sign-in, or another VPN is active.
- **Rules.** A segmented control switches between Tunnel, Work and Block.
  Direct is the footer: "Everything not listed goes Direct."
  - A search field at the top doubles as **Test a site**: type a domain and
    get its route and the reason (`classify`).
  - Rows show:
    - a website ("example.com · and subdomains");
    - an exact host (a badge);
    - a service (icon plus domain count);
    - an IP range (Work only).
  - Swipe actions: Delete, Move to….
  - "+" opens Add website / Add service / Add IP range.
- **Servers.** The first row is **Automatic** (toggle; shows the current
  pick). Sections follow per subscription (updated time; pull to refresh),
  then Manual servers. Each row shows latency. Tap to pin. Swipe to Test,
  Share QR or Delete. A toolbar button runs Test all.
- **Activity.** A throughput chart split by route (Swift Charts [H9]) sits
  over a segmented control: **Live · Problems · Log**.
  - *Live* groups connections by route (domain, matched rule, server, bytes).
  - Tapping a row offers *Route via Tunnel / Work / Block*. That is the
    README's "diagnose, then tunnel" loop in one tap.
  - *Problems* lists domains that keep failing, categorized by `laneerr`
    (timeout, reset, refused), each with "Try via Tunnel".
- **Settings.**
  - Connect on demand (Always / Off / Not on these Wi-Fi networks);
  - Abroad mode (Auto / Always / Never);
  - Work routing transport;
  - DNS;
  - Default route for unlisted sites;
  - Wi-Fi sign-in pages;
  - Diagnostics;
  - About, with the GPLv3 notice and a source link, required once libbox
    ships.

### 5.4 Key flows

**First run and import.** Follow HIG onboarding: brief, and ask for
permission in context [H2, H8].
1. Welcome: one diagram of the routes.
2. **Add servers.** Choose one: Scan QR, Paste link, Subscription URL,
   Import a file (Shadowrocket/Surge `.conf`, Clash YAML, sing-box JSON), or
   **From my Mac**. The Mac option means AirDropping `rowt config export`,
   registered as a document type; it brings servers *and* rules.
3. Review the servers, with a latency test and remove buttons.
4. **What goes through the Tunnel?** Starter services as toggles (Google,
   YouTube, GitHub, Telegram, …) plus custom sites.
5. **Work sites** appear only if a corp transport is available. The screen
   says plainly which transport is in use and what policy it implies.
6. Pre-permission screen, then the system "Add VPN Configurations" alert.
7. Connect.

**Connect and its states.** `NEVPNStatus` plus rowt's own facts.

| State | Hero text | Action |
|---|---|---|
| Off | "rowt is off" | Connect |
| Connecting / reasserting | "Connecting…" | — |
| On | "Routing · via hk-2 · 86 ms" | — |
| On, tunnel failing | "Tunnel server isn't answering. Tunnel sites won't load (they won't leak)." | Switch server |
| Wi-Fi needs sign-in | "This Wi-Fi wants you to sign in" | Open sign-in page (the portal URL); notification sent once per episode |
| Abroad | "No tunnel needed here · Tunnel sites go direct" | Use tunnel anyway |
| **Another VPN is on** | "Your work VPN is connected. iPhone runs one VPN at a time." | "Turn on rowt": a confirm sheet says *this disconnects your work VPN* [P A1] |

**Auto versus pinned.** Tapping a server pins it and turns Automatic off.
Tapping Automatic re-enables it. The switch is live, with no reload (§3.6).
The Status hero always shows *which* server is actually carrying traffic.

**Lane editing.**
- Adding a site that already lives in another route opens a sheet: "google.com
  is in Block. Move it to Tunnel?" This is the single-lane invariant, via
  `lanes::apply`.
- Adding a plain site that a Service also covers suggests the Service but
  never applies it silently. That is the macOS `escape add` hint, via
  `geosite::candidates`.
- **Wi-Fi sign-in pages** sits under Settings › Advanced. It explains itself:
  "Airline and hotel login sites that must load before you're online."

**Live view.** Covered in §5.3. It exists only in engine mode (a). Relays
(g) and remote rowt (f) give the phone no per-connection visibility.

### 5.5 System surfaces

- **Control Center, Lock Screen and Action button:** one
  `ControlWidgetToggle`, "rowt", with symbols for both states [H3].
  - Turning it *off* affects security, so consider requiring unlock (HIG:
    *"Require authentication for actions that affect security"* [H3]).
  - Redact the server name on a locked device.
- **Widgets.**
  - Small: state, server, latency.
  - Medium: today's traffic by route.
  - Lock Screen accessory: on/off [H4].
- **App Intents and Shortcuts:** Connect, Disconnect, Toggle, Use server
  (parameter), Automatic on/off, Add site to route (parameter), Get status
  [H7]. Network-based automation belongs in on-demand rules (SSID, DNS
  search domain [P A16]), not in Shortcuts automations.
- **Share extension:** from Safari, "Route this site…" → Tunnel / Work /
  Block.
- **Notifications, only when action is needed:**
  - Wi-Fi sign-in (tap opens the portal);
  - tunnel down with no working server;
  - a subscription failed to update.
  Never send repeats for the same episode [H5].
- **No Live Activity.** HIG scopes them to activities *"that don't exceed
  eight hours"* [H6]. The status-bar VPN badge already covers "on".

### 5.6 If the outcome is different

- **No corp transport (the likely default for a proprietary client).** The
  Work tab section reads "Not routed on this iPhone". Rules synced from the
  Mac are kept, not applied. The "Another VPN is on" state is the everyday
  hand-off.
- **Relay-to-Mac (b2).** The Work chip shows "via rowt-mac" with
  reachability. A persistent footnote names the policy trade-off.
- **Relays win the spike (g).** The app gains a second mode, "Keep work
  VPN": no Connect state machine, because a relay is a configuration that
  iOS 26 lets users toggle in Settings (`UIToggleEnabled` [P A15]). Status
  shows relay health. Rules edits recompile `matchDomains`. Activity
  disappears.
- **Only (f1).** The app becomes a remote for the Mac's rowt (status and
  server switching over the LAN) plus a generator for a Wi-Fi
  `.mobileconfig` with the PAC URL [P A13].

---

## 6. Risks and open questions

### 6.1 Decisions only you can make

1. **Corp on iPhone:** off (toggle to the corp client), relay via the Mac
   (b2: policy), or an endpoint (b1: only if the corp VPN is
   OpenConnect-family, OpenVPN, WireGuard or Tailscale).
2. **Distribution:** own devices (individual developer account, no review)
   or App Store (organization account, GPL exposure, no China storefront).
3. **Whether to run the relay spike (§3.8)** before building (a).
4. **Sequencing:** do PORTING.md Phase 5's TUN render first. It is the same
   work, and it has a Linux gate.
5. **Engine licensing:** stay on sing-box (GPL; parity with every existing
   render gate), or accept a rewrite of the render for a permissively
   licensed engine. The latter throws away the parity culture, so it is not
   recommended.

### 6.2 Risks

- **Rule-based split inside a tunnel is TN3120's "unsupported"** [P A4, F8].
  It is tolerated today and could change in any iOS release.
- **Undocumented utun-fd KVC** in libbox's Apple glue [P B1]. iOS could
  break it.
- **50 MiB jetsam** [P B4, F3]. Endpoint transports (b1, b2) and big
  rule-sets compete for the same budget. OOM crash reports exist [S B11].
- **App Store volatility**, shown by sing-box's own history [P B5, B6].
- **GFW:** escape resilience remains the protocols' problem, not rowt's.
  Relays (g) speak standard HTTP CONNECT / MASQUE and must not face the
  border directly [S X5, X6].
- **Corp policy** for (b1) and (b2).
- **Engine versions.** The (b1) endpoints need sing-box ≥1.14 and a
  sing-box relay server for (g) needs ≥1.15, but rowt refuses 1.14.x as
  known-bad: a CPU spin in its UDP DNS loop (§4, Gates).
- **Two runtimes in the extension** (Go, plus Rust for the FSM): binary size,
  and a second allocator in the memory budget. Keep Rust small there.

### 6.3 Device tests: the spike checklist

| # | Question | Decides |
|---|---|---|
| Q1 | Does a Wi-Fi manual proxy or PAC stay in force while the corp client is connected, split and full tunnel? | (e), (f1) with the corp VPN on |
| Q2 | Does an `NERelayManager` relay stay active with the corp client connected, and does the relay connection leave on the physical interface or ride corp? | (g) |
| Q3 | Which apps honor relays: Safari, URLSession, WebKit, Network.framework, BSD-socket apps? | (g) coverage |
| Q4 | Does Apple's relay client interoperate with sing-box 1.15's HTTP/2 and HTTP/3 inbound (CONNECT, CONNECT-UDP)? | the (g) server side |
| Q5 | A Personal VPN (IKEv2) plus an enterprise VPN on one phone: are routes shared as DTS describes [P F1]? | (b3), (f5) |
| Q6 | Memory headroom: libbox + FakeIP + three geosite sets, then plus a Tailscale endpoint, then plus OpenConnect, under real browsing | (b1), (b2) viability |
| Q7 | Inside the extension, does `local` DNS reach the Wi-Fi's DHCP resolver? A portal page opened in Safari while connected is the test | hotspot, `System` corp |
| Q8 | Does an *unbound* socket from the extension stay out of rowt's own tunnel and follow the system routes into a coexisting Personal VPN or office LAN? (See the binding catch in §3.7.) | `System` transport, and whether auto-detect can stay on |
| Q9 | After a jetsam kill of the extension, does an on-demand `Connect` rule bring the tunnel back without the user? | the watchdog's crash-recovery job (§3.5) |

---

## 7. Sources

Apple documentation pages were read as raw DocC JSON
(`developer.apple.com/tutorials/data/…`). Two pages were read as raw HTML
after a summarizing fetcher returned **wrong** content for them: the App
Review Guidelines (a fabricated 5.4 text) and the capabilities table
(claimed free accounts get Network Extensions). Forum answers were read
through that summarizing fetcher and are marked as such. Their staff
attribution and substance agree with the primary docs they sit beside
(F1 ↔ A1; F3 ↔ B4).

**Apple documentation [P]**
- A1 NETunnelProviderManager, "Configuration Model" — https://developer.apple.com/documentation/networkextension/netunnelprovidermanager
- A2 Personal VPN — https://developer.apple.com/documentation/networkextension/personal-vpn · NEVPNManager.isEnabled — https://developer.apple.com/documentation/networkextension/nevpnmanager/isenabled
- A4 TN3120 Expected use cases for packet tunnel providers — https://developer.apple.com/documentation/technotes/tn3120-expected-use-cases-for-network-extension-packet-tunnel-providers
- A5 TN3134 Network Extension provider deployment — https://developer.apple.com/documentation/technotes/tn3134-network-extension-provider-deployment
- A6 Routing your VPN network traffic — https://developer.apple.com/documentation/networkextension/routing-your-vpn-network-traffic
- A7 includeAllNetworks — https://developer.apple.com/documentation/networkextension/nevpnprotocol/includeallnetworks · excludeLocalNetworks (default `true` on iOS) — https://developer.apple.com/documentation/networkextension/nevpnprotocol/excludelocalnetworks
- A9 NEProxySettings — https://developer.apple.com/documentation/networkextension/neproxysettings
- A10 NERelayManager — https://developer.apple.com/documentation/networkextension/nerelaymanager · NERelay — https://developer.apple.com/documentation/networkextension/nerelay
- A11 Network Extensions entitlement (values incl. `relay`) — https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.developer.networking.networkextension
- A12 GlobalHTTPProxy payload ("Requires supervision: iOS") — https://developer.apple.com/documentation/devicemanagement/globalhttpproxy
- A13 WiFi payload (`ProxyType` None/Manual/Auto, no supervision) — https://developer.apple.com/documentation/devicemanagement/wifi
- A14 Cellular payload / APNsItem (`ProxyServer`, `ProxyPort`) — https://developer.apple.com/documentation/devicemanagement/cellular/apnsitem
- A15 Relay payload (match/excluded domains, `UIToggleEnabled`, iOS 27 deprecations) — https://developer.apple.com/documentation/devicemanagement/relay
- A16 NEOnDemandRule — https://developer.apple.com/documentation/networkextension/neondemandrule
- A17 NEHotspotNetwork.fetchCurrent — https://developer.apple.com/documentation/networkextension/nehotspotnetwork/fetchcurrent(completionhandler:)
- A18 App Review Guidelines (2.5.4, 5, 5.4; last updated June 8, 2026) — https://developer.apple.com/app-store/review/guidelines/
- A19 Supported capabilities (iOS) — https://developer.apple.com/help/account/reference/supported-capabilities-ios
- A20 TN2277 Networking and Multitasking — https://developer.apple.com/library/archive/technotes/tn2277/_index.html
- A22 Packet tunnel provider (the iCloud Private Relay note) — https://developer.apple.com/documentation/networkextension/packet-tunnel-provider
- A23 URL filters (iOS 26; WebKit and URLSession only) — https://developer.apple.com/documentation/networkextension/url-filters
- Apple Platform Deployment: VPN overview — https://support.apple.com/guide/deployment/vpn-overview-depae3d361d0/web · network relays — https://support.apple.com/guide/deployment/use-network-relays-dep91a6e427d/web
- APIs for §5: ControlWidget — https://developer.apple.com/documentation/swiftui/controlwidget · ControlWidgetToggle — https://developer.apple.com/documentation/widgetkit/controlwidgettoggle · App Intents — https://developer.apple.com/documentation/appintents · DataScannerViewController — https://developer.apple.com/documentation/visionkit/datascannerviewcontroller · NWPathMonitor — https://developer.apple.com/documentation/network/nwpathmonitor

**Human Interface Guidelines [P]**
- H1 Tab bars — https://developer.apple.com/design/human-interface-guidelines/tab-bars
- H2 Onboarding — https://developer.apple.com/design/human-interface-guidelines/onboarding
- H3 Controls — https://developer.apple.com/design/human-interface-guidelines/controls
- H4 Widgets — https://developer.apple.com/design/human-interface-guidelines/widgets
- H5 Notifications — https://developer.apple.com/design/human-interface-guidelines/notifications
- H6 Live Activities — https://developer.apple.com/design/human-interface-guidelines/live-activities
- H7 App Shortcuts — https://developer.apple.com/design/human-interface-guidelines/app-shortcuts
- H8 Privacy (requesting permission) — https://developer.apple.com/design/human-interface-guidelines/privacy
- H9 Charts — https://developer.apple.com/design/human-interface-guidelines/charts

**Apple staff on the developer forums [P, read via summarizer]**
- F1 Matt Eaton (DTS), Mar 2021: one packet tunnel at a time; Personal VPN rules — https://developer.apple.com/forums/thread/675507
- F2 Quinn, Sep 2015: two tunnel providers cannot be active — https://developer.apple.com/forums/thread/17019
- F3 Quinn: iOS 16 limits (packet tunnel 50 MiB, app proxy 15, DNS proxy 15); Oct 2025: unchanged on iOS 26 — https://developer.apple.com/forums/thread/73148?page=2
- F4 Quinn, 2018: the limit applies to the whole process and is undocumented — https://developer.apple.com/forums/thread/106377
- F5 Quinn, "iOS Background Execution Limits" — https://developer.apple.com/forums/thread/685525
- F6 Quinn, Sep 2017: a proxy server on iOS; app proxy is managed-only — https://developer.apple.com/forums/thread/86110
- F7 Quinn, Sep 2017: "NSURLSession takes care of proxies for you" — https://developer.apple.com/forums/thread/86364
- F8 Quinn, Apr 2026: non-VPN packet tunnels are brittle and unsupported — https://developer.apple.com/forums/thread/821306
- F9 DTS, Jan 2025: relay `matchDomains` accepts CIDR — https://developer.apple.com/forums/thread/773199
- F10 Quinn, Jan 2025: no fixed relay limits; no two relays for the same domain — https://developer.apple.com/forums/thread/772853
- F11 Quinn, Mar 2025: an iOS relay save validates the server — https://developer.apple.com/forums/thread/776419

**WWDC [P]**
- W1 WWDC23 10002 "Ready, set, relay" — https://developer.apple.com/videos/play/wwdc2023/10002/
- W2 WWDC25 234 "Filter and tunnel network traffic with NetworkExtension" — https://developer.apple.com/videos/play/wwdc2025/234/

**sing-box [P]**
- B1 sing-box-for-apple `ExtensionPlatformInterface.swift` (openTun, KVC fd, NWPathMonitor, notifications, `localDNSTransport` = nil) — https://github.com/SagerNet/sing-box-for-apple/blob/dev/Library/Network/ExtensionPlatformInterface.swift
- B2 `ExtensionProvider.swift` / `ExtensionProfile.swift` (command server, `oomMemoryLimitMB`, on-demand) — https://github.com/SagerNet/sing-box-for-apple/tree/dev/Library/Network · entitlements — https://github.com/SagerNet/sing-box-for-apple/blob/dev/Extension/Extension.entitlements
- B4 `service/oomkiller/policy.go` (50 MiB) and `experimental/libbox/setup.go` — https://github.com/SagerNet/sing-box/blob/testing/service/oomkiller/policy.go
- B5 Changelog (1.14.0: App Store return, OpenConnect, OpenVPN; 1.15-alpha: MASQUE, HTTP/2/3 + CONNECT-UDP, new TUN stack) — https://github.com/SagerNet/sing-box/blob/testing/docs/changelog.md
- B6 Apple client docs (requirements, App Store notice, TUN option support) — https://sing-box.sagernet.org/clients/apple/ · https://sing-box.sagernet.org/clients/apple/features/
- B7 TUN inbound (`platform.http_proxy`) — https://sing-box.sagernet.org/configuration/inbound/tun/
- B8 Endpoints: OpenConnect — https://sing-box.sagernet.org/configuration/endpoint/openconnect/ · OpenVPN client — https://sing-box.sagernet.org/configuration/endpoint/openvpn-client/ · Tailscale — https://sing-box.sagernet.org/configuration/endpoint/tailscale/
- B9 HTTP inbound (`version` 1/2/3, since 1.15) — https://sing-box.sagernet.org/configuration/inbound/http/
- B10 LICENSE (GPL-3.0-or-later plus the naming clause) — https://github.com/SagerNet/sing-box/blob/testing/LICENSE
- B11 [S] iOS OOM reports — https://github.com/SagerNet/sing-box/issues/2857 · https://github.com/SagerNet/sing-box/issues/3976
- B12 Route (`auto_detect_interface`: *"Takes no effect if `outbound.bind_interface` is set"*) — https://sing-box.sagernet.org/configuration/route/ · Dial fields (`bind_interface`, `network_strategy`: Apple/Android graphical clients only) — https://sing-box.sagernet.org/configuration/shared/dial/

**Other [S]**
- X1 Tailscale KB, other VPNs ("iOS and Android enforce a limit of running only one VPN at a time") — https://tailscale.com/kb/1105/other-vpns
- X2 TechCrunch, 2017-07-29, Apple removes VPN apps from the China App Store — https://techcrunch.com/2017/07/29/apple-removes-vpn-apps-from-the-app-store-in-china/
- X3 FSF, 2010-05-25, App Store and the GPL — https://www.fsf.org/news/2010-05-app-store-compliance
- X4 Jedda, "Beneath the MASQUE" (relays next to WireGuard and Tailscale; Envoy as a relay server) — https://jedda.me/beneath-the-masque-network-relay-on-apple-platforms/
- X5 USENIX Security 2025, SNI-based QUIC censorship by the GFW — https://gfw.report/publications/usenixsecurity25/en/
- X6 USENIX Security 2024, fingerprinting encapsulated TLS handshakes — https://www.usenix.org/conference/usenixsecurity24/presentation/xue-fingerprinting
- X7 netbird#1627 (users folding a WireGuard VPN into Surge because of the one-VPN rule) — https://github.com/netbirdio/netbird/issues/1627
- X8 Surge Knowledge Base, common FAQs (a "Surge Mac and VPN together" section; nothing for iOS) — https://kb.nssurge.com/surge-knowledge-base/faq/common-faqs

**This repo:** DESIGN.md §2–§4, §8, §10–§11 · PORTING.md §4.1, §5 (Phase 5), §6.3 · `crates/rowt-core/src/{render,watch,geosite,foreignio,srio}.rs` · `crates/rowt-cli/src/lifecycle.rs` · `crates/rowt-core/src/bin/rowt-render.rs` · `crates/rowt-platform/src/lib.rs` · `bin/rowt` (shell-init: `rowt-share-on`, `rowt-remote-on`) · `rowt-monitor/src/{metrics.rs,source/live.rs}`.
