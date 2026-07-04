# rowt — personal VLESS/AnyTLS VPN alongside a corporate VPN

Run a personal **VLESS / AnyTLS** VPN for selected sites **while the corporate
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
rowt route www.google.com     # HTTP 200 through the tunnel
rowt status                   # mode / server / proxy / reachability
```

(Mode `vm` also needs its image — `up vm` fetches that too if it isn't cached.
Probing for host-vs-vm is most accurate with the corp VPN on, so if you let
`rowt up` auto-detect, re-run it once after connecting corp.)

Day to day:

```sh
rowt-proxy-on                 # point THIS shell's curl/git/npm/… at rowt (rowt-proxy-off to undo)
rowt escape add youtube.com   # send another site through the personal tunnel
rowt corp add '*.intranet.example.com' '10.0.0.0/8'   # send a domain or CIDR into the corp VPN
rowt block add ads.example.com   # sinkhole an ad/telemetry domain (no DNS, no dial)
rowt use JP                   # pick a server (rowt ping shows the fastest)
rowt status                   # is it working? (mode / server / proxy / reachability)
rowt reload                   # after switching Wi-Fi ↔ wired ↔ hotspot
rowt watch install            # (optional) auto-reload on every network change
rowt down                     # stop everything
```

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
# one or more servers (vless:// or anytls://)…
./bin/rowt server add 'vless://<uuid>@host:port?security=reality&sni=...&pbk=...&sid=...#JP' \
                       'anytls://<pass>@host2:port?sni=...#US'
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

Every configured server (VLESS or AnyTLS, from manual imports and/or
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
| `watch <install\|uninstall\|status>` | install/remove a LaunchAgent that runs `reload` automatically on every network change — but **only while the router is up**, debounced, and a no-op when neither the interface nor the active-service proxy moved. `install` also adds a scoped passwordless-sudo rule for the `networksetup` proxy toggles (so a Wi-Fi↔Ethernet switch doesn't prompt); `uninstall` removes both. |
| `status` | mode, servers, proxy state, reachability **and config validity** (absorbs the old `doctor`). |
| `route <domain\|ip>` | explain which lane a destination takes — `escape` (proxy), `corp` (into the corp VPN), or `direct` (pass-through) — and which rule matched. Mirrors the real rule order (corp domain → corp CIDR → escape domain → final); adds a live HTTP check if the router is running. |
| `report` | full offline diagnostic (deps, configs, per-server reachability, DNS, through-proxy tests, log tail) → `~/.config/rowt/diag-*.txt`, **secrets masked**, for sharing. |

**Servers & selection**

| command | what it does |
| --- | --- |
| `server list` | list servers (`*` = active). |
| `server add '<vless://\|anytls://…>' [more…]` | add manual server(s) from link(s), deduped. |
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
| `direct stats [5m\|10m\|1h\|…\|all]` | **which domains failed on the default DIRECT lane** in that window (default 10m) — your escape candidates. Reason is categorized (timeout/reset/refused ⇒ likely blocked; dns ⇒ transient). Reproduce a misbehaving app, then `direct stats 10m` and `escape add` the real ones. |
| `<lane> stats [5m\|…\|all]` | same for any lane: `block stats` (default 24h) = what got sinkholed; `escape stats` / `corp stats` = failures on those lanes. |
| `<lane> log` | live-tail that lane's connection log. |

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
