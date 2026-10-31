# rowt: troubleshooting

## First look

1. `rowt doctor` (what's around rowt) and `rowt status` (rowt itself). The status
   lines cover:
   - `router:` running or not;
   - `engine:` pinned, or KNOWN BAD;
   - `system proxy:` and `config:`;
   - `escape:` the lane's health as rowt last observed it (the watchdog's probe every
     120 s, the follower's test every 15 s): `ok`, `degraded` (failing but under the
     watchdog's limit, or slow), `holding` (a dead pinned server; the row names the
     remedy), `DOWN` (the streak reached the limit, no server answered the follower,
     offline, the router stopped, a captive portal), with since when and the last ok.
     `probed now:` means nothing fresh was observed, so status made one try of the
     watchdog's delay test itself. `members:`
     is each server's last delay test. The same as JSON: `rowt status --json`
     (`status-json.md`), `.escape.state` and `.escape.reason`;
   - `via proxy:` (with `--probe`) a real fetch through the escape lane, 8 s budget.
     `HTTP 000 … no answer within 8s` is a slow link or a dead tunnel (check
     `rowt connections` for live traffic before concluding which); `tunnel not
     reachable` failed outright;
   - `uplink:` (with `--probe`, after a failed `via proxy:`): the same path with the
     router out of it — the captive-probe host fetched unproxied over the physical
     NIC, then a ping at that NIC's gateway. It names one of three faults: the
     **tunnel** (`rowt restart`, or another server), a **login page** (`rowt proxy
     off`, log in, `rowt proxy on`), or the **network itself** (nothing answers over
     the NIC, or only its gateway does). A restart cannot fix the last one — rejoin
     the Wi-Fi or switch networks. 2026-10-26: a corp Wi-Fi subnet with no exit read
     as a dead tunnel through eight watchdog reloads and two restarts;
   - `watchdog:` `loaded`, `NOT loaded — … rowt watch refresh`, or `not installed`,
     and whether its plist is STALE.
2. Logs in `~/.config/rowt/log/`:
   - `host.log`: start-up and config errors;
   - `lane-escape|corp|direct|block.log`: per-lane connection errors;
   - `watch.log`: what the watchdog did;
   - `discovery.log`: what each network or VPN advertised, one line per change;
   - `captive.log`: why a portal probe returned what it did;
   - `audit.log`: every state change and who made it.
3. `rowt report` writes a masked diagnostic the user can share.

When the router is down, run `rowt up` in the foreground; it's idempotent.

## Symptom → cause → fix

**Router won't start.** `rowt up` says "router did not come up healthy" with a log tail.

- `bind: address already in use`: `rowt doctor` shows the owner of 7890/9090 (usually a
  Clash client). Never kill it: the user quits it, or sets `ROWT_PORT` /
  `ROWT_CLASH_PORT` to free ports.
- A config error: `rowt reload`. If it persists, check `host.log`.

**"It hung", but the change applied.** You piped `up`, `reload` or a lane edit on rowt
≤ 3.5.8. The log splitter held the pipe's write end, so the reader never saw EOF. The
router is fine. Don't kill the task: on rowt ≤ 3.6.6 that kills the router's process
group too. Run one clean foreground `rowt reload` with output redirected to a file; the
old router pair exits and the stuck reader completes. Fixed in 3.5.9.

**Router died with your task, your terminal, or a daemon restart — on rowt ≤ 3.6.6.**
sing-box, its splitter and the collector stayed in the process group of whatever ran
`up`/`reload`/a lane edit, so anything that killed that group killed them: a harness
ending a timed-out task, or launchd cleaning up a daemon that an SSH command ran under
(a lane edit over Tailscale SSH starts them inside tailscaled's group; a Tailscale
update then took the router down with no log line). Since 3.5.9 sing-box ignores
SIGHUP; since 3.6.7 the three start in a session of their own, so no group signal
reaches them. On an older rowt, run those commands in the foreground and never over
a daemon's SSH; the watchdog restarts a dead router within its 120 s tick either way.

**A server named by domain stops answering, or `rowt status` shows `escape dns:` with REFUSED or DISAGREE.**
rowt resolves escape servers' names itself over DoH, asking both AliDNS and DNSPod. It
refuses answers no public server has (private, loopback, link-local, CGNAT, reserved,
fake-IP `198.18.0.0/15`, plus the box's own `refuse` ranges in
`~/.config/rowt/escape-dns.txt`). When the two sources DISAGREE it believes neither,
and an answer that differs from the last good address is not believed either: the
new addresses are fetched through the server itself, and only one that answers is
used (`MOVED`); otherwise the last good address stays (`held`), else the system
resolver's. The status
lines name each refused address and each disagreement, and what sing-box was given.
A disagreement on a network that hijacks DNS is the check working. A server that
really lives on a private or tailnet address behind a name needs an `allow <name>
<cidr>` line there (or an address literal); `off` turns the check off. Put a sinkhole
you have seen here in `refuse` only on this box. When a provider moves its entry
addresses, the watchdog follows within about 5 minutes with no restart once both
sources agree; `ROWT_ESCAPE_DNS_MAX_AGE=0 rowt render` asks again at once, and the
running router picks the new address up by itself.

