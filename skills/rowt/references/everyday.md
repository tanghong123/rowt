# rowt: everyday operation

Read this for day-to-day requests after onboarding. `rowt <cmd> --help` is the
version-current truth. Any command that starts or restarts the router runs in the
foreground with output redirected to a file (see SKILL.md → Rules).

## Networks, upgrades, stop and start

- **Network changes** (Wi-Fi ↔ Ethernet ↔ hotspot, corp VPN on or off): the watchdog
  (`rowt watch install`) re-applies by itself. It reloads only when the physical
  interface or the proxy actually moved; a VPN-only change runs `corp sync` and
  reloads only if the corp lane changed. Without the watchdog, run `rowt reload`.
  `rowt restart` bounces sing-box in place without re-rendering. `rowt down` stops
  everything: proxy off, router down.
- **After `brew upgrade rowt`**: run `rowt reload` once. The routing config
  (`host.json`) is rendered by the binary, so a new version changes nothing on the
  wire until the next render. The watchdog re-syncs its own LaunchAgent; if
  `rowt status` says the agent is `NOT loaded` or STALE, run `rowt watch refresh`.
- **Abroad, or anywhere without a firewall**: `rowt up local` runs with no tunnel and
  contacts no server. The escape rules stay in place but point at direct, so they
  still out-rank broader block entries. The direct lane resolves through a public
  resolver instead of AliDNS, and the watchdog stops probing a tunnel that isn't
  there. A bare `rowt up` chooses local mode by itself when Google answers over the
  physical NIC. Go back with `rowt up host`. Local mode works with a China corp VPN
  up, because direct is bound to the NIC.
