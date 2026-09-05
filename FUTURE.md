# FUTURE — intelligent routing management for rowt

> Status: **design sketch, not built.** This captures the target design for an
> agent that senses the network and manages routing automatically, so the intent
> survives across sessions. It supersedes the loose "transparent mode" / "full
> mode" TODOs in [DESIGN.md §9](DESIGN.md) — those modes are *mechanisms* this
> tool would drive, not features on their own.

## 1. Vision

Today you choose rowt's posture by hand: which mode (`host`/`vm`), which server,
whether the proxy is on, and — if the ideas below land — whether to use system
proxy vs. transparent capture vs. a full tunnel. The right choice is a
deterministic function of the environment you're in: *is corp VPN up, what
network am I on, is `bind_interface` filtered, is escape reachable.*

The goal: **sense the environment and set the correct routing posture
automatically**, while never violating the invariants rowt already lives by
(never leave the system in a limbo state; everything reversible; effects don't
outlive a reboot; coexist with the corp VPN; fail closed).

This is the policy layer above the routing engine. rowt already has the seeds:
`probe` (senses whether `bind_interface` is filtered → host vs vm), `watch`
(reacts to network changes), `use auto` (picks a server by latency), `explain`
(reasons per-destination). The tool wires these together, broadens the sensors,
and adds a policy engine + an optional LLM-assist layer.

## 2. Architecture — two layers, LLM kept OUT of the reflex path

The single most important design decision:

**Layer 1 — deterministic policy engine (the reflex).** Sense → match a profile
→ apply state. A state machine / rules table, **not** a model. It must be:

- **fast** — fires on every network flap without lag,
- **deterministic** — same environment ⇒ same posture, testable,
- **offline-capable** — works with zero network (the plane case), so no cloud
  dependency on the hot path,
- **safe** — it also manages pf anchors; a nondeterministic actor here can wedge
  all networking. Networking reflexes must be boring.

**Layer 2 — LLM assist (the judgment), strictly off the hot path.** Where a
Claude-style agent genuinely earns its place — fuzzy, one-time, human-in-the-loop:

- "I'm on a network I haven't seen — here's what I sensed, propose a profile."
- "This site went the wrong way — explain why and suggest a lane rule" (natural
  language over `explain`, then generate a `corp-domains`/`escape-domains` entry).
- "Diagnose why escape is failing on this network."
- Learn usage patterns over time and *suggest* profile changes you approve.

Advisory, confirmable, never in the reflex loop. Reflexes for speed and safety;
the model for intent and explanation.

## 3. Coverage mechanisms (the actions the engine chooses between)

Each is a way to get app traffic into rowt's `127.0.0.1:7890` router. They differ
in coverage, intrusiveness, and — decisively — whether they coexist with corp.

| mechanism | how | coexists with corp? | covers | cost / risk |
|-----------|-----|---------------------|--------|-------------|
| **system proxy** (today) | `networksetup` per-service proxy | ✅ yes (no route/firewall change) | proxy-aware apps (browsers, GUI) | lowest; trivially reversible |
| **proxy env** (today) | `eval "$(rowt proxy env)"` per shell | ✅ yes | opt-in CLI tools | manual, per-shell |
| **transparent / redirect** | pf `rdr` → sing-box `redirect` inbound (recovers original dst via `DIOCNATLOOK`) | ✅ yes (firewall rewrite, not a route) | **all TCP, automatically** | pf anchor = higher blast radius; EDR/pf conflicts; TCP-only |
| **full / TUN** | sing-box `tun` inbound | ❌ no (claims default route — fights corp) | **everything incl. UDP** | corp-off only; sole-tunnel reliability bar |

Escape delivery (`host` vs `vm`) is an orthogonal axis the engine also picks,
driven by `probe` (is `bind_interface` filtered).

The coverage ladder: system proxy → transparent (corp-on ceiling) → full TUN
(corp-off only). Note the transparent rung is the highest coverage attainable
**while corp is on**, which is rowt's whole reason to exist.

### 3a. How transparent/redirect works (recap)

A proxy-oblivious app connects straight to the real destination IP. Two pieces
capture it without its cooperation:

1. **Intercept** — a pf `rdr` rule rewrites the outbound TCP destination to a
   local listener (`127.0.0.1:PORT`); the app is none the wiser.
2. **Recover original destination** — the listener asks the kernel pf device via
   the `DIOCNATLOOK` ioctl "where was this headed before you rewrote it?" and gets
   the real `ip:port` back. (Linux equivalent: `SO_ORIGINAL_DST` / TPROXY.) It
   then forwards to the upstream, and rowt's router applies the lanes as usual.

