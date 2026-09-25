# rowt: troubleshooting

## First look

1. `rowt doctor` (what's around rowt) and `rowt status` (rowt itself). The status
   lines cover:
   - `router:` running or not;
   - `engine:` pinned, or KNOWN BAD;
   - `system proxy:` and `config:`;
   - `via proxy:` a real fetch through the escape lane;
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
router is fine. Don't kill the task, because that kills the router's process group. Run
one clean foreground `rowt reload` with output redirected to a file; the old router
pair exits and the stuck reader completes. Fixed in 3.5.9.

**Router died with your task or terminal.** You backgrounded it, and the harness killed
the process group. Since 3.5.9, sing-box ignores SIGHUP, but SIGTERM or SIGKILL to the
group still kills it. Always run it in the foreground.

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
- Fix with a lane `add`. It restarts the router, so run it in the foreground.

**Escape lane: many timeouts or resets.**

- If every error points at the same server port, the path to that server is the
  problem, not rowt.
- Check with `rowt ping`. A pinned server is the user's choice, so recommend
  `rowt use <faster>` or `rowt use auto` rather than switching it yourself.

**Corp names fail through the proxy but resolve with `dig`/`ping`.**

- On rowt < 3.1.1, connection lookups used public DoH, which SERVFAILs internal zones.
  Fix: `brew upgrade rowt && rowt reload`.
- From 3.1.1, corp names resolve through the system resolver at connect time.

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
