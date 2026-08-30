# rowt bug report — corp lane throughput collapse on large transfers

**Reported:** 2026-08-24 (PDT) · **rowt:** running, pid 61570, `127.0.0.1:7890`
**Status:** RESOLVED 2026-08-29 — see the Resolution at the end. rowt was not the cause;
`rowt explain` and a new `rowt speed` were fixed/added in 3.4.11.
**Severity as reported:** high — makes any multi-MB corp download impossible, and the failure mode
misleads the caller.

## Symptom in one line

Traffic routed to the **corp** lane sustains ~150–220 KB/s, while the **direct** lane on
the same machine at the same moment sustains ~40 MB/s. Small responses complete fine, so
the lane looks healthy; anything multi-MB never finishes and dies on the client's timeout.

## Measurements

All taken within a few minutes of each other, same Wi-Fi, same shell.

**Corp lane** — `git.cn-hangzhou.oss-cdn.aliyun-inc.com` (44,515,696-byte object):

```
recv=2065303  speed=206489 B/s  connect=0.175s  firstbyte=0.998s
recv=1591959  speed=159095 B/s  connect=0.173s  firstbyte=0.965s
recv=2187247  speed=218608 B/s  connect=0.183s  firstbyte=1.030s
```

**Direct lane** — `speed.cloudflare.com`, 20,000,000 bytes, same moment:

```
recv=20000000  speed=39972339 B/s        # completed, ~40 MB/s
```

**≈200× difference.** Connect (0.17s) and time-to-first-byte (~1s) are both healthy on the
corp lane, so this is not DNS, TLS, or connection setup — it is **sustained transfer
throughput** that collapses after the response starts flowing.

Small responses on the *same corp host* complete normally (latency-dominated):

```
git.cn-hangzhou.oss-cdn.aliyun-inc.com/aone-cli/manifest.json   1,346 B  → OK
artlab.alibaba-inc.com/1/generic/.../artifact.tar.gz?version=…  5,016 B  → OK
```

That is why the lane appears fine in normal use — `rowt status`, health probes, and API
calls are all small.

## What it looks like to the caller

The server is not at fault. Across repeated requests the origin is byte-identical:

```
Content-Length: 44515696
ETag: "526C35CC7BDD5CD0626491E772E9CC42"
x-oss-hash-crc64ecma: 2411704504658111427
```

But every download stops early, at a *different* offset each time, purely by wherever the
client's timeout lands:

```
curl: (28) Operation timed out after 90004 ms with 20651903 out of 44515696 bytes received
curl: (28) Operation timed out after 20005 ms with  2981798 out of 44515696 bytes received
```

**The dangerous part:** `%{http_code}` is **200** and `%{size_download}` is a plausible
non-zero number, because the headers arrived fine and the body simply stopped. A caller
that checks only the status code — or that runs curl with `-s` and reads `-w` output
without checking the exit status — sees a successful 200 with truncated bytes.

I hit this myself and briefly concluded a corp artifact CDN was serving mutable artifacts,
because repeated "successful" downloads of one immutable versioned URL produced different
SHA256s each time. They were just truncated at different offsets. **curl does signal the
failure (exit 28); it is easy to lose that signal, not absent.**

Anything that pipes a download straight into a hash, a tar, or a package manager will
produce a corrupt result that looks like an integrity failure at the origin.

## Environment

```
rowt status
  mode:      local  (no tunnel — the escape lane routes direct)
  router:    running (pid 61570) on 127.0.0.1:7890
  buckets:   escape=260 corp=141 block=26+ads +geosite:github,google,meta,x final=direct
  via proxy: HTTP 200 (google, direct)

rowt explain git.cn-hangzhou.oss-cdn.aliyun-inc.com
  ->  CORP   (into the corp VPN via the OS route)
  matched: longest-match corp-domains suffix 'aliyun-inc.com'
  live:    HTTP 403 through the router
```

Two things worth a look here:

1. **`mode: local` — no tunnel is up.** The escape lane routes direct. Whether the corp
   lane is meant to work in this mode, and what it actually falls back to when the corp
   VPN route is absent or degraded, is the first thing to check.
2. **`rowt explain` reports `live: HTTP 403 through the router`** for a URL that returns
   **200** to plain curl from the same shell. The router's own probe and the actual data
   path disagree, which may be a second bug or may be the same one seen from another angle.

## Reproduction

