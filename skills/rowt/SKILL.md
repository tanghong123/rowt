---
name: rowt
description: Set up, run and troubleshoot rowt — the macOS split-router (`brew install tanghong123/tap/rowt`) that sends chosen sites through the user's own VLESS/VMess/AnyTLS/hysteria2/Shadowsocks/Trojan/TUIC servers (escape), intranet traffic into their corporate VPN (corp), and everything else straight out the physical network (direct), all from one local proxy on 127.0.0.1:7890. Use it FIRST for onboarding — "set up / install rowt", moving off Shadowrocket, Clash Verge, V2Box or FlClash, adding share links or a subscription, running a personal VPN alongside a work VPN. The skill diagnoses the Mac with `rowt doctor` and drives the setup itself, handing the user only what needs their hands. Also for everyday operation (up/down/reload, switching servers, escape/corp/block/hotspot lanes and geosite sets, captive portals, local mode abroad, tailnet sharing, config export/import, uninstall) and debugging (router down, a site on the wrong lane, a slow lane, high CPU, corp names failing).
---

# rowt

**macOS only.** rowt runs sing-box as a local HTTP+SOCKS proxy on `127.0.0.1:7890` and
classifies every connection by its sniffed domain or IP into a lane. A personal tunnel
and a corp VPN coexist because nothing fights over the default route:

| lane | listed in | goes | typical |
|---|---|---|---|
| **escape** | `escape-domains.txt` | the user's own server, bound to the physical NIC | google, github, AI APIs |
| **corp** | `corp-domains.txt` (domains **and** CIDRs) | the OS routing table, i.e. into the corp VPN | intranet |
| **direct** | everything unlisted | out the physical NIC, around both VPNs | local sites |
| **block** | `block-domains.txt` + a built-in ad set | sinkholed: no DNS, no dial | ads, telemetry |

Unlisted **private/overlay IPs** (RFC 1918, `100.64/10`, link-local) take the corp lane,
so VPN, LAN and Tailscale hosts work with no configuration; only unlisted public IPs
go direct. Everything user-editable lives in `~/.config/rowt/`. The sing-box engine is
bundled by the formula and pinned (1.13.x; 1.14.x is known-bad). `rowt <cmd> --help` is
the version-current truth for every command below.

## Onboarding: diagnose, then drive

**You run the commands; the user does only what needs their hands**: an admin password,
GUI apps (quitting a VPN client, connecting the corp VPN), and choices (which servers
to keep, which sites to tunnel). Say what each step does before it, and show the
evidence after it.

### Step 0: diagnose, always first

If rowt isn't installed yet (`command -v rowt` finds nothing), check the three things
that decide whether it can be:
- `uname -s` must say `Darwin`, because rowt is macOS-only.
- `command -v brew` must find Homebrew. If it doesn't, the **user** installs it (its
  installer asks for a password), or clones the repo and runs `./install.sh`.
- `uname -m` saying `x86_64` means `brew install` compiles rowt, which takes Rust and
  several minutes. Warn the user, then proceed.

Then, after asking, run `brew install tanghong123/tap/rowt`.

With rowt installed, run both in parallel:

```sh
rowt doctor <your API host>    # the Mac AROUND rowt; ends with FLAGS
rowt onboard                   # rowt's own setup checklist
```

- **Your API host:** Claude Code uses `api.anthropic.com` (the default), Codex uses
  `api.openai.com` or `chatgpt.com`, and Gemini CLI uses
  `generativelanguage.googleapis.com`.
- **What doctor reports:** it changes nothing and takes about 10 s. It shows the
  system and rowt's version, and which apps own ports 7890 and 9090. It lists other
  proxy and VPN software and who holds the default route, the system proxy and any
  PAC, and reachability over the physical NIC and through 7890. It also says whether
  passwordless proxy writes are available and which clients can be imported. It
  ends with **FLAGS**.

Then tell the user in plain words: what's installed, which VPN clients and corp VPN
you found, whether they're behind a firewall, what could bite (the flags), and the
plan. Ask only what the evidence can't answer. Re-run `rowt doctor` after any big
change.

### What the flags mean, and who acts