Three gotchas the engine must handle:

- **Domain recovery.** The app resolved DNS itself, so the listener sees an *IP*,
  not a name. sing-box **sniffs** the TLS SNI / HTTP Host to recover the domain so
  rowt's domain-based rules still fire.
- **Loop trap.** The proxy's own outbound sockets match the same `rdr` rule →
  infinite loop. Must exclude by the sing-box uid, plus loopback, LAN, and corp
  CIDRs.
- **TCP-only.** `rdr`/`DIOCNATLOOK` is TCP NAT; UDP (QUIC/HTTP3, games, voice)
  leaks direct unless a separate tproxy path is added. Only the TUN mode closes
  that.

## 4. When to prefer system proxy over transparent (policy input)

Transparent capture is greedy and touches the firewall; system proxy is narrow
and boring. Prefer **system proxy** when:

1. **On the corp-managed machine, whenever possible.** Loading your own pf anchors
   next to a corporate EDR + VPN client is contested territory: EDR may flag
   firewall changes, corp's pf reload can flush your anchor, MDM may forbid it.
   `networksetup` never touches the firewall.
2. **Limbo bar matters most.** A bad proxy setting breaks only proxy-aware apps
   and reverts in one call; a bad pf anchor can wedge all networking.
3. **Only browsers/GUI apps need coverage** — they already honor system proxy;
   transparent buys nothing.
4. **You want opt-in, legible control** — "this shell proxied, that one not,"
   rather than a greedy net + exclusion rules.
5. **Fast bail-out** — captive portal / plane / hotel Wi-Fi is one toggle, no
   anchor to flush.
6. **Lower maintenance** — `networksetup` is a stable API; local-traffic pf +
   `DIOCNATLOOK` is fiddly and shifts across macOS versions.

Prefer **transparent** only off the corp machine (or corp-off), on a trusted
network, when you have many proxy-oblivious CLI tools to capture automatically.

⇒ This falls out as a policy rule, not a global setting:
**corp-on / managed machine → system proxy; corp-off / trusted → transparent.**

## 5. Sensing → deciding

| sense (signals) | source | decide (policy) |
|-----------------|--------|-----------------|
| corp VPN up? (utun + corp routes/DNS present) | routing table, resolver | **gate**: TUN allowed only corp-off; managed-machine → prefer system proxy |
| which network? (SSID / gateway MAC / ethernet / tether) | `networksetup`, ARP | trust class: home / known-office / public |
| `bind_interface` filtered? | `probe` | escape via `host` vs `vm` |
| escape reachable / fastest server? | latency probe | server selection, or disable escape (fail closed) |
| captive portal present? | probe a known endpoint | pause proxy so you can log in |
| corp pf anchors present? | `pfctl -s all` | whether transparent is safe here at all |

→ outputs: **coverage mode** (proxy / transparent / full), **escape delivery**
(host / vm), **server**, **DNS strategy**, per-lane **fail-closed vs open**.

## 6. Policy model — declarative profiles

A profile is a match → desired-state rule. Hand-authored first; later the LLM
layer *suggests* profiles from observed behavior, you approve, it's written to
config. Everything inspectable and reversible.

```
# illustrative shape, not final syntax
profile "corp-office" {
  match  { corp_vpn = up;  ssid = "CorpOffice*" }
  set    { coverage = system_proxy;  escape = host;  server = auto;  final = direct }
}
profile "home-personal" {
  match  { corp_vpn = down; gateway_mac = "aa:bb:cc:*" }
  set    { coverage = transparent;  escape = host;  server = auto }
}
profile "untrusted-wifi" {
  match  { corp_vpn = down; trust = public }
  set    { coverage = full;  escape = vm;  server = auto;  killswitch = on }
}
profile "default" {
  match  { any }
  set    { coverage = system_proxy;  escape = probe;  server = auto }
}
```

## 7. Safety invariants (non-negotiable)

- **Atomic + reversible + fail-closed** on every transition. The agent must never
  wedge networking; pf makes this stricter than the proxy-only case today.
- **Bulletproof teardown** — `down` flushes any pf anchor and clears system proxy
  even offline (extends the existing "never leave limbo" work; a stuck pf anchor
  is worse than a stuck proxy setting).
- **Dry-run / explain-before-apply** — "on this network I *would* switch to
  transparent + server-X because Y." Surfaces through `explain`, the planned TUI
  monitor, and the desktop widget. The agent shows its reasoning; it never acts
  invisibly.
- **Confirmation model** — auto for safe transitions (proxy on/off, server
  switch); **confirm** for intrusive ones (enabling pf/transparent, TUN).