```bash
# Corp lane — expect a stall and exit 28
curl -sS -o /dev/null -w 'recv=%{size_download} speed=%{speed_download}\n' --max-time 20 \
  https://git.cn-hangzhou.oss-cdn.aliyun-inc.com/aone-cli/v0.2.53/a1-darwin-arm64

# Direct lane, same moment — expect completion at tens of MB/s
curl -sS -o /dev/null -w 'recv=%{size_download} speed=%{speed_download}\n' --max-time 20 \
  "https://speed.cloudflare.com/__down?bytes=20000000"
```

The corp URL is anonymously readable, so no credentials are needed to reproduce.

## Questions for whoever picks this up

- Is the corp lane usable at all in `mode: local`, or should rowt refuse/warn when corp
  traffic is requested with no tunnel up?
- Is throughput being shaped somewhere in the corp path (a buffer size, a single-threaded
  relay, a per-connection window), or is this the upstream corp route genuinely being slow?
- Should `rowt explain`'s live probe use the same path as real traffic? Its 403 vs the
  actual 200 suggests it does not.
- Worth considering: a `rowt` diagnostic that measures **sustained throughput per lane**,
  not just reachability. Every existing health signal is small-response and therefore blind
  to this failure.

## Relevance to FUTURE.md

[FUTURE.md](FUTURE.md) sketches an agent that senses the environment — *"is corp VPN up,
what network am I on, is `bind_interface` filtered, is escape reachable"* — and sets the
routing posture automatically. This bug is a concrete data point for that design, and it
suggests the sensing set is missing a dimension:

- Every signal rowt currently has is **reachability** (does a small request succeed).
  This failure is invisible to all of them: the corp lane returns 200 on small responses
  while being unusable for real transfers.
- FUTURE.md's invariant *"fail closed"* is arguably violated here. Corp traffic in
  `mode: local` with no tunnel does not fail — it succeeds slowly enough to corrupt
  callers that do not check byte counts. Failing closed would be safer than 150 KB/s.
- A throughput probe per lane would feed the sensing loop directly, and would also give
  the auto-posture agent a reason to switch modes rather than only a reason to warn.

## Not in scope but worth knowing

The symptom is easy to misattribute to the *origin* rather than the transport. If anyone
reports corrupt downloads, mismatched checksums, or "the artifact changed", check
`Content-Length` against bytes received and the client's exit status before suspecting the
server.

---

## Addendum 2026-08-25 — uploads are affected too, and SSH is not

The original report covered downloads only. New measurements while publishing
build artifacts to `code.alibaba-inc.com`:

| Operation | Transport | Throughput | Outcome |
|---|---|---|---|
| Git LFS push, 12 MB object | HTTPS | — | **incomplete after 10 min**, killed |
| plain `git push`, same 12 MB payload | SSH | **1.65–1.81 MiB/s** | completed in ~11 s |
| `curl` download, corp host | HTTPS | 206 KB/s | stalls (as reported above) |
| `brew` clone/fetch of that 12 MB object | SSH | ~110 KB/s effective | completed in 1:49 |

One caveat on that table before reading too much into it: the `brew` row measures
wall-clock for a full clone-and-checkout (negotiation, pack resolution, checkout),
not raw transfer, so its effective rate understates the link. The clean comparison
is the LFS push vs the plain `git push` — identical payload, same host, minutes
apart, ~200x apart in outcome.

**The split appears to be HTTPS vs SSH, not upload vs download.** Git LFS moves
objects over HTTPS even when the git remote is SSH — that is why an LFS push
crawls while a plain `git push` of the same bytes over SSH is ~9× faster and
finishes. `artifact.alibaba-inc.com` also answers in ~0.5 s, so not every corp
host is affected.