| flag | meaning → action (**who**) |
|---|---|
| `NO_BREW` | rowt was installed some other way (`./install.sh`), so upgrades are manual: `git pull` in the clone, then `./install.sh`. |
| `INTEL_MAC` | This Mac builds rowt from source on every `brew upgrade` (Rust, several minutes). A few prebuilt sidecars are Apple-Silicon-only; rowt runs without them. |
| `ROWT_OUTDATED` | **Agent**: `brew upgrade rowt`, then `rowt reload` if it's running. The routing config is rendered by the binary, so an upgrade alone changes nothing on the wire. |
| `PORT_BUSY` | Another app (usually a Clash client) holds rowt's port, so rowt can't start. **Never kill it.** Finish every other step; then the **user** quits that app and runs `rowt up host` in their own terminal. If that app also carries your session (`AGENT_VIA_VPN`), give the resume command *before* they quit it, and verify (step 6) from the resumed session. Otherwise, have them move the app's own port off 7890, and the normal order stands. |
| `VPN_HOLDS_DEFAULT` | A VPN owns the default route. If it's the personal client, that's expected until the switch-over (escape and direct bind to the NIC, so rowt works alongside it). If it's the corp VPN, that's the design. |
| `SYSTEM_PROXY_OTHER` | Another client's system proxy. `rowt up` replaces it. After the **user** quits that client, run `rowt proxy check`: some clients clear the proxy on quit, and then you run `rowt proxy on`. |
| `PAC_ON` | An auto-proxy, usually set by the corp VPN client, outranks the manual proxy rowt sets. Browsers then bypass rowt while every rowt check passes. rowt won't touch it and neither should you. The **user** decides: turn it off, or accept that only CLI tools use rowt (`rowt proxy env`). |
| `NO_PHYSICAL_NETWORK` | No Wi-Fi or Ethernet interface has an address. rowt binds escape and direct to the physical NIC, so the **user** connects a network first (`ROWT_IFACE` names an unusual NIC). |
| `NET_DOWN` | Nothing answers over the NIC. The cause is a captive portal (log in), being offline, or a sandbox around your shell (ask for network access). Fix that first. |
| `OPEN_INTERNET` | No firewall on this network. A bare `rowt up` would pick local mode (no tunnel), so onboard with `rowt up host` to exercise the servers, and explain local mode. |
| `GITHUB_BLOCKED` | Keep the old VPN on during setup. First-run downloads (ad and geosite rule-sets) and subscription fetches use the shell's current path. |
| `AGENT_API_NEEDS_TUNNEL` | Your own API is refused on the direct path, so it must ride escape. Add its domains before the switch-over (step 3). |
| `AGENT_VIA_VPN` | Your session rides the old VPN's tunnel and drops the moment it goes off. Hand over the resume command first (step 7). |
| `AGENT_VIA_PROXY_ENV` | Your session uses a proxy env. If that's rowt's port it survives the switch. If it's another app's port it stops with that app, so hand over the resume command. |
| `NO_SUDO_RULE` | You can't answer a password prompt. The **user** runs `rowt watch install` once, in their own terminal (step 4). |
| `NO_IMPORT_SOURCE` | Ask for share links or a subscription URL (step 2). |
| `YQ_MISSING` | **Agent**: `brew install yq`. Clash Verge and FlClash imports need it. |

`rowt onboard` covers rowt's own state. If its engine line says sing-box **must be
replaced**, run `rowt fetch host` (with a working path to GitHub) or `rowt reload`.

### Two constraints set the order

1. **No password prompts.** Your shell has no terminal, so `sudo` can't ask for a
   password. In host mode only the system-proxy write needs admin, and
   `rowt watch install` installs a scoped passwordless rule for exactly those writes.
   Have the user run it once before `rowt up`. A `sudo -v` in their terminal does
   nothing for you, because sudo's credential cache is per terminal.
2. **Don't cut your own connection.** Behind a firewall, your API calls currently
   ride the old VPN. Keep it on until rowt demonstrably carries your API host through
   127.0.0.1:7890, and hand over the resume command before the user turns the old VPN
   off.

### The steps

1. **Install** (agent, if needed): `brew install tanghong123/tap/rowt`.
   `rowt skill install` links this skill for future sessions; if you're reading
   this, that's done.
