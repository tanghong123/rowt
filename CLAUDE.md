# CLAUDE.md

Guidance for Claude Code (claude.ai/code) working in this repository.

This file covers **developing and releasing rowt itself**. For how the tool
*behaves* — lanes, config layout, and the operational rules (e.g. never run
`up`/`reload`/`restart` as a killable background task: sing-box stays in the
task's process group, so killing that group kills the router — and redirect
their output to a file rather than piping it: through rowt 3.5.8 the log
splitter held a pipe open and the command never returned) — use the **`rowt`
skill** in `skills/rowt/`. Don't duplicate that here.

## Repo layout

- **`bin/rowt`** — the whole CLI, one bash script (~5.3k lines).
  `ROWT_VERSION="X.Y.Z"` at line ~56. Runs under **bash**; the interactive shell
  here is zsh with `noclobber`, so use `>|` (or the file tools) for redirection,
  and prefer `bash bin/rowt …` when testing.
- **`crates/`** — the Rust workspace: `rowt-core` (pure), `rowt-platform` (the OS
  seam), `rowt-cli` (`rowt-rs`). The strangler port of `bin/rowt`; see
  **PORTING.md** for the phase plan and what is / isn't cut over.
- **`rowt-monitor/`** — a *separate* workspace (own `Cargo.lock`, own goldens),
  two bins: the ratatui TUI (`rowt-monitor`) and the metrics `collector`
  sidecar. Absolute-cell rendering matched **byte-for-byte** against golden
  captures in `rowt-monitor/renders/`.
- **`config/`** — Python helpers, stdlib-first: `vless-parse.py`,
  `sr-import.py`, `foreign-import.py`, `geosite-lookup.py`, plus lane-template
  `*.txt`. Most are being replaced by `rowt-rs` helpers; `bin/rowt` prefers the
  Rust path and falls back to these.
- **`tests/parity/`** — the differential harness (see below).
- **`skills/rowt/`** — the end-user skill. `~/.agents/skills/rowt` is a symlink
  to the **brew-installed** copy (`/opt/homebrew/opt/rowt/libexec/skills/rowt`),
  *not* to this tree — so editing it here changes nothing live until the next
  release + `brew upgrade`. Keep it in sync with `rowt help` / `rowt onboard`
  when commands change.
- **`share/knack/rowt.toml`** — the knack foreign-owner recipe, printed by
  `rowt skill recipe`. The formula installs `share/` into `libexec`, so adding a
  file here that the CLI reads means changing `Formula/rowt.rb` in the same pass.
- **Docs:** `README.md`, `DESIGN.md`, `PORTING.md`, `FUTURE.md`,
  `rowt-monitor/{README,DESIGN,METRICS,COLORS}.md`. Keep them current with
  behavior you change.