**Fan loud, battery draining, everything green.** This is the spin: sing-box at 100–200%
CPU with little traffic.

- Measure over an interval: `ps -o time= -p <pid>` twice, about 15 s apart. Never trust
  `ps %CPU`.
- The watchdog restarts a spinning router once and leaves evidence in
  `log/spin-*.txt`. `rowt restart` clears it by hand.
- To diagnose the next one, run `ROWT_PPROF=9091 rowt reload`, then
  `curl -s 'localhost:9091/debug/pprof/goroutine?debug=2'`.

**`engine: sing-box 1.14.x — KNOWN BAD`.** 1.14 spins behind corp EDR network filters,
which kill idle UDP DNS sockets. `rowt fetch host` or any `rowt reload` swaps in the
pinned 1.13.x; a bare `restart` only helps for seconds. The corp resolver is UDP-only,
so "use DoH for corp" is not an option.

**A red `● ERROR` in status or the monitor.** That's a synthetic probe of the active
server; some servers fail it while carrying real traffic. Verify with
`curl -x http://127.0.0.1:7890 https://www.google.com/generate_204` or
`rowt connections escape` before concluding anything.

**A site goes the wrong way.**

- `rowt explain <domain>` names the lane and the entry that decided it. The longest
  suffix wins across lanes, and an exact `domain:` entry beats all suffixes.
- Failures are listed per lane: `rowt escape errors`, `rowt direct errors` (escape
  candidates), `rowt block errors` (what got blocked).
- Fix with a lane `add`. A running router picks it up live (3.8.10+; earlier
  versions restart, so run it in the foreground there).

**Escape lane: many timeouts or resets.**

- If every error points at the same server port, the path to that server is the
  problem, not rowt.
- Check with `rowt ping`. A pinned server is the user's choice, so recommend
  `rowt use <faster>` or `rowt use auto` rather than switching it yourself.

**Corp names fail through the proxy but resolve with `dig`/`ping`.**

- On rowt < 3.1.1, connection lookups used public DoH, which SERVFAILs internal zones.
  Fix: `brew upgrade rowt && rowt reload`.
- From 3.1.1, corp names resolve through the system resolver at connect time.

**A corp host is reset through the proxy but loads with no proxy.**

- It is on a suffix nobody added, so it went `direct` (bound to the physical NIC).
- `rowt explain <host>` says which case it is. If the router resolves it into a corp
  CIDR, it already routes corp (rowt 3.6.3 and later; before that, add the suffix).
  If it "does not resolve at" the router's resolver, it is split-horizon: run
  `rowt corp add <suffix>`.

**Corp host unreachable while on the corp network.**

- `rowt explain <host>`: is it corp?
- `rowt corp sync --dry-run`: are the VPN's routes mirrored?
- Timeouts while the corp VPN is down are expected.
- If host mode can't reach the corp network at all, the corp network may forbid
  binding to the NIC. `rowt probe` with the corp VPN up decides between host and vm.

**Everything is configured, yet traffic doesn't go through rowt.**

- An auto-proxy (PAC or WPAD), usually set by the corp VPN client, outranks the manual
  proxy. `rowt proxy check` and `rowt status` name it (3.4.14+).
- rowt won't disable it, and you shouldn't propose that it does. The user turns it
  off, or uses `rowt proxy env` / `rowt run` for CLI tools.

**Reachable but unusably slow** (big downloads die, small requests work).

- `rowt speed <url>` measures sustained throughput per lane, proxied and bypassed, and
  states a verdict per row; don't compare the columns yourself. "not rowt" is usually
  the answer. A corp VPN doing about 200 KB/s over 40 MB/s Wi-Fi was a real case: the
  tunnel's MTU was at fault.
- rowt never reroutes a lane for being slow: the lane is what the user's rules chose.

**A hotspot login page never loads.** Ask for the portal's hostname, then
`rowt hotspot add <host>`. `captive.log` shows what the watchdog saw.

**The escape lane is down and `rowt status` has an `escape:` row saying the pinned
server is dead.** The server behind `rowt use <tag>` does not accept a connection
from this machine at all (the watchdog tried a direct TCP connect from the NIC), so
a reload would only restart the router onto the same dead server; the watchdog
holds instead and writes one line per tick to `watch.log`. Fix: `rowt use auto`
(the fastest live server, with hot failover from then on), or pin a live one with
`rowt use <other>` (`rowt server` lists them, `rowt ping` measures them). The row
clears when that server answers again or the selection moves.

**The watchdog does nothing.** `rowt watch status` shows the state and recent log.
`NOT loaded` or STALE is fixed with `rowt watch refresh`. A missing sudoers rule is
fixed by the user running `rowt watch install` in their own terminal.

**The agent lost its connection after the switch-over.** The session was riding the old
VPN. Resume through rowt from the same directory — `-c` continues the latest session
there — with `rowt run claude -c` (or your agent's resume flag).
Check that the API host is in escape: `rowt explain api.anthropic.com`.

**First `up` in China fails to fetch rule-sets.** It needs a path to GitHub. Keep the
old VPN on and run `rowt fetch host`. The sing-box engine itself is bundled; nothing
is downloaded for it.