2. **Servers** (agent, plus the user's choices). For each detected client, accumulate
   its servers into one review file. Each run skips anything already in the pool or
   the file:
   ```sh
   rowt server import --from <shadowrocket|clash-verge|v2box|flclash>   # → ~/.config/rowt/import-review.json
   ```
   Show what came in. **Never print that file: it holds credentials.** Project the
   safe fields instead:
   ```sh
   F=~/.config/rowt/import-review.json
   jq -r '.servers | to_entries[] | [.key, (.value._source // "?"), .value.tag, .value.type] | @tsv' "$F"
   jq -r '.subscriptions | to_entries[] | [.key, (.value._source // "?"), (.value.name // .value.title // "?"), ((.value.url // "") | (capture("^(?<h>[a-z0-9]+://[^/?#]+)").h // "?"))] | @tsv' "$F"
   jq -c '{skipped, proxy_domains: (.proxy_domains | length)}' "$F"
   ```
   - `skipped` counts, by type, the client's entries the importer couldn't convert.
     Either it can't read that type from that client (the Shadowrocket importer takes
     VLESS, AnyTLS and Shadowsocks), or the entry is an unsupported variant such as
     SSR, a plugin or a chain. If the user needs one, ask for its share link and use
     `rowt server add`.
   - `proxy_domains` are the client's own PROXY-rule domains, which `--apply` merges
     into the escape lane. Ask the user; `.proxy_domains = []` keeps rowt's defaults
     instead.
   - Drop what the user rejects by index, and edit the file only this way:
     ```sh
     jq 'del(.servers[3,5]) | del(.subscriptions[0])' "$F" >| "$F.new" && mv "$F.new" "$F" && chmod 600 "$F"
     ```
   - **If there are subscriptions, apply them first.** A subscription is re-fetched
     on every update, while a server copied out of one goes stale when the provider
     rotates it.
     1. Set `.servers = []` (same write-back) and run `rowt server import --apply`,
        which fetches the subscriptions.
     2. Re-run the import. Servers a subscription now provides should be skipped as
        already in the pool, leaving the user's own.
     3. Curate those and run `rowt server import --apply` again.

     If `--apply` prints `subscription fetch failed`, that subscription has expired
     or can't be reached from here; `(HTTP 404)` on that line means the provider
     dropped it. Its servers won't be skipped on the second pass, so tell the user
     and keep only the servers they recognize.

   If there's no client, ask for links instead: `rowt server add '<link>'` (vless,
   vmess, anytls, hysteria2, ss, trojan, tuic) or `rowt sub add '<url>'`. The user
   pastes secrets into the chat, so pass them straight to the command and never
   repeat them back. Finish with `rowt server list`.
3. **Lanes** (agent). Editing a lane doesn't start anything; it only restarts a router
   that is already running.
   - For `AGENT_API_NEEDS_TUNNEL`, check `rowt explain <your API host>`. A new
     install's defaults already escape Anthropic, Claude, OpenAI and ChatGPT, next to
     Google (which covers Gemini), GitHub, Meta and X.
   - A lane list from an older rowt may lack them: `rowt escape add anthropic.com
     claude.ai claude.com`, or `rowt escape add openai.com chatgpt.com`.
   - Ask which other blocked services they use; `rowt escape add geosite:<name>`
     covers a whole service.
4. **Admin, once** (the **user**, in a Terminal window of their own):
   `rowt watch install`. It asks for the password once. It installs the passwordless
   proxy rule and the watchdog, which reloads on network changes, handles captive
   portals, recovers a crashed or spinning router, and clears a stale proxy at login.
   Re-run `rowt doctor`; `NO_SUDO_RULE` should be gone. If they'd rather not run the
   watchdog, then every command that sets the proxy (`rowt up`, `rowt proxy on`)
   is theirs to run in their own terminal.
5. **Start** (agent; the user instead if `PORT_BUSY`). Run it in the foreground,
   output to a file, never piped or backgrounded (see Rules):
   ```sh
   rowt up host >| /tmp/rowt-up.out 2>&1; echo "exit=$?"; tail -20 /tmp/rowt-up.out
   ```
   `host` skips auto-detection. A bare `rowt up` picks local mode when Google answers
   direct, and otherwise probes host vs vm. vm mode is for corp networks that forbid
   binding to the NIC; `rowt probe` decides with the corp VPN up.
6. **Verify through rowt** (agent). Name the port explicitly: `rowt run` would take
   the old VPN's path and pass falsely.
   ```sh
   curl -sS -o /dev/null -w '%{http_code}\n' -m 10 -x http://127.0.0.1:7890 https://www.google.com/generate_204  # 204
   curl -sS -o /dev/null -w '%{http_code}\n' -m 10 -x http://127.0.0.1:7890 https://<your API host>/           # not 000/403
   rowt explain <your API host>            # → escape
   rowt status; rowt proxy check
   ```
   A **403 through 7890** means the active server exits in a region your API refuses.
   Run `rowt use <another tag>` and re-test; don't let the user switch over until one
   passes. Then run `rowt ping` and let the user choose `rowt use <tag>` (pinned,
   never probed) or `rowt use auto` (the fastest live server; it moves off a dead
   one). With `auto`, re-test your API host too.