If that split is real, it narrows the search: something in the HTTPS corp path
(the router's own proxying) rather than the corp route as a whole. The SSH numbers
are a useful control — same host, same moment, same network, two orders of
magnitude apart.

### One more diagnostic suggestion

Worth measuring **per-transport**, not just per-lane: a `rowt` check that pushes
and pulls a few MB over both HTTPS and SSH to the same corp host would have
isolated this immediately. Every existing health signal is small-response HTTPS
and is blind to both the throughput collapse and this asymmetry.

---

## Resolution 2026-08-29 — rowt is not the cause; two real bugs fixed alongside

Investigated with the corp VPN up, by adding the one measurement the original
report never took: **the same object with the proxy bypassed.**

```
through rowt      226 KB/s   (213, 212 on repeats)
--noproxy '*'     253 KB/s   (208, 177 on repeats)
direct lane        32 MB/s through rowt, 44 MB/s without
```

**Removing rowt from the path entirely does not help.** The collapse happens with
the router out of the picture, so it is not rowt's relay, its buffering, or its
proxying. The control confirms rowt itself is healthy: the direct lane moved 32
MB/s *through the same router* at the same moment — a normal proxy overhead, not
a 200x one.

Everything corp-lane rides `utun11` (the corp VPN tunnel, **mtu 1294**), and that
tunnel sustains ~200 KB/s for bulk transfer while the same Wi-Fi does 40 MB/s.
Small responses finish inside a couple of round trips, which is exactly why the
lane looks healthy to every reachability check.

Every measurement in the original report reproduced. What was wrong was the
**attribution**: rowt was in the path, so rowt was blamed, and the with/without
comparison that would have exonerated it in one command was never run. That is
the lesson worth keeping — the addendum's HTTPS-vs-SSH split is consistent with
tunnel behaviour and needs no rowt bug to explain it.

### Not a bug either: silent truncation of rowt's own downloads

Tested directly: `curl -f` exits **28** on a truncated body, and every download
path in `bin/rowt` (`fetch_ads_srs`, `fetch_geosite`, the sing-box and image
fetches) checks that exit status and deletes the partial file. rowt cannot be
made to accept a short body as success.

The hazard in the report is real, but it lives in **hand-rolled `curl` calls**:
`%{http_code}` is 200 and `%{size_download}` is a plausible number, so a caller
reading only those sees success. curl does signal it (exit 28); it is easy to
lose that signal, not absent. Check `Content-Length` against bytes received.

### What WAS rowt's, and is now fixed (3.4.11)

**1. `rowt explain` reported an origin status as a lane verdict.** It printed
`live: HTTP 403 through the router`, which reads as "the lane is broken" — and is
what pointed this investigation at rowt in the first place. The probe fetches the
**site root**, because routing is per-host and `explain` has already dropped any
path. So the status is the origin answering about `/`:

| host | root | meaning |
|---|---|---|
| `git.cn-hangzhou.oss-cdn.aliyun-inc.com` | 403 | bucket listing denied — objects under it serve 200 |
| `gitlab.alibaba-inc.com` | 301 | redirect to a login path |
| `artifact.alibaba-inc.com` | 200 | serves a root page |

All three prove the same thing: the request crossed the lane and came back. Only
`000` means it did not. `explain` now says exactly that instead of printing a bare
status that invites the wrong reading.

**2. `rowt speed [url …]` — the missing signal.** Every other check rowt had was
reachability, so all of them were blind to this failure class by construction. It
fetches a capped range through the router **and** with the proxy bypassed:

```
  URL                          VIA ROWT     BYPASSED     LANE
  git.cn-hangzhou.oss-cdn…     189 KB/s     220 KB/s     corp
  speed.cloudflare.com…        31.9 MB/s    32.3 MB/s    escape
```

Both columns slow → the transport is slow, rowt is not in the way. Only the
proxied column slow → the router's relay. Both fast → the lane is healthy for
bulk transfer. The entire investigation above, in one command.

### Answering the report's own questions

- **Is the corp lane usable in `mode: local`?** Yes — by design. The point is to
  not tunnel traffic that the corp VPN already reaches directly. Ruled 2026-08-29.
- **Is throughput shaped somewhere in the corp path?** Not by rowt. The bypassed
  measurement settles it; the tunnel is the bottleneck.
- **Should `explain`'s live probe use the same path as real traffic?** It already
  did — both went through the router. The 403-vs-200 was root-vs-object, now
  stated plainly.
- **A throughput diagnostic per lane?** Built: `rowt speed`.

### Still open

The **fail-closed** question the report raises is a fair one and is NOT resolved
here. Corp traffic over a tunnel doing 200 KB/s does not fail — it succeeds slowly
enough to corrupt callers who do not check byte counts. Whether rowt should warn
(it now can: `rowt speed`) or refuse is a design decision, and it is the kind of
sensing FUTURE.md's auto-posture agent would need. Left for that work.

The corp tunnel's own throughput is outside rowt entirely — a question for whoever
runs the VPN, with `mtu 1294` and per-connection shaping as the places to look.
