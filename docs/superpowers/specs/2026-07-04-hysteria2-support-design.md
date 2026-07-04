# Design: hysteria2 support in rowt

- **Date:** 2026-07-04
- **Status:** Approved (brainstorming) — pending implementation plan
- **Scope owner:** rowt

## Problem

rowt only parses `vless://` and `anytls://` share links. Many airports now
default to **hysteria2** (a QUIC/UDP-based protocol). A real-world airport
subscription (`t0.wjkc66.vip`) returns **only** hysteria2 nodes, so
`rowt sub add <url>` imports **0 servers** and the tool is unusable with it.

Two distinct failures were confirmed:

1. **Parser gap.** `config/vless-parse.py` does not recognize `hysteria2://`, so
   `parse_many` skips every node.
2. **Subscription retrieval.** With rowt's own User-Agent (`rowt/1.0`) this
   airport returns **Clash YAML**, which `fetch_subscription` rejects outright
   (`vless-parse.py:213-216`). Only a **Shadowrocket-style UA** makes the airport
   return base64-encoded `hysteria2://` links. So even after adding a hy2 parser,
   `rowt sub add` still fails for this airport unless the fetch UA changes.

## Key fact that bounds the work

The tunnel/render/selector/`bind_interface`/vm-SOCKS/ping/route paths are all
**protocol-agnostic**: `group_jq` (`bin/rowt:589-603`) simply adds
`bind_interface` to whatever outbound dicts sit in `servers.json`, and
`bind_interface` is a common sing-box dialer field that applies to hysteria2's
QUIC dialer too. sing-box natively supports the `hysteria2` outbound type.

**Therefore the entire feature lives in `config/vless-parse.py` plus docs and one
test. `bin/rowt` is not modified.**

## Decisions (from brainstorming)

- **Q1 — subscription retrieval:** Option **A** — switch the subscription fetch to
  a Shadowrocket-style User-Agent (overridable via env). Reuses the existing
  base64→links decode path; stdlib-only; verified to work on the target airport.
  Clash-YAML parsing is *not* added.
- **Q2 — field coverage:** Option **B** — cover `server`, `port`, `password`,
  `sni`, `insecure`, `up_mbps`/`down_mbps`, **plus** `obfs` (Salamander) and
  `alpn`. Accept the `hy2://` short scheme. **Skip** port-hopping (`mport`).
- **Q3 — scope:** defer `sr-import.py` hy2 support (no Shadowrocket in play); add
  one small parser test (repo currently has zero tests); update docs.

## Design

### 1. Parser — `parse_hysteria2(link, tag)` in `config/vless-parse.py`

Parse `hysteria2://<password>@host:port?…#name` into a sing-box hysteria2
outbound. `password` comes from the URI userinfo (`urlsplit().username`,
url-decoded); hy2 puts the auth string there, with no colon.

Emitted outbound (minimal case):

```json
{
  "type": "hysteria2",
  "tag": "<tag>",
  "server": "<host>",
  "server_port": 443,
  "password": "<userinfo>",
  "tls": { "enabled": true, "server_name": "<sni|peer|host>", "insecure": false }
}
```

Rules:

- **TLS is always on** (hy2 is QUIC): `tls.enabled = true`;
  `server_name = sni || peer || host`.
- **insecure**: `true` when `insecure` query param is `1`/`true`/`True`.
- **alpn**: when the `alpn` param is present, add `tls.alpn = [..]` (split on
  `,`, drop empties). Otherwise omit — sing-box defaults to `h3`.
- **bandwidth**: when `upmbps`/`downmbps` present, set top-level integer
  `up_mbps`/`down_mbps`.
- **obfs**: when `obfs` present (Salamander), add top-level
  `"obfs": {"type": "salamander", "password": "<obfs-password>"}`. Use the
  `obfs-password` param for the password (empty string if absent).
- **Port** defaults to `443` when the URI omits it.
- Raise `ValueError("hysteria2 link missing password or host")` when either is
  absent (consistent with the existing parsers).

### 2. Dispatch & list handling

- `parse_link` (`vless-parse.py:127`): route both `hysteria2://` and `hy2://` to
  `parse_hysteria2`.
- `parse_many` allow-list (`vless-parse.py:158`): accept `hysteria2://` and
  `hy2://` alongside the existing schemes.
- **Header lines:** the base64 body starts with a `REMARKS=…` line (and possibly
  other non-URI lines). `parse_many` should **quietly skip** any line without
  `://` (currently it emits a `skipping unsupported link (?://)` warning). Skip
  such lines silently; keep the existing warning only for lines that *do* have a
  `://` scheme rowt doesn't support.
- `combine` (`vless-parse.py:171`): dedupe key already falls back to `password`
  (`vless-parse.py:177`), so hysteria2 nodes dedupe correctly — no change.

### 3. Subscription fetch — `fetch_subscription` (`vless-parse.py:195`)

- Default the request User-Agent to a **Shadowrocket-style string**, overridable
  via the **`ROWT_SUB_UA`** environment variable:
  `ua = os.environ.get("ROWT_SUB_UA") or "<shadowrocket-style UA>"`.
- Everything else unchanged: body → (base64 decode if no `://`) → `splitlines()`.
- Clash-YAML input remains explicitly unsupported (the existing error message
  stays); option A does not add YAML parsing.

Rationale for the UA change being low-risk: a Shadowrocket UA is the most widely
honored "return raw share links" signal. Airports that return base64 for a
generic `rowt/1.0` UA today will also return base64 for a Shadowrocket UA, so
currently-working subscriptions keep working.

### 4. Tests — `config/test_parse.py`

Plain `assert`s, no test framework, runnable as `python3 config/test_parse.py`
(prints `ok` and exits 0 on success; raises / exits nonzero on failure). It
imports the parser module by path. Cases:

1. **Basic link** — `hysteria2://pw@h.example:443?insecure=0&sni=s.example&upmbps=20&downmbps=80#JP`
   → asserts `type`, `server`, `server_port`, `password`, `tls.server_name`,
   `tls.insecure == false`, `up_mbps == 20`, `down_mbps == 80`, and that no
   `obfs` key is present.
2. **insecure + default port** — `insecure=1` → `tls.insecure == true`; a link
   without `:port` → `server_port == 443`.
3. **obfs** — link with `obfs=salamander&obfs-password=xyz` → outbound has
   `obfs == {"type":"salamander","password":"xyz"}`.
4. **alias** — `hy2://…` produces the same dict as the `hysteria2://…` form.
5. **parse_many over a base64-style blob** — input list
   `["REMARKS=foo", "hysteria2://pw@h:443#a", "hysteria2://pw2@h2:443#b"]`
   → returns 2 outbounds (header skipped), with unique tags.
6. **missing password** — `hysteria2://@h:443` raises `ValueError`.

### 5. Docs

- `README.md`: update the "supported protocols" statements, the intro/quick-start
  where VLESS/AnyTLS are listed, and add a hysteria2 example to `server add`.
- `config/vless-parse.py` module docstring: add hysteria2 to the supported
  protocols line and the header comment.

## Out of scope (deferred)

- `sr-import.py` hysteria2 support (no Shadowrocket in the current workflow).
- hysteria2 **port-hopping** (`mport` / port ranges).
- Clash-YAML subscription parsing.

## Verification plan

After implementation:

1. `python3 config/test_parse.py` → passes.
2. `rowt sub add '<airport url>'` → imports 3 hysteria2 servers.
3. `rowt server list` → three rows with type `hysteria2`.
4. `rowt up` (host mode) → `rowt ping` shows latencies; `rowt route www.google.com`
   → `escape` with an HTTP 200 through the tunnel.