7. **Switch over** (the **user**, once you've handed over the resume command). If
   `AGENT_VIA_VPN`, or the proxy env points at another app, say first: "when you turn
   off <client>, this session drops; restart it with
   `cd <this session's working directory> && rowt run claude -c`." Spell out the
   actual directory: `-c` resumes the latest session *in that directory*. Use your
   agent's own resume flag; `rowt run` finds the working path, which by then is rowt.
   The user then quits the old client and connects the corp VPN, if any. The watchdog
   reloads by itself (without it, run `rowt reload`). Afterwards, re-run `rowt doctor`
   (the old client's flags should be gone), then `rowt proxy check` (and
   `rowt proxy on` if the old client cleared the proxy on quit), then `rowt status`.
8. **Corp lane** (mostly automatic). The corp network's DHCP search domains and the
   corp VPN's routes are mirrored into the corp lane by `rowt corp sync`, which the
   watchdog runs on connect. On the corp network, show the user `rowt corp suggest`
   and add anything missing with `rowt corp add <suffix|CIDR>`. You may propose
   suffixes from the company name, but confirm first. For Tailscale, add `tailscale`
   to `~/.config/rowt/sync-ifaces.txt`.
9. **Finish.** Ask before `rowt shell-init --install`, which edits `~/.zshrc` to add
   `rowt-proxy-on/-off`, the tailnet helpers and completion. Mention `rowt run <cmd>`
   for CLI tools, which ignore the system proxy. The user opens `rowt monitor` in a
   new terminal; it's a TUI, so it isn't yours to drive. Re-run `rowt onboard` until
   every box is ✓.

## Rules when operating rowt

- **Run `up`, `reload`, `restart`, `down` and `router …`, and lane `add`/`rm` (they
  restart), in the FOREGROUND, with output redirected to a file:**
  `rowt reload >| /tmp/rowt-out 2>&1; echo "exit=$?"; tail -15 /tmp/rowt-out`.
  The shell here may be zsh with `noclobber`, hence `>|`. **Never background them.**
  sing-box and its log splitter stay in the launcher's process group, so a harness
  that kills a timed-out task's group kills the router. **Never pipe them.** On rowt
  ≤ 3.5.8 the pipe never closed and the caller hung, although the change had already
  applied. They finish in well under 30 s.
- **There is no hot reload.** Lane `add`/`rm` restart the router themselves. After
  editing a lane file by hand, run `rowt reload`.
- **Never type or ask for a password**, and never kill the user's other apps. Admin
  steps go to the user's own terminal.
- **Don't change what the user chose**: the pinned server, their lane entries, the
  corp client's PAC, the corp VPN. Recommend a change instead.
- **Secrets:** `servers.json`, `manual.json`, `subs.txt` and `import-review.json` in
  `~/.config/rowt/` hold credentials and subscription tokens. Never print, paste or
  send them. `rowt report` writes a masked, shareable diagnostic, and `rowt config
  export` output needs an encrypted channel.
- **A red `● ERROR` is a synthetic probe, not proof of an outage.** Confirm with a real
  fetch through 127.0.0.1:7890 or with `rowt connections`.
- **Measure CPU over an interval**: compare `ps -o time=` about 15 s apart. Never use
  `ps %CPU`, which is a decaying average.

## Everyday: see `references/everyday.md`

| need | command |
|---|---|
| is it working | `rowt status` · `rowt proxy check` · `rowt connections [lane]` |
| why that lane | `rowt explain <domain\|ip>` · `rowt escape errors` / `rowt direct errors` (candidates for escape) |
| route a site | `rowt escape\|corp\|block\|hotspot add <entry>` (one lane per entry) · `geosite:<name>` (escape and block only) |
| after a network change | automatic with the watchdog; otherwise `rowt reload` |
| switch server | `rowt ping` → `rowt use <tag>` / `rowt use auto` |
| a venue's login page | automatic with the watchdog; recurring venue: `rowt hotspot add <portal-host>` |
| abroad, no firewall | `rowt up local` (back: `rowt up host`) |
| CLI tools | `rowt run <cmd>` · `rowt proxy env` · `rowt-proxy-on` (from shell-init) |
| watch it live | `rowt monitor` (user's terminal) · `rowt metrics top` |
| move or share the setup | `rowt config export [--routes-only]` → `rowt config import <file>` (merges; `--replace` overwrites) · to an iPhone: `rowt config export --to shadowrocket` |
| stop / remove | `rowt down` · `rowt uninstall [--purge]` then `brew uninstall rowt` |

Everyday details, the monitor keys, tailnet sharing and geosite are in
`references/everyday.md`.

## Troubleshooting: see `references/troubleshooting.md`

Start with `rowt doctor` plus `rowt status`, and read `~/.config/rowt/log/` (`host.log`,
`lane-*.log`, `watch.log`, `captive.log`, `audit.log`). Then find the symptom in
`references/troubleshooting.md`:

- router down or not answering;
- a site on the wrong lane;
- corp names failing through the proxy;
- traffic bypassing rowt (a PAC);
- a lane that is reachable but too slow (`rowt speed <url>`);
- a hot, spinning sing-box;
- a known-bad engine;
- a stale watchdog;
- a login page that never loads.

Working on rowt's own code or releases? That's `CLAUDE.md` in the rowt repository,
not this skill.