- **Captive portals (hotel, airport, plane Wi-Fi)**: automatic with the watchdog. It
  probes directly at the Wi-Fi's own resolver, drops the system proxy so the login
  page can load, opens that page in the browser, and restores the proxy after login.
  Meanwhile `rowt status` shows `captive:`. Without the watchdog: `rowt proxy off` →
  log in → `rowt proxy on`. For a venue the user visits repeatedly, add its portal host
  to the **hotspot** lane: `rowt hotspot add unitedwifi.com`. That puts it on macOS's
  proxy bypass list (`x.com` and `*.x.com`), so the page loads on the first try with
  the proxy on. It's not a routing lane: nothing is rendered, and an edit only
  refreshes the bypass list. If a portal page never loads, ask for its hostname (the
  blank tab's address bar, or `watch.log`) and add it.

## Servers

- `rowt ping` ranks every server through the tunnel, fastest first; `*` marks the
  active one.
- `rowt use <tag>` pins a server: a plain selector that never health-checks, so
  flaky servers can't spin the CPU.
- `rowt use auto` switches to the fastest live server and moves off a dead one. In the
  monitor, `a` toggles auto, and turning it off pins the server in use.
- Pool: `rowt server list|add|rm|clear|import|dump`. Subscriptions:
  `rowt sub list|add|rm|update|clear|import|dump`; `update` re-fetches them.
- `server dump` and `sub dump` output contains secrets.

## Lanes

- `rowt escape|corp|block|hotspot <list|add|rm|clear|import|dump>`; escape, corp and
  block also have `errors`.
- An entry lives in exactly one lane: adding it to one removes it from the others,
  hotspot included.
- Entries are suffixes on a label boundary: `z.com` matches `z.com` and `a.z.com`, but
  not `xz.com`. `--domain` matches the whole host only.
- corp also takes CIDRs. `--force` is required for a whole namespace such as `com`.
- The longest match wins across lanes, and an exact `domain:` entry beats every
  suffix.
- `rowt explain <domain|ip>` says which lane a destination takes and why.
- `rowt escape errors` and `rowt direct errors` show what failed. A domain failing
  direct is a candidate for escape.
- **geosite categories** (escape and block only): `rowt escape add geosite:google`
  routes every Google domain (all ccTLDs, gstatic, youtube);
  `rowt block add geosite:tiktok` blocks a whole service.
  - Names are [sing-geosite](https://github.com/SagerNet/sing-geosite) tags. The set
    is fetched and cached in `~/.config/rowt/cache/`.
  - A plain `escape add <domain>` never swaps in a category; it only suggests the ones
    that cover the domain. A specific domain beats a category.
  - If GitHub is unreachable, copy `geosite-<name>.srs` into the cache from a machine
    that can reach it.
- **Corp lane**: `rowt corp suggest` lists this network's advertised internal domains
  (on the corp LAN or VPN). `rowt corp sync [--dry-run]` mirrors the corp VPN's DHCP
  domains and routed CIDRs; the watchdog runs it on connect. Learned entries persist
  when the tunnel drops, because they're still needed in the office. Private and
  overlay ranges already go to the corp lane.
  - For Tailscale or other overlays, list the interface label in
    `~/.config/rowt/sync-ifaces.txt`.
  - A user's own escape or block entry always wins over an auto-discovered corp domain.
  - `ROWT_PRIVATE_DEFAULT=direct` restores the old behaviour, where private IPs go out
    the NIC.

## Watching

- `rowt status`: mode, server, engine, proxy, reachability and the watchdog line.
- `rowt connections [lane|-w]`: live connections per lane.
- `rowt metrics top [secs]`: the heaviest domains. The collector records per-domain
  bytes (`rowt metrics query "<SQL>"` is read-only).
- `rowt audit`: who changed what, whether a person (`by=zsh`) or the watchdog
  (`by=launchd`).
- `rowt report`: a masked, shareable diagnostic file.
- **`rowt monitor`** is a TUI; the user runs it in their own terminal. Keys:
  - `v` flips the view (live, up-history, down-history); `s` sets the span; `f`
    filters by lane; `/` searches.
  - `e`/`c`/`b`/`d`/`t` route the selected host to escape, corp, block, direct or
    hotspot. The shifted key routes its parent suffix.
  - `u` uses a server; `a` toggles auto server selection; `?` shows help.
  - `--theme dark|light|auto` sets the colours; pin it if auto guesses the background
    wrong.

## CLI tools and the system proxy

- **CLI tools ignore the macOS system proxy.** `rowt run <cmd>` probes, in order, the
  shell's proxy env, the system proxy, rowt's port, then direct, and execs the command
  through the first path that reaches `https://www.google.com/`. Change that target
  with `ROWT_RUN_TARGET=https://api.anthropic.com/`.
- `rowt proxy env` prints exports for a shell, including a `no_proxy` with the same
  bypass list macOS has. `rowt-proxy-on` and `rowt-proxy-off` come from
  `eval "$(rowt shell-init)"`.
- `rowt proxy status|check|on|off` covers the system proxy itself. `check` also
  reports a PAC that would bypass rowt.

## Sharing inside a tailnet (from `rowt shell-init`)

- **Sharing:** `rowt-share-on` publishes the loopback proxy through a tailnet-only
  Tailscale Serve TCP forward on `ROWT_SHARE_PORT` (17890). `rowt-share-status` shows
  it and `rowt-share-off` removes it. It never uses Funnel and never binds rowt to the
  LAN.
- **Tailnet policy must restrict that port to trusted devices**: a shared client can
  use every lane, corp included.
- **Using a shared rowt:** on a client, `rowt-remote-on <host>` points the shell's
  HTTP/HTTPS/`socks5h` proxy variables at a tailnet host after checking it's
  reachable. A short name is expanded through `tailscale status --json`.
  `rowt-remote-off` clears them.
- **System-wide:** `rowt-remote-system-on <host>` and `-off` do the same for macOS
  (admin only when the settings change). Don't combine it with a local rowt watchdog:
  both manage the same settings.

## Moving, sharing and removing the setup

- **Moving:** `rowt config export` writes a chmod-600 `.tgz` of the servers,
  subscriptions and lane rules. It holds credentials, so copy it only over an
  encrypted channel, then `rowt config import <file>` and `rowt up`.
- **Sharing:** `--no-servers` exports only the lane lists, with no credentials, which
  is safe to hand to a colleague.
- **Importing:** `config import` **merges** by default. It unions each lane list,
  reports an entry that lands in a different lane as a conflict, and leaves the
  recipient's servers alone. `--replace` overwrites instead.
- **Removing:** `rowt uninstall` reverses the setup: down, proxy off, the watchdog and
  its sudoers rule removed, the shell-init line stripped. It keeps
  `~/.config/rowt`; add `--purge` to wipe that too. Then run `brew uninstall rowt`.
  `brew uninstall` alone leaves the watchdog, the proxy setting and the rc line
  behind.
