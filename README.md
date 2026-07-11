# rowt — personal VLESS/AnyTLS VPN alongside a corporate VPN

Run a personal **VLESS / AnyTLS / hysteria2** VPN for selected sites **while the corporate
VPN keeps the default route**, without the two clients fighting over the tunnel.

## TL;DR — the common path

Do the whole setup with **Shadowrocket (or any working VPN) ON** — `rowt up`
downloads sing-box for you, so there's no separate fetch step. Only switch to
the corp VPN once it's up and working.

```sh
# --- with Shadowrocket ON the whole time ---

# 1. install
brew install tanghong123/tap/rowt          # or: ./install.sh

# 2. bring your servers in — easiest is straight from Shadowrocket:
rowt server import            # writes an editable review file, prints a summary
$EDITOR ~/.config/rowt/sr-review.json   # delete stale servers/subs
rowt server import --apply    # (or: rowt server add '<vless://…>' / rowt sub add '<url>')

# 3. set up & start (auto-fetches sing-box if missing, then renders/starts/proxies)
rowt up host                  # host mode (the common one); 'rowt up' auto-detects, 'up vm' forces vm

# --- now switch networks ---
# 4. quit Shadowrocket, connect the CORP VPN, then verify:
rowt explain www.google.com   # which lane a destination takes (was 'rowt route')
rowt status                   # mode / server / proxy / reachability
```

(Mode `vm` also needs its image — `up vm` fetches that too if it isn't cached.
Probing for host-vs-vm is most accurate with the corp VPN on, so if you let
`rowt up` auto-detect, re-run it once after connecting corp.)

**One-time shell setup (recommended).** Add this to your `~/.zshrc` (after
`brew shellenv`):

```sh
eval "$(rowt shell-init)"
```

It gives you **tab-completion** for every subcommand *and* the `rowt-proxy-on` /
`rowt-proxy-off` aliases used below — idempotent, so it's safe to keep in your rc.

Day to day:

```sh
rowt-proxy-on                 # point THIS shell's curl/git/npm/… at rowt (rowt-proxy-off to undo)
rowt escape add youtube.com   # send another site through the personal tunnel
rowt corp add '*.intranet.example.com' '10.0.0.0/8'   # send a domain or CIDR into the corp VPN
rowt block add ads.example.com   # sinkhole an ad/telemetry domain (no DNS, no dial)
rowt use JP                   # pick a server (rowt ping shows the fastest)
rowt status                   # is it working? (mode / server / proxy / reachability)
rowt monitor                  # live full-screen dashboard (connections, errors, server health)
rowt reload                   # after switching Wi-Fi ↔ wired ↔ hotspot
rowt watch install            # (optional) auto-reload on every network change
rowt down                     # stop everything
```

