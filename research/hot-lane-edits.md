# Lane edits without a restart — the lane rules as local rule-sets

*Design, 2026-10-31. Hong said build it the same day (#s53, see 4.11), and it
was built on #s54 (rowt 3.8.10 and later); 4.1 and 4.2 note where the build
departs from this text. It covers rowt 3.8.8 (main at 52c0dcc when written; PR #5 merged
since, main at e833881) with sing-box pinned at 1.13.14, and was written on
rowt #s52 after contributor PR #5 (#s47) showed the mechanism for the corp lane. The question
it answers: can `rowt escape|corp|block add|rm|import|clear`, and `corp sync`'s
DHCP domains, reach the running router without a restart — while keeping
today's precedence between the lanes?*

Every claim carries a tag. **[P]** is verified from a primary source: sing-box
source or docs, a measurement with the bundled binary on this Mac (2026-10-31),
or the downstream fork's measurements on its own task (2026-10-30). **[I]** is an inference
from rowt's own code, which is quoted where it matters.

## 1. The problem

Every lane edit ends in `reload_if_running`: a render, then `router restart`.
The restart drops every connection on the Mac — escape and direct alike, not
only the lane that changed — and the monitor's batched edits exist only to pay
that price once instead of per entry. The lanes are edited often: on this Mac
the escape lane holds 261 suffixes and 4 geosite references, block 493
suffixes, corp 13 (counts only). PR #5 removed the restart for one source of
change, `corp sync`, by moving the corp CIDRs and the synced DHCP search
domains out of host.json into two local rule-set files that sing-box swaps in
place. It could not do the same for the hand lanes, because one set per lane is
one rule at one position, and the lanes depend on an ordering between entries,
not between lanes (§3). This design keeps that ordering.

## 2. What sing-box gives us

- **A local rule-set is a file, referenced by tag.** `format: source` is JSON:
  `{"version":1,"rules":[{"domain":[…]},{"domain_suffix":[…]}]}`. A route rule
  names one or more sets (`rule_set: ["a","b"]`, any match) and carries the
  outbound itself; a DNS rule names sets the same way and carries the server.
  **[P]** for the syntax: PR #5 renders both shapes with one-element lists,
  and this design's DNS rule over nine sets passes `check` (below). **[I]**
  for the semantics — that a list of sets matches on any of them, as every
  list field in a sing-box rule does; `rule-set match` takes one set, so the
  OR cannot be measured offline, and the live test in §4.10 covers it.
- **The file is watched, and a replaced file swaps the rules in place, same
  process.** **[P]** sing-box 1.13 `route/rule/rule_set_local.go` (the PR's
  source reference); the fork's agent measured on this Mac: a set replaced by rename is
  in use in 0.3 s, same pid; a long stream opened before the swap keeps going;
  a bad set is logged and the old rules kept; a VirtioFS share delivers no
  event at all (`~/.config` is never one).
- **`sing-box check` loads set contents.** **[P]** Measured here: a config with
  27 lane sets (24 of them empty) and one DNS rule over nine sets passes, and
  so does the built shape, 49 sets and one DNS rule over sixteen;
  truncate one set file and it fails with `parse rule-set[22]: unexpected
  EOF`; a set holding an odd line (`a.b.example/x`) passes — an entry's
  validity stays rowt's job, exactly as with inline rules.
- **`domain_suffix` matches on a label boundary.** **[P]** Measured here with
  `sing-box rule-set match` on a set holding `a.a.example`: `x.a.a.example`
  and `a.a.example` match, `xa.a.example` and `b.a.example` do not. PORTING.md
  §6.7's 2026-08-13 row measured the same on 1.13.14. §4.1 rests on this.
- **Not offered:** a hot change to outbounds, inbounds or DNS servers. SIGHUP
  reloads the whole config inside the same process (rowt's `port add` already
  uses it) but closes every connection, since the outbounds are rebuilt. **[P]**
  `bin/rowt`, `_port_apply`'s comment and its "open connections were reset".

## 3. Why one set per lane is not enough

The render flattens the three hand lists into (suffix, lane) pairs and emits one
rule per suffix, sorted by label count and then string length, longest first —
so `api.foo.example` in escape beats `foo.example` in block — with the exact
(`domain:`) entries before every suffix, in lane order escape/corp/block, and
the `geosite:` sets, the ad set, the resolve rule, the corp CIDRs and the
private ranges after. **[I]** `assemble_host`'s `$suffix_rules` and
`$exact_rules`; `render::suffix_rules`/`exact_rules` in rowt-core mirror them.

A rule-set rule is one position in that list. "All escape suffixes" as one set
and "all corp suffixes" as another makes lane order the precedence, which is
the trade-off PR #5 accepted for the synced DHCP domains alone (they rank after
every hand suffix). For the hand lanes that would change what `explain`
promises and what people have built their lists on.