- **`research/`** — surveys that answer a question before any code exists, e.g.
  `ios-port.md` (can rowt's lanes run on an iPhone). Each is dated and cites
  its platform sources. Treat it as a snapshot: re-verify the claims before
  building on one.
- **`archive/`** — material the repo has outgrown, kept for history and not
  maintained: the original monitor UX handoff (`ux-design/`) and resolved
  investigations such as `BUG-corp-lane-throughput.md`. No code or test reads
  from it, so don't cite it as current.
- **`.githooks/pre-commit`** — the auto version-bump, **and** a refusal when a
  commit leaves `tests/parity/{LEDGER,CLI-LEDGER}.md` stale. Enable in a fresh
  clone: `git config core.hooksPath .githooks`.

## Gates

**`.github/workflows/ci.yml` is the authoritative list** — read it rather than
trusting a summary. It runs on every push to `main` and every PR:

- **Shell:** `bash -n` over `bin/rowt install.sh .githooks/pre-commit`
  (shellcheck is advisory, `continue-on-error`).
- **Python:** `ruff check config/`, `ruff format --check config/`,
  `python -m py_compile config/*.py`. **ruff is pinned to 0.16.2** and
  `ruff.toml` declares the rule selection — both deliberately: an unpinned ruff
  0.16 widened its implicit defaults and turned `main` red on 31 findings in
  files nobody had touched. Don't unpin to "fix" a lint.
  Smoke tests are not in CI; run them locally:
  `for t in config/test_*.py; do python3 "$t"; done`
  (`test_parse`, `test_foreign`, `test_import_merge`, `test_netdetect`,
  `test_corp_sync_reconcile`).
- **Rust:** `cargo build --release --workspace` and `cargo test --workspace` on
  macOS (the platform layer talks to `networksetup`, `scutil`, `launchctl`,
  `kern.boottime`). The monitor is a separate workspace and needs its own
  `cd rowt-monitor && cargo test` or it is simply not built.

**Do NOT run `cargo fmt`.** The tree is intentionally hand-formatted with long
lines and is not fmt-clean. CI deliberately omits `cargo fmt --check`. Match the
surrounding style by hand. A pre-existing clippy "very complex type" note in
`metrics.rs` is expected.

Monitor golden tests compare glyphs with a mask (`rowt-monitor/tests/golden.rs`);
when you change layout, update the goldens/assertions deliberately.

### The differential gates (pre-release, local only)

`tests/parity/bin/parity` runs rowt inside a sandbox that cannot touch this
machine — `HOME`/`XDG_CONFIG_HOME` point at a scratch tree and every effectful
command is a recorder shim — then compares **stdout, stderr, exit status, the
argv trace, and the whole config tree** against the bash original.

They are **not in CI**: `parity cli-diff` alone is ~241 cases and ~11 minutes,
and five cases wait on a router designed not to come up. CI's job is catching
what doesn't compile. `parity list` shows the matrix; the main gates are
`cli-diff`, `lanes-diff`, `render-matrix`, `classify-matrix`, `golden`, and
`selftest`. **See `tests/parity/README.md`** — it is the source of truth, and
`LEDGER.md` / `CLI-LEDGER.md` record how much of the shell `rowt-rs` covers.

Run these before any release that touches `bin/rowt` or `crates/`.

## Versioning & commits

The pre-commit hook **auto-bumps the PATCH** in `bin/rowt` for any commit
touching `bin/`, `config/`, `lima/`, `install.sh`, `README.md`, or `DESIGN.md`
— **unless** a `ROWT_VERSION=` change is already staged (a manual minor/major).

It also **refuses a commit that leaves a coverage ledger stale**, whenever the
commit stages one of the three inputs they are generated from: `bin/rowt`,
`crates/rowt-cli/src/main.rs`, or `cli-cases.txt`/`commands.txt`. It checks and
tells you the command; it does not regenerate for you. Both tables had gone
stale for two releases — `speed` was a command arm neither listed — and they
were wrong in the direction that *over*-reports coverage. Fix with
`tests/parity/bin/parity ledger && tests/parity/bin/parity cli-ledger`, then
stage both.

**`rowt-monitor/`-only changes do NOT bump.** A monitor fix commits at the
current version; to *release* it you must bump `ROWT_VERSION` in `bin/rowt`
yourself, in the same commit.

Commit only when asked. Conventional-commit subjects; explain the *why* in the
body.

Testing against the **live** `~/.config/rowt`: run the repo's `bin/rowt`, but
remember a later `rowt up`/`reload` from the *installed* binary re-renders
`host.json` — so a feature must land in the installed version too. Run any
reload/restart in the **foreground** (see the `rowt` skill).

## Cutting a release

Repo `tanghong123/rowt`; tap **`~/personal/homebrew-tap`** (`Formula/rowt.rb`),
installed as `tanghong123/tap`. Only when the user says to.

1. Clean tree at the intended version — confirm `grep ROWT_VERSION= bin/rowt`.
2. `git tag -a vX.Y.Z -m "…" && git push origin vX.Y.Z`
3. `gh release create vX.Y.Z --title "rowt X.Y.Z" --notes "…"`
4. **Prebuilt binary asset** (Apple Silicon pours it — the formula's
   `resource "rowt-monitor"`; the name is historical, it now carries every
   prebuilt binary, from **two** workspaces). Build native on arm64:

   ```sh
   (cd rowt-monitor && cargo build --release --bins)   # rowt-monitor, rowt-collector
   cargo build --release --workspace                   # rowt-render, rowt-watch-tick, rowt-rs
   COPYFILE_DISABLE=1 tar --no-mac-metadata -czf /tmp/rowt-monitor-aarch64-apple-darwin.tar.gz \
     -C rowt-monitor/target/release rowt-monitor rowt-collector \
     -C "$PWD/target/release" rowt-render rowt-watch-tick rowt-rs
   tar tzf /tmp/rowt-monitor-aarch64-apple-darwin.tar.gz   # must list exactly those five
   gh release upload vX.Y.Z /tmp/rowt-monitor-aarch64-apple-darwin.tar.gz -R tanghong123/rowt
   MON_SHA=$(shasum -a 256 /tmp/rowt-monitor-aarch64-apple-darwin.tar.gz | awk '{print $1}')
   ```

   **The second `-C` is absolute on purpose**: `tar`'s `-C` is relative to the
   previous one, so a relative `-C target/release` would look for
   `rowt-monitor/target/release/target/release`.

   **Every name in that list is one the formula calls `.install` on, so a
   missing one fails the install, not the build.** This step once tarred only
   two of the four the formula then installed, producing an asset `brew install`
   rejected. If you add or drop a prebuilt binary, change this command *and*
   `Formula/rowt.rb` in the same pass.

5. Source sha: `curl -sL https://github.com/tanghong123/rowt/archive/refs/tags/vX.Y.Z.tar.gz | shasum -a 256`
6. Update `Formula/rowt.rb`: source `url` + `sha256`, the
   `resource "rowt-monitor"` `url` + `sha256` (`$MON_SHA`), and the
   `assert_match "rowt X.Y.Z"` test. Keep `caveats`/`depends_on` current with any
   new user-facing command. **When `SINGBOX_VERSION` in `bin/rowt` changes, update
   both `resource "sing-box"` blocks (arm64 + amd64 `url` and `sha256`) in the same
   pass** — the formula bundles the pinned engine (it no longer `depends_on
   "sing-box"`, whose version brew picks) and rowt copies it from `libexec/bin`;
   if the two disagree, `onboard` reports the bundle as non-pinned and rowt tries
   GitHub instead. If the formula carries a `revision N` line (from a prior
   formula-only fix — a packaging or sing-box change with no rowt source bump),
   **delete it** in this pass: a new version resets the revision to 0, so
   `3.5.9` + a leftover `revision 1` would wrongly pour as `3.5.9_1`.
   `git pull --rebase` the tap first, commit `rowt X.Y.Z`, push.
7. Validate: `brew update && brew fetch tanghong123/tap/rowt` (errors on either
   sha mismatch) and `brew info tanghong123/tap/rowt` (shows `stable X.Y.Z`).

Intel gets no asset but is not a lesser install: the formula's `on_intel` branch
builds **`rowt-monitor` and `crates/rowt-cli`** from source
(`depends_on "rust" => :build`). That second `cargo install` is what let
`python@3.12` leave the dependency list on *both* architectures — `bin/rowt`
runs the Rust helpers now, and without `rowt-rs` an Intel install would fail on
`server add` with "python3 not found". The collector/render/watch-tick sidecars
stay Apple-Silicon-only. **This is the one install path this machine cannot
test** — if you touch it, say so in the release notes.

Monitor-only releases still need the manual `ROWT_VERSION` bump in step 1, and
still rebuild + re-upload the step-4 asset (the pinned resource must point at
the new tag).

## Notes

- **`rowt onboard` is the shared getting-started + reference surface** for both
  CLI users and the `rowt` skill: it prints the stateful checklist *plus* the
  full command reference and on-disk doc paths. The formula ships `README.md`
  and `DESIGN.md` into `libexec` (with a `File.exist?` guard) so `$HERE/README.md`
  resolves for brew users. A new user-facing command or flow should land in
  `onboard`, `README.md`, and `skills/rowt/` together.
- The Python parsers are stdlib-first; `foreign-import.py` shells out to `yq`
  for Clash YAML (PyYAML isn't a dep) and to the bundled
  `sing-box rule-set decompile` for geosite membership. Reuse
  `vless-parse.py`'s `parse_link`/`combine`/`key_of` rather than duplicating
  outbound/dedup logic.
- Fixtures use documentation domains (`example.com`, `corp.example`, RFC 5737
  ranges). **Never commit real employer hostnames or CIDRs** — the repo is
  public and was swept once already.