**`rowt monitor`** is the read-only, `htop`-style live view — press `?` for keys,
`q` to quit. Great for watching what's going through escape vs direct, spotting
domains that are failing (candidates for `rowt escape add`), and checking server
latency at a glance. See [Monitor (TUI)](#monitor-tui).

Tired of running `rowt reload` by hand after every network switch? **`rowt watch
install`** sets up a LaunchAgent that does it for you — it re-applies on each
network change, but only while the router is up, and skips when nothing actually
moved. `rowt watch status` shows whether it's active; `rowt watch uninstall`
removes it. (It installs a scoped passwordless-sudo rule for just the
`networksetup` proxy toggles so a Wi-Fi↔Ethernet switch doesn't prompt; that's
removed on uninstall.)

Forgot where you left off? **`rowt onboard`** (or just `rowt` with no arguments)
prints a checklist of these steps with the exact next command to run.

That's it. The rest of this README explains how the three-way split works and the
full command set.

> **sing-box is fetched, not hand-installed.** rowt downloads a pinned `sing-box`
> into `~/.config/rowt/bin/` — but that download needs internet **from GitHub**,
> which in China means it must happen *while a working VPN (Shadowrocket) is on*.
> `rowt up` auto-fetches it if it's missing, which is why the common path above
> runs the whole setup (through `rowt up`) with Shadowrocket on, then switches to
> the corp VPN. You can also pre-download explicitly with `rowt fetch [host|vm]`.
>
> For **mode `vm`** the guest can't borrow the host's VPN (it's bridged onto the
> LAN), so `up vm` also fetches the ubuntu image + the linux sing-box into
> `~/.config/rowt/cache/` (if not already there); it then boots from the local
> image and installs sing-box into the guest from that cache — the VM never
> reaches GitHub itself.
>
> Alternatives if GitHub is blocked: `brew install sing-box` (rowt will use it),
> or download the tarball yourself and `SINGBOX_TARBALL=/path/to/it rowt fetch host`.
> Other deps (`brew`, `jq`, `python3`, `curl`) ship with macOS; mode `vm` also
> `brew install`s Lima.

The trick: don't run a second `tun`. `rowt` runs **sing-box on the host as
a rule-router** — a mixed HTTP+SOCKS proxy on `127.0.0.1:7890` — that splits
traffic **three ways**. Because it's a userspace proxy, there's no default-route
war with the corp client.

## Running commands with automatic environment set-up

CLI tools (`claude`, `git`, `npm`, `curl`, …) don't respect the macOS system
proxy — they only honour the `http_proxy` / `https_proxy` / `all_proxy`
environment variables. rowt gives you `rowt-proxy-on` / `rowt-proxy-off`
(from `eval "$(rowt shell-init)"`) to set and clear those, but if you
**hop between networks a lot** — corp VPN, home Wi-Fi, a hotspot, a plane —
or you have **several proxy apps** around (rowt, Shadowrocket, a corp client),
the right value keeps changing, and it's easy to forget which one is live.
Running a command through the wrong (or a stale) proxy env then fails in
confusing ways, and toggling `rowt-proxy-on`/`-off` by hand before every command
gets tedious.

**`rowt run <command> [args…]`** does it for you: it figures out which proxy path
can actually reach the internet *right now*, sets the env accordingly, and runs
your command with it — no manual toggling.

```sh
rowt run claude            # run claude through whatever path reaches the net
rowt run git pull          # a one-off git through the working proxy
rowt run npm install
```

It **probes in order** and uses the first that reaches the target:

1. the proxy already set in your shell (`http(s)_proxy` / `all_proxy`);
2. the **macOS system proxy** (exported as env for this run);
3. **rowt's own port** (`127.0.0.1:7890`) — only when the router is up *and* the
   system proxy is off (the "rowt is on but I don't want it hijacking everything"
   case);
4. **no proxy at all** (direct).

If none of them reach the target, `rowt run` **stops and does not run the
command** (exit 1) — so you never silently launch something into a dead network.

"Reaches the target" means the host actually answered (any `2xx`/`3xx`/`4xx` — a
`200`, a redirect, even a `401`/`404` all prove the path works). The default
target is **`https://www.google.com/`** — a domain that's genuinely blocked on a
restricted network, over **HTTPS** so a captive portal or poisoned DNS can't fake
it. (A CDN connectivity host like `gstatic.com` can stay reachable even when
Google proper is blocked — a false positive — which is why the real domain is
used.) Override with `ROWT_RUN_TARGET` to gate on whatever you actually need,
e.g. an API:

```sh
ROWT_RUN_TARGET=https://api.anthropic.com/ rowt run claude
```

Everything after `run` is the command, so its own flags (`--help`, `-v`, …) pass
straight through; use `rowt help run` for `run`'s own documentation.

### Captive portals (hotel / airplane Wi-Fi)

This is exactly why **"router up, system proxy *off*"** is a first-class mode —
and `rowt run` is built for it. On hotel/airline Wi-Fi you first have to load a
**captive-portal** page and authenticate. If the macOS **system proxy** is
pointed at rowt, that breaks: rowt can't reach the portal, and macOS's own
captive-portal *detection* gets confused, so the auth page never appears. Keeping
the system proxy **off** lets the portal flow work normally.

So the flow is:

1. `rowt down` (or just `rowt proxy off`) so the **system proxy is off** —
   `rowt status` shows `system proxy: No`, and `rowt monitor` shows
   `router: running` + `sys proxy: off`. Keep the router up.
2. Connect to the Wi-Fi and finish the captive-portal login in your browser.
3. Now run the tools that need the tunnel with **`rowt run`** — it routes just
   that command through rowt's port via env, without ever touching the system
   proxy (so it can't re-break the portal). It's captive-portal-aware too: the
   default `https://www.google.com/` target uses HTTPS, so a portal can't fake a
   success — `rowt run` refuses to launch until you've *actually* authenticated
   and the open internet is reachable.

`rowt monitor`'s split of **`router`** (is the tunnel engine up) vs **`sys
proxy`** (is system traffic being pointed at it) makes this mode legible at a
glance.

## Three-way routing

| bucket | list | where it goes | example |
| --- | --- | --- | --- |
| **escape** | `config/escape-domains.txt` | personal **VLESS tunnel** | google, youtube, github |
| **corp** | `config/corp-domains.txt` (domains **and** CIDRs) | **into the corp VPN** (via the OS routing table, so the corp client's own routes carry it) | `*.corp.example.com`, `10.0.0.0/8` |
| **direct** | everything else (the default) | **straight out the physical NIC**, bypassing *both* corp and escape | baidu, the China internet |

How each is enforced inside sing-box:

- **escape** → the VLESS selector (its uplink is bound to the physical NIC / or a VM, per mode).
- **corp** → a `direct` outbound with **no interface binding** — the OS routing table decides, so anything the corp VPN has a route for (internal IPs, its DNS) rides the tunnel. Corp domains also resolve via the **system resolver** (corp DNS) so intranet names resolve.
- **direct** → a `direct` outbound **bound to the physical NIC** (`en0`), resolving via a China DNS (223.5.5.5) over that NIC — no corp-DNS leak, China-optimal IPs.

Unlisted traffic defaults to **direct**; set `ROWT_FINAL=corp` to send the
catch-all through the corp VPN instead. Edit a list then reload:
`rowt reload`.

Fixing the tunnel-uplink problem (so escape traffic doesn't get swallowed by the
corp tunnel) is done two ways, picked automatically by `probe`:

| mode | how the uplink escapes | when |
| --- | --- | --- |
| **host** | VLESS outbound with `bind_interface=<physical NIC>` — forced out the physical interface | corp enforces via **routes** (common). Compact, no VM, works while travelling. |
| **vm** | a **bridged Lima VM** runs the VLESS tunnel; the host forwards escape traffic to it over SOCKS. The VM has its own network stack. | corp enforces via a **packet filter** and `bind_interface` can't bypass it. |

In both modes the system-proxy target stays `127.0.0.1:7890`; only the `escape`
outbound differs (direct VLESS vs. SOCKS→VM), so switching modes is transparent.

## Install

**Homebrew** (recommended):
```sh
brew install tanghong123/tap/rowt
```

**From source:**
```sh
./install.sh          # copies to ~/.local/share/rowt, symlinks ~/.local/bin/rowt
rowt version
```

The installer is **version-guarded**: re-running it does nothing if the installed
copy is the same or newer than the source (`--force` overrides; `--uninstall`
removes it). `--prefix DIR` / `--bindir DIR` change the locations. You can also
just run it in place from the repo as `./bin/rowt` without installing.

## Quick start

```sh
# one or more servers (vless:// , anytls:// , or hysteria2:// / hy2://)…
./bin/rowt server add 'vless://<uuid>@host:port?security=reality&sni=...&pbk=...&sid=...#JP' \
                       'anytls://<pass>@host2:port?sni=...#US' \
                       'hysteria2://<pass>@host3:443?sni=...&insecure=0#SG'
# …a subscription link (add several with --add)…
./bin/rowt sub add 'https://example.com/sub/xxxxx'
# …or migrate straight from Shadowrocket (see below):
./bin/rowt server import

# with the CORP VPN connected:
./bin/rowt up
```

### Import from Shadowrocket

`server import` reads your Shadowrocket install (servers, subscriptions, and `PROXY`
rules) and writes an **editable review file** to `~/.config/rowt/`. Delete
the entries you don't want (stale servers/subs), then apply:

```sh
./bin/rowt server import            # dump review file + print a summary
$EDITOR ~/.config/rowt/sr-review.json
./bin/rowt server import --apply    # import what remains; fetches the subs fresh
```

VLESS and AnyTLS servers import; Shadowsocks/other protocols are reported as
skipped. `PROXY`-rule domains are merged into your escape list.

### Removing servers / subscriptions

```sh
./bin/rowt server rm <tag>   # remove a manual server (or `server clear`)
./bin/rowt sub               # list subscriptions (numbered)
./bin/rowt sub rm <n>        # remove a subscription (or `sub clear`)
```

A server that came from a subscription is removed by dropping that subscription
(individually-dead nodes are auto-avoided by `use auto`).

`up` runs `probe` (chooses host/vm), brings up the VM if needed, renders the
configs, starts the router, and points the macOS system proxy at it. Then edit
which sites escape and reload:

```sh
$EDITOR config/escape-domains.txt
./bin/rowt reload
```

## Multiple servers & switching

Every configured server (VLESS, AnyTLS, or hysteria2, from manual imports and/or
subscriptions) is a member of a sing-box **selector** group named `escape`.

```sh
./bin/rowt server          # list servers; * marks the live one
./bin/rowt ping            # parallel latency test (router must run)
./bin/rowt use JP          # pin a specific server (manual — the default)
./bin/rowt use auto        # opt into auto-selection instead
```

**Manual is the default** (`use <tag>`): a plain selector that **never
health-checks its members**, so a flaky or dead subscription server just sits
idle and can't spin the CPU. Use `ping` to find a good one, then pin it —
switching between pinned servers is live via sing-box's Clash API.

`use auto` switches to a `urltest` that auto-picks the fastest **live** server
and re-probes them every `ROWT_AUTO_INTERVAL` (default 20m). Toggling
auto↔manual re-renders and restarts (the urltest appears/disappears).

The selector lives wherever the tunnel runs (host in mode `host`, the VM in mode
`vm`), so selection works identically for both. `sub add` remembers the URLs;
`sub update` re-fetches them.

## Do apps need proxy config?

Mostly **no**. `rowt proxy on` sets the macOS **system proxy** (SOCKS +
HTTP + HTTPS) to `127.0.0.1:7890`, and GUI apps that honour it (Safari, Chrome,
most Electron apps) just work — the app sends *all* its traffic to the one
proxy and sing-box decides escape/corp/direct per destination. The app never
needs to know about the three buckets.

Exceptions:

- **Terminal / CLI tools** (`curl`, `git`, `npm`, …) ignore the macOS system
  proxy. Point them at the router with env vars:
  ```sh
  eval "$(./bin/rowt proxy env)"      # sets http_proxy/https_proxy/all_proxy
  eval "$(./bin/rowt proxy env --off)"  # unset them
  ```
- **Apps with their own proxy setting** (some browsers/extensions): set them to
  SOCKS5 `127.0.0.1:7890` (or HTTP `127.0.0.1:7890`).
- **Apps that bypass proxies entirely** aren't routed by escape; they follow the
  OS (i.e. the corp VPN / direct) as usual.

### When a full VPN (Shadowrocket-direct) would serve you better

escape only routes what reaches its **proxy** — great for web browsers and normal
apps, but a proxy can't catch everything a full packet tunnel does. You'd be
better off with a full tunnel when you need to route:

- **Games** and other UDP-heavy apps,
- **Voice/video calls** (Zoom, Teams, WhatsApp/Telegram calls), WebRTC, QUIC/HTTP3,
- **Torrents / P2P**, mail clients, or other non-HTTP protocols,
- **Apps that ignore proxy settings** entirely.

The catch is that a full tunnel (Shadowrocket-direct) is exactly what **conflicts
with the corp VPN** — that's why this tool exists. So the trade is: *escape =
coexists with corp, covers proxy-aware apps; Shadowrocket-direct = covers
everything, but can't run next to the corp client.* If you need full coverage for
one app, mode `vm` is the middle path (point that app/device at the VM).

See **[DESIGN.md](DESIGN.md)** for the full packet- and DNS-level walkthrough of
how routing works with the corp VPN on, and a Shadowrocket→escape mapping table.

## Diagnosing what a site needs

The default policy is **direct**, but web apps quietly reach out to domains that
may be blocked or need the tunnel — and it's rarely obvious which. Two read-only
tools tell you exactly what's happening, so you can move *just those* domains into
`escape` (or `corp`) instead of tunnelling everything. (Both read live state; the
per-lane error capture needs the router running, i.e. after `rowt up`.)

**What's flowing right now — `rowt connections`.** A live snapshot of active
connections, aggregated by host, showing which lane each is on, bytes up/down, and
the rule that matched. This is the only view that shows **successful** traffic:

```
$ rowt connections
13 active connections:  escape=7  direct=6
  escape  api.anthropic.com:443     2× ↑35.7M  ↓160K   domain_suffix
  escape  claude.ai:443             1× ↑31K    ↓13K    domain_suffix
  direct  gateway.icloud.com:443    1× ↑5K     ↓5K     final
  …
```

`rowt connections escape` filters to one lane; `rowt connections -w` refreshes
every 2s (Ctrl-C to stop).

**What's failing — `rowt <lane> errors [period]`.** sing-box runs quietly (warn
level), so it only logs *failed/refused* connections. The router sorts those per
lane into `~/.config/rowt/log/lane-<lane>.log` (`timestamp⇥domain⇥reason`), and
`errors` summarizes them by domain, categorizing the reason:

```
$ rowt direct errors 10m
direct lane — 12 failed connection(s) in the last 10m, across 3 domain(s):
        8  timeout  rr1.googlevideo.com
        3  reset    x.com
        1  dns      gateway.icloud.com
  → tunnel the real ones: rowt escape add <domain>  (timeout/reset/refused ⇒ likely blocked; dns ⇒ often transient)
```

- `timeout` / `reset` / `refused` ⇒ the site is almost certainly **blocked** — a prime escape candidate.
- `dns` ⇒ usually a transient resolver blip, not a routing problem.

Because only failures are logged, an **empty** `escape errors` means that lane had
*no errors* — **not** that it carried no traffic (for that, use `connections`).
Periods take minute granularity — `5m 10m 1h 24h 7d all` (default 10m; `block
errors` defaults to 24h). `rowt <lane> log` live-tails the raw log. All these logs
rotate automatically (bounded disk, 9 generations kept).

**The workflow — find and fix a misbehaving app** (keeping the default DIRECT):

```sh
# 1. reproduce the problem with the app on the default policy, then:
rowt direct errors 10m               # which domains couldn't be reached directly
# 2. tunnel the blocked ones (timeout/reset/refused):
rowt escape add googlevideo.com x.com     # or 'rowt corp add <host>' for an intranet name
#    editing a lane auto-reloads the router
# 3. confirm the new routing:
rowt explain rr1.googlevideo.com     # -> escape (explains which rule matched)
rowt connections escape              # watch them actually flow through escape
```

`rowt explain <domain>` explains the lane any destination *would* take and why,
without hitting the site — handy for sanity-checking a change. And `rowt block
errors [period]` (default 24h) shows what the ad/telemetry sinkhole refused, so you
can spot a chatty tracker or confirm the block lane is doing its job.

## Commands

Commands are grouped by noun. `rowt help` prints the full list, annotated by how
often you'll reach for each (**●** everyday · **◐** occasional · **○** advanced).
Every command has detailed help: `rowt <command> --help` (or `rowt help <command>`).

**Lifecycle**

| command | what it does |
| --- | --- |
| `onboard` | guided getting-started checklist — shows how far you are and the exact next command. `rowt` with no args shows it too. |
| `up [host\|vm] [--force]` | ensure sing-box → probe (if no mode) → render → start router → proxy on. Idempotent (no-op if already up), except vm mode re-detects the VM's DHCP IP and re-wires if it moved; `--force` does a full rebuild. Switching to host mode powers the VM down. |
| `down` | tear everything down: system proxy off, **kill sing-box** (incl. strays), VM down. |
| `restart` | bounce the tunnel in place (host or vm, whichever is active) — no re-render, no proxy change. Use if sing-box is stuck/high-CPU. |
| `reload` | re-detect the network interface, re-render, restart, re-apply the proxy — run after switching Wi-Fi ↔ wired ↔ hotspot. |
| `watch <install\|uninstall\|status>` | install/remove a LaunchAgent that runs `reload` automatically on every network change — but **only while the router is up**, debounced, and a no-op when neither the interface nor the active-service proxy moved. It also runs once at **login**: if rowt isn't running but the system proxy is still set to `127.0.0.1:7890`, it clears it, so rowt's proxy effect never outlives a reboot. `install` also adds a scoped passwordless-sudo rule for the `networksetup` proxy toggles (so a Wi-Fi↔Ethernet switch doesn't prompt); `uninstall` removes both. |
| `status` | mode, servers, proxy state, reachability **and config validity** (absorbs the old `doctor`). |
| `explain <domain\|ip>` | explain which lane a destination takes — `escape` (proxy), `corp` (into the corp VPN), or `direct` (pass-through) — and which rule matched. Mirrors the real rule order (corp domain → corp CIDR → escape domain → final); adds a live HTTP check if the router is running. (`route` still works as a hidden alias.) |
| `report` | full offline diagnostic (deps, configs, per-server reachability, DNS, through-proxy tests, log tail) → `~/.config/rowt/diag-*.txt`, **secrets masked**, for sharing. |
| `monitor` | **full-screen read-only TUI** (`htop`-style) — the live view of everything at once: connections + throughput, errors/blocked over a rolling window, and server health. See [Monitor (TUI)](#monitor-tui). |
| `run <command> [args…]` | run a command through whatever proxy path actually reaches the internet — probes, in order, the current shell proxy env → the macOS system proxy → rowt's port (if the router is up and the system proxy is off) → direct, and execs the command with the first where the target host answers (default `https://www.google.com/`; override `ROWT_RUN_TARGET`). Aborts without running if none work. Handy for CLI tools (`claude`, `git`, `npm`…) that ignore the system proxy: `rowt run claude`. |

**Servers & selection**

| command | what it does |
| --- | --- |
| `server list` | list servers (`*` = active). |
| `server add '<vless://\|anytls://\|hysteria2://…>' [more…]` | add manual server(s) from link(s), deduped. |
| `server rm <tag>` / `server clear` | remove a manual server / clear all manual. |
| `server import [--apply]` | import from Shadowrocket (servers **and** subs) via an editable review file. |
| `server import <file.json>` | restore manual servers from a `server dump` (round-trips). |
| `server dump [file]` | export the manual servers as JSON (backup; has secrets). Subscription servers come from their subs — use `sub dump`. |
| `sub list\|add <url>\|rm <n>\|update\|clear` | manage subscriptions. |
| `sub import [--apply]` | same as `server import` (Shadowrocket). |
| `sub import <file>` | restore subscriptions from a `sub dump` (round-trips). |
| `sub dump [file]` | export the subscription URLs (one per line). |
| `use <tag>` / `use auto` | pin a server (manual, nothing probed) or auto-pick the fastest live server. |
| `ping [tag]` | **parallel** latency test through the tunnel (fastest first, `*`=active). `ROWT_PING_URL` (default Cloudflare) / `ROWT_PING_TIMEOUT` (8s). |
| `probe` | with corp VPN up, test all servers (default route vs physical NIC) and pick `host` or `vm`. |

**Routing lanes** — `escape` and `corp` share the same verbs:

| command | what it does |
| --- | --- |
| `escape` / `corp` / `block` (no verb) | list the lane. |
| `… add <d>…` / `… rm <d>…` | add / remove domains (corp also takes CIDRs). Reloads if running. |
| `… import <file>` | batch-add one domain per line from a file (merges; never replaces). |
| `… clear` | remove every entry (keeps the file's comment header). Reloads if running. |
| `… dump [file]` | export the lane (stdout, or to a file for backup/versioning). |
| `direct errors [5m\|10m\|1h\|…\|all]` | **which domains failed on the default DIRECT lane** in that window (default 10m) — your escape candidates. Reason is categorized (timeout/reset/refused ⇒ likely blocked; dns ⇒ transient). Reproduce a misbehaving app, then `direct errors 10m` and `escape add` the real ones. |
| `<lane> errors [5m\|…\|all]` | same for any lane: `block errors` (default 24h) = what got sinkholed; `escape errors` / `corp errors` = failures on those lanes. Only *failed/refused* connections are logged — an empty list means no errors, **not** no traffic. |
| `<lane> log` | live-tail that lane's connection-error log. |
| `connections [lane\|-w]` | **live view of active connections** and which lane each is on (escape/direct/corp/block), with bytes up/down and the matched rule. Unlike `errors`, this shows *successful* traffic — "what's actually going through escape right now". `-w` refreshes every 2s. |

The router captures each failed/refused connection per lane (`timestamp⇥domain⇥reason`)
into `~/.config/rowt/log/lane-<lane>.log` — the block flood is diverted out of
`host.log`; direct/corp/escape failures are kept in `host.log` too. All rotate.

The **block** lane is an ad/telemetry sinkhole: matching domains are refused
instantly — no DNS lookup, no dial — which stops the direct-lane retry storm
(dead ad/tracker hosts retried in a tight loop) that can spike sing-box CPU. It's
additive on top of a large **geosite** ad/tracker rule-set (thousands of
domains) that `rowt fetch host` caches and rowt renders in automatically when
present (offline-safe: the hand list works without it). Block runs **before**
corp/escape/direct.

**Proxy & internals**

| command | what it does |
| --- | --- |
| `proxy status\|check\|on\|off\|env [--off]` | show / verify / set / unset the macOS system proxy; `env` prints CLI env exports. `on`/`off` are **idempotent** — they read the current state first (no sudo) and only invoke admin for what's actually wrong, so re-running never prompts if already correct. `on` is a **no-op unless the router is running** (else it would just point the system proxy at a dead port and break traffic) — run `rowt up` first, or `proxy on --force` to override. `check` exits 0 iff fully configured (used to re-apply after the OS config drifts). |
| `shell-init` | shell integration to `eval` in your rc — defines `rowt-proxy-on`/`-off` **and** loads tab-completion for subcommands (zsh/bash), idempotent. Add `eval "$(rowt shell-init)"` to `~/.zshrc`. |
| `completion <zsh\|bash>` | print a tab-completion script (normally auto-loaded by `shell-init`; defers to the live command set so it never drifts). |
| `render` | regenerate the sing-box configs from current state. |
| `fetch [host\|vm\|both]` | pre-download while a VPN is on so `up` works offline. `host` = the macOS sing-box binary; `vm` = the ubuntu image + linux sing-box tarball into `~/.config/rowt/cache/` (then `up vm` boots from the local image and installs sing-box into the guest from that cache — **the VM never reaches GitHub itself**). Default `both`. |
| `router up\|down\|restart\|status\|log` | the local rule-router process — the always-on proxy on `127.0.0.1:7890` (the front door your system proxy points at), which runs in **both** modes. (`router` is a process, not a mode — switch modes with `up host`/`up vm`.) |
| `vm up\|down\|restart\|status\|log\|delete` | the bridged Lima VM (mode `vm`). |
| `version` | print the version (`major.minor.revision`). |

## Monitor (TUI)

`rowt monitor` is a full-screen, **read-only** terminal UI for watching a running
router — the observe-everything companion to the one-shot `status` /
`connections` / `errors` commands. It never changes routing, servers, or the
proxy; everything is derived on a 2-second tick.

```sh
rowt monitor            # live view (falls back to a demo fixture if nothing is running)
rowt monitor --fixtures # force the offline demo
```

**Layout** (reflows at 130 columns — side-by-side above, stacked below):

- **identity band** — mode/interface, active server + latency, router, proxy,
  config validity, uptime, and a status dot: green **LIVE** (breathing), red
  **DOWN** (router unreachable), orange **ERROR** (active server failing its
  probe / auto-mode with nothing reachable), grey **PAUSED**.
- **live · connections** — per-lane throughput rates (`↑`/`↓` B/s) and a table of
  active connections (host:port, concurrency, cumulative bytes, matched rule),
  colored by lane. Block-lane traffic is excluded.
- **errors & blocked** — failures and sinkholed domains over a rolling window
  (`5m`/`10m`/`1h`/`24h`), colored by category (dns = transient, timeout/reset/
  refused = persistent, blocked = purple).
- **server health** — `N up / N down`, and a marquee of the reachable pool with
  latencies (the active server marked `▶`). Servers are probed through the tunnel
  against Google's `generate_204` every 10 min (press `r` to re-probe now).

**Keys:** `↑↓`/`jk` move · `←→`/`hl` switch pane · `Tab` cycle focus · `f`
(or `1`/`2`/`3`, `0`/`Esc`) lane filter · `w` / `[` `]` errors window · `y` copy
the selected domain / host:port · `r` re-probe servers · `p` pause · `?` help ·
`q` quit. Mouse: wheel scrolls the list under the pointer; click a lane / window
tab / row to select it.

**Sources:** the clash API (`127.0.0.1:9090`), `host.json`, `state`/`servers.json`,
and `lane-*.log`. Env: `ROWT_MONITOR_PROBE_INTERVAL` (secs, default 600),
`ROWT_PING_URL` (probe target). It's a small Rust/`ratatui` binary built and
installed alongside `rowt` (also runnable standalone as `rowt-monitor`).

## How it decides (probe)

With the corp VPN connected, `probe`:
1. checks the server is reachable via the **default route** (sanity: server up / online);
2. checks it's reachable when **bound to the physical NIC** (`curl --interface`).

If (2) works → **host** mode. If (2) fails while (1) works → the corp client is
enforcing at the packet-filter layer, so → **vm** mode.

## Design notes / bulletproofing

- **Fail-closed:** listed domains only ever use the `escape` outbound. If the
  tunnel is down they **fail**, they don't silently leak onto the corp path.
- **Home LAN excluded by corp VPN:** the host→VM SOCKS hop rides the LAN, which
  corp full-tunnel clients exclude, so mode `vm` needs no route exclusion at home.
- **Remote DNS:** the router sniffs the destination host and hands the *domain*
  to the escape server, so escape lookups resolve at the exit — no leak to corp DNS.
- **Safe to leave running with the corp VPN off.** The escape and direct buckets
  work regardless; only *corp*-listed sites need the corp VPN and will simply fail
  until you connect it. No leak, no conflict — connect corp only when you need
  work sites.
- **Pinned sing-box** (`SINGBOX_VERSION`, default 1.13.14 — ≥1.12 for AnyTLS) is
  downloaded to `~/.config/rowt/bin/` so configs always match the schema.
- **No secrets in the repo:** servers/subscriptions live under
  `~/.config/rowt/` (mode 600); the routing lists are seeded there from the
  repo templates on first use, so imports/edits never touch the tracked repo.
- **Live switching:** sing-box's Clash API (`127.0.0.1:9090`, or the VM's LAN IP
  in mode `vm`) is protected by a random secret stored in the state file.

## Requirements

- macOS on Apple Silicon (Intel works too), `brew`, `jq`, `python3`, `curl`.
- Mode `vm` additionally installs **Lima + socket_vmnet** via brew and writes a
  bridged network to `~/.lima/_config/networks.yaml` (needs one `sudo` to
  authorize `socket_vmnet` — `limactl sudoers`).
- **Bridge over Ethernet when you can** — bridging onto Wi-Fi is the flaky path.

## Files

```
bin/rowt              main tool (subcommands above)
config/vless-parse.py      vless:// / anytls:// link → sing-box outbound (stdlib)
config/sr-import.py        Shadowrocket store + rules → servers/subs/domains
config/escape-domains.txt  template for bucket 1 (escape) — seeded into ~/.config
config/corp-domains.txt    template for bucket 2 (corp), domains + CIDRs
lima/rowt-vm.yaml        bridged Lima VM template (mode vm)
```

The live, user-editable copies of the two `*-domains.txt` lists live at
`~/.config/rowt/`; the repo files are just first-run templates.

> ⚠️ Routing around a mandated corporate VPN may violate acceptable-use policy.
> This tool is for a **personal machine at home**; confirm it's sanctioned before
> relying on it.