- **Offline-first** — the reflex path never depends on a cloud model.
- **Compliance** — on the managed machine, default to the least-intrusive
  mechanism that works; never silently modify the firewall where EDR/MDM would
  object. Intrusive modes are opt-in and clearly flagged.

## 8. What this consolidates (and what it doesn't)

- **Shadowrocket → absorbed** as the `full`/TUN mode (corp-off, same VLESS
  servers, delivered as a tunnel). Covers UDP; retires the separate app.
- **transocks → absorbed** as the `transparent` mode. Its job (transparent TCP →
  SOCKS) is a native sing-box `redirect` inbound, so no third binary — rowt just
  adds the inbound + manages a pf anchor. Retires transocks *and* the manual
  `proxy env` dance.
- **ExpressVPN → stays out.** A closed commercial client (Lightway/OpenVPN) with a
  multi-region fleet and streaming-optimized IPs. rowt can't ingest it, and
  self-hosting its use cases (one VPS exit, easily flagged for streaming) is a
  downgrade. Different problem.

## 9. Relationship to other planned surfaces

- **`explain`** (built) becomes the agent's per-destination reasoning primitive
  and the natural home for "why did this go there / what rule would fix it."
- **TUI monitor** and **desktop widget** ([DESIGN.md §9](DESIGN.md)) are where the
  agent *shows its reasoning* and offers one-click overrides — the dry-run/explain
  invariant needs a surface.
- **`probe` / `watch` / `use auto`** are existing sensors/actuators the reflex
  engine orchestrates rather than reinvents.

## 10. Open design questions

- **Profile definition & learning.** Config schema; how the LLM proposes profiles
  from observed behavior; approval + write-back flow.
- **Trust classification.** How to reliably tell home vs known-office vs public
  (SSID spoofable; gateway MAC better; ethernet/tether signals).
- **pf coexistence on a managed laptop.** Probe corp's anchors first; decide
  whether transparent is ever enabled there, or gated to corp-off only.
- **Confirmation UX.** What's auto vs. prompted, and how prompts reach you
  (notification, TUI, widget) without being annoying.
- **Kill-switch / DNS-leak semantics** for the TUN mode if rowt ever becomes a
  sole tunnel on hostile networks — a real reliability bar, not a checkbox.
- **State model & observability.** Where the current posture + "why" is recorded
  so the monitor/widget/`explain` can all read it.
- **Packaging.** Reflex engine as a rowt subcommand + the existing `watch`
  LaunchAgent, vs. a small daemon; where the LLM layer runs (local CLI you invoke
  vs. background).

- **Slow-lane policy — settled: warn, never refuse.** A lane that is reachable but
  far slower than the others (the corp tunnel's ~200 KB/s in
  [BUG-corp-lane-throughput.md](BUG-corp-lane-throughput.md)) must never be
  disabled, nor its traffic re-routed, on throughput alone. Fail-closed exists to
  stop traffic taking a **wrong** path — escape down means the request would leave
  unproxied, so dropping it is the safe answer. A slow lane is still the **right**
  path, and refusing it has only two outcomes: strand a user whose sole route to
  that host is the tunnel, or push them onto a lane their own policy excluded.
  Degraded beats broken, and beats a silent policy violation. Still open: the
  threshold (absolute floor vs. a ratio against the fastest lane measured in the
  same run) and the surface (`status`, `explain`, the monitor, or a watch-time
  notice). `rowt speed` (3.4.11) already produces the signal such a rule consumes,
  so this stays advisory: measure, say so, let the human decide.

## 11. Suggested phasing (incremental, each shippable)

1. **Broaden sensing** — add network identity (SSID/gateway) + corp-state +
   captive-portal detection to what `probe`/`watch` already gather; expose via
   `rowt status`. No behavior change yet.
2. **Profiles, manual apply** — declarative profiles + `rowt apply <profile>` and
   a dry-run `rowt plan` that prints the posture + reasoning. Human drives it.
3. **Auto-apply safe transitions** — let `watch` select + apply profiles
   automatically for the low-risk mechanisms (system proxy, server, host/vm),
   confirm the rest. Enforce the safety invariants + teardown.
4. **Transparent mode** — sing-box `redirect` inbound + managed pf anchor, gated
   by the corp/EDR checks; still human-confirmed to enable.
5. **Full/TUN mode** — corp-up guard, kill-switch/DNS-leak, corp-off only.
6. **LLM assist** — profile suggestion, failure diagnosis, rule generation from
   intent; strictly off the reflex path, always confirmable.