## 4. The design: one set per lane per label count

### 4.1 Why that keeps the ordering

For a host `h` and a label count `N` there is exactly one candidate suffix:
the last `N` labels of `h`. That is what label-boundary matching means (§2), so
`h.x.a.example` can match `x.a.example` (3 labels) and `a.example` (2) but
no *other* 3-label suffix. Two suffix entries with the same label count can
therefore both match one host only if they are the same suffix — and rowt's
single-lane rule already forbids one entry living in two lanes, either spelling
(`.x.example` and `x.example` are the same entry to `add`, `rm` and the
render; the sets store the bare form). One hole to close on the way: `add`
strips whitespace but does not lowercase, and `_lane_has` compares exactly, so
`Example.com` in one lane and `example.com` in another can both be stored
today — and then rank by whichever lane the render saw first, since they tie
on label count and length. DNS names are case-insensitive, so the
implementation makes `add`, `rm` and `_lane_has` compare case-insensitively
on both sides, the set writer lowercases, and the render warns on a pair a
hand edit left behind. With that, "the same entry" means the same thing to
the single-lane rule and to the sets. So:

- within a label count, the order between lanes never decides anything;
- between label counts, "more labels first" is the whole of today's sort —
  the string-length tie-break only ever ordered rules that could not both
  match;
- exact entries are whole-host equality, at most one match per host; they
  stay first, in lane order escape/corp/block, which is what `explain`'s
  `_longest_domain_hit` and `classify::longest_domain_hit` encode today.

`explain` and `route` therefore keep every verdict; the one change to their
code is comparing entries lowercased, as the sets hold them (4.2). **[I]**

**As built — one correction.** A dot-led entry `.z.example` (the names under
z.example) splits to three like the bare `x.z.example`, and both can match one
host; inline, the bare one — always the longer string — won. One set per lane
per split count would let lane order decide that pair, so each band has a bare
set and a dot-led set per lane, bare first: 3 exact + 3×8 bare + 3×7 dot = 48
lane sets. The argument above holds within each of the two kinds.

### 4.2 The sets

Under `<config dir>/rule-sets/`, one directory per set (`<tag>/<tag>.json`,
below), mode 0600 like PR #5's (corp names are sensitive):

    escape-exact.json  corp-exact.json  block-exact.json              key `domain`
    escape-1.json … escape-8.json, corp-1…8, block-1…8                key `domain_suffix`, bare entries
    escape-2-dot.json … escape-8-dot.json, corp-2…8-dot, block-2…8-dot  the dot-led entries
    corp-cidr.json                                                  key `ip_cidr` (PR #5's set, kept)

48 lane sets and the CIDR set, always all present; an empty one is `{"version":1,"rules":[]}`,
which `check` accepts (§2). Band `N` holds the lane's suffixes with exactly `N`
labels; band 8 holds eight or more.

**Why a fixed band count.** host.json names the sets, so a band count that
followed the lanes would change host.json on the first entry deeper than any
before — a restart, which is the thing being removed. **Why 8.** The live
lanes top out at 6 labels (block; escape at 4). The overflow band can only
rank wrongly when one lane holds an entry of 9+ labels and another lane holds a
suffix of it with 8+ labels; the render checks for that pair and warns on
stderr ("…ranks by lane order in band 8"), and I expect never to see it. Ten
bands cost three more files and nothing else; the number is a knob (§4.11).

**The synced DHCP domains** (PR #5's `corp-sync-dom`) join the corp bands by
label count. That removes PR #5's one ordering caveat: a synced domain then
ranks by depth like any hand entry. The corp CIDR set stays as PR #5 made it.

### 4.3 host.json

Route rules, first match wins:

    sniff
    rule_set [escape-exact] → escape   rule_set [corp-exact] → corp   rule_set [block-exact] → block
    for N = 8 … 1:  rule_set [escape-N] → escape,  [corp-N] → corp,  [block-N] → block,
                    then, for N ≥ 2, [escape-N-dot], [corp-N-dot], [block-N-dot] the same way
    the geosite rules, the ad set, the resolve rule, corp-cidr, the private ranges — as today

Lane order inside a band is free (§4.1); escape/corp/block is kept for
readability and for `explain`'s tie convention. Named ports keep splicing
their catch-alls before the resolve rule; nothing changes for them. The DNS
rules become one rule, `{rule_set: [corp-exact, corp-8, corp-8-dot … corp-1],
server: "local"}` **[P]** (measured: a DNS rule over sixteen sets passes
`check`). `route.rule_set` lists the 49 sets with their absolute paths, the
geosites and the ad set.

After this, host.json depends on: mode, the servers and the selection (auto
and its knobs), the ports, the `geosite:` references, whether the ad set is
cached, the private default, whether corp has any CIDR (the resolve rule, as
in PR #5), and the interface. **It is byte-identical across every lane edit
except a `geosite:` add or rm** (its rule and its `rule_set` definition live in
host.json; four of this Mac's escape entries are geosites, so that edit stays a
restart — §4.6).

### 4.4 Writing the sets

PR #5's `_corp_set_put` / `lifecycle::corp_sets_write`, generalised: for each
(lane, band), build the JSON from the lane file — `list_json`'s filtering and
order, the bare spelling, exact entries to the `-exact` set, `geosite:` lines
ignored — then write only when the bytes differ, to a temp file in the same
directory renamed over the set (the watcher only ever reads a whole file), the
previous file kept as `.prev` until the render's `sing-box check` passes and
put back when it fails. Both implementations byte-identical, which
`render-matrix` compares.

**Two files at once.** A lane move (an `add` that pulls the entry out of a
sibling lane) touches two sets. sing-box's watcher is per file, so there is no
multi-file atomicity — inherent, not fixable from here. The order should be:
write the gaining lane first, then the losing lane. Between the two renames
both lanes hold the entry and the band's rule order (escape, corp, block)
decides; after the second the new lane alone does. The window is one rename
long, milliseconds on APFS. **[I]**

### 4.5 Applying an edit

`edit_list`'s tail and `corp sync` end in the same function — PR #5's
`_corp_apply` / `lifecycle::corp_apply` generalised to `_lanes_apply` /
`lanes_apply` (`config import` also brings servers and ends in `after_import`,
so it restarts as before, §4.6):

1. render (which writes the sets as a side effect, as today's render writes
   host.json);
2. if host.running (PR #5's mark: the cksum of the host.json the router was
   started from) equals the fresh host.json's: `applied live through the lane
   rule-sets — router not restarted (pid N), no connection dropped`;
3. else restart as today, saying why: the running router predates live lane
   updates (the first edit after an upgrade), host.json differs (a `geosite:`
   entry came or went, or another change was pending), or `ROWT_LANES_HOT=0`.

`--no-reload` keeps its meaning — do not touch the router — so the monitor's
batched edit still works; it is no longer needed, since each edit is cheap, but
nothing forces it to change. The watchdog is untouched: its reloads are for
health, not edits, and a `corp sync` on a tick simply goes live.

### 4.6 What still restarts

Everything that is not a rule, because sing-box builds those at start:
`server add`/`rm`, a subscription update that changes servers, a `config
import` (it carries servers), `use auto` on or off and its
`--every`/`--tolerance`/`--interrupt`, `port add`/`rm` (already
an in-place SIGHUP, connections reset), mode switches and local mode, an
interface move (the direct outbound is bound to the physical NIC), the ad set
appearing in the cache, a `geosite:` add or rm, and the watchdog's recoveries,
where the restart *is* the fix. Whether `rowt reload` should use sing-box's
SIGHUP reload for the endpoint changes — same pid, proxy untouched, sub-second,
connections reset — is a separate decision with its own trade-off and is not
part of this design's go or no-go.

### 4.7 What does not change

`explain` and `route` verdicts (§4.1). `config export --to shadowrocket`,
which reads the lane files (its ranking of synced domains, #s49, becomes moot
once they rank by depth again). `rowt doctor`, `status --json`, the monitor
(the clash API). The lane files and their format: they stay the source of
truth, `config export`/`import` carry them, and the sets are rendered artifacts
like host.json. The hotspot lane is the OS bypass list, not a sing-box rule,
and is untouched. Nothing in rowt, the monitor or the fork's fleet reads the lane
rules out of host.json (checked on #s47).

**The contract that does change:** host.json's shape (49 rule-set references
and one DNS rule where inline lists were), a `rule-sets/` directory of 49 files, and
a lane edit under a running router no longer restarts it — its exit status and
its `added:`/`removed:` lines are as before; `==> reloading router` and the
`✓ running` line give way to the applied-live line.

### 4.8 Migration and rollback

The first lane edit after the upgrade renders the new shape, host.json differs
from what the router runs, and the edit restarts once with the message saying
so; every edit after that is live. `ROWT_LANES_HOT=0` forces the restart path
without changing the shape (one render shape, as PR #5 chose, so the goldens
and the sandbox stay single). It replaces PR #5's `ROWT_CORP_HOT`, which is in
the same unreleased train (3.8.9), so nothing depends on that name yet; the
watchdog plist passes the new knob through as PR #5 passes the old one. A
downgrade renders inline again and leaves the `rule-sets/` directory unused; `uninstall --purge` removes the directory as today.

### 4.9 Failure handling

- A bad lane entry: `add` validates text as it does now; a line that slips
  through matches nothing in a set, as it matched nothing inline (§2).
- A render that fails `check`: the sets go back to `.prev`. sing-box may have
  swapped the failing set in for a moment and kept its old rules (it logs the
  parse error); the restore swaps the good file back. As in PR #5.
- A crash between two set writes: the next render rewrites every set that
  differs; the writer is idempotent.
- A watcher that misses an event (a shared folder): the knob above forces the
  restart path; on APFS the event arrives (the fork's measurement).
- 49 watchers, one per set, each on its set's own directory — **as built**,
  because a watcher on macOS (kqueue) holds a descriptor for every file in the
  directory it watches, and 49 sets in one directory measured 2,550 open files
  against 100 with a directory each. The design's "one `fsnotify` watch per set
  **[I]** from the source reference; kqueue on macOS handles that number without
  a thought, but the live test below should confirm there is no startup cost.

### 4.10 Gates and parity

- `render-matrix`: every scenario's host.json golden changes shape. Regenerate
  deliberately and read the diff once; the change is the same in all of them.
- `cli-diff`: the config tree gains the `rule-sets/` directory, 49 files, all
  deterministic, with their 0600 modes compared. The #s50 restart cases under `use-auto-unchanged`
  (`escape add new.example`) keep restarting unless the scenario can say the
  router runs the rendered host.json — and host.running cannot be a static
  fixture, since the cksum covers the sandbox's own path. So `make_sandbox`
  gets a `host.running.self` marker that computes it after the `@CFG@`
  substitution, like `host.pid.self` makes a router. Then one case per lane
  verb takes the live path and the sandbox's `watch.log`/audit shows the
  applied-live line.
- `lanes-diff`, `classify-matrix`, `golden`: unchanged (lane files and
  verdicts).
- `selftest`: step 2 (moving an entry between lanes) still fires through
  `render-matrix`; add a step that drops one band's write and requires
  `cli-diff`'s config-tree compare to notice.
- The `rowt-render` shadow binary follows the Rust render.
- A live test outside the harness, as PR #5 did: a real router with a loopback
  far end and a long-lived stream, then `escape add`, `rm`, a lane move and a
  `corp sync`; the stream must survive each, and a new connection to the added
  name must take its lane within a second.

### 4.11 Choices, as decided

Hong, 2026-10-31, on #s53 ("yes, fold the PR#5 into this once we decide to
build it"; "Okay, merge now. Then build the new feature as designed after."):

1. Band count: 8. Not the owner's call after all: it only sets where the one
   overflow edge sits (a 9+-label entry in one lane under an 8+-label suffix
   in another ranks by lane order), the render warns when that pair appears,
   and no lane on either Mac has an entry past six labels.
2. The synced DHCP domains fold into the corp bands: PR #5's separate
   `corp-sync-domains.json` goes away, and with it its ordering caveat and
   #s49. PR #5 merged first (#s47, main at e833881), so the build replaces
   that one set and keeps its CIDR set, writer, `.prev` handling and
   `host.running` mark.
3. `rowt reload` by SIGHUP for endpoint changes: separate ask, not this one.

## 5. Cost and dependencies

About the size of PR #5: both renders (the jq in `assemble_host` and
`rowt-core::render`), the set writer and the apply step in both
implementations, `explain` untouched, goldens, the sandbox marker, docs
(DESIGN.md's corp-sync property becomes a lanes property; README; the skill's
everyday page, which tells agents a lane edit restarts). It builds on PR #5's
set writer, `.prev` handling and host.running, all on main since #s47 merged it.

## 6. Sources

- **[P]** sing-box 1.13.14, `route/rule/rule_set_local.go` — the local set's
  file watcher and in-place reload (cited by PR #5; the fork's measurements on
  its #s443, 2026-10-30: 0.3 s swap, same pid, a long stream survives, a bad
  set is logged and the old rules kept, empty sets pass `check`).
- **[P]** Measurements on this Mac, 2026-10-31, with the bundled sing-box
  1.13.14 in a scratch config directory (no router): `check` passes with 27
  lane sets (24 empty), 28 route rules and a DNS rule over nine sets; a
  truncated set fails `check` (`parse rule-set[22]: unexpected EOF`); an odd
  entry passes; `rule-set match` shows the label boundary.
- **[P]** PORTING.md §6.7, the 2026-08-13 dot-boundary row.
- **[I]** `bin/rowt`: `assemble_host` (the rule order), `edit_list` (the
  reload tail), `_port_apply` (SIGHUP), PR #5's `_corp_set_put`/`_corp_apply`;
  `crates/rowt-core/src/render.rs`, `crates/rowt-cli/src/lifecycle.rs`.
