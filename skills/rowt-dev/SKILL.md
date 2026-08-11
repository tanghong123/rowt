---
name: rowt-dev
description: Develop and release the rowt codebase (repo at ~/personal/rowt) — editing the bash CLI `bin/rowt`, the Rust/ratatui TUI `rowt-monitor/`, or the Python helpers in `config/`; running the CI gates and tests; committing (with the auto version-bump hook); and cutting a Homebrew release through the tap at ~/personal/homebrew-tap. Use when the task is changing/testing/shipping rowt's own code, NOT operating an installed rowt for a user — for how the tool BEHAVES and how end users set it up, use the `rowt` skill. Encodes the exact gates (bash -n, ruff, cargo test, "don't cargo fmt"), the version-bump rules, and the tag → gh release → tap → brew-validate flow.
---

# rowt-dev

Development + release for the **rowt** repo. For the tool's runtime behavior, lanes, config layout, and the critical operational rules (e.g. never run `up/reload/restart` as a background task — the daemon inherits the task's pipe and killing the task kills sing-box), see the **`rowt` skill**; don't duplicate that here.

## Repo layout (`~/personal/rowt`)

- **`bin/rowt`** — the whole CLI, one bash script (~3.5k lines). `ROWT_VERSION="X.Y.Z"` near the top. Runs under **bash** (the interactive shell here is zsh with `noclobber` — use `>|` or file tools; prefer `bash bin/rowt …`).
- **`rowt-monitor/`** — Rust crate, two bins: the ratatui TUI (`rowt-monitor`) and the metrics `collector` sidecar. Absolute-cell rendering matched **byte-for-byte** against golden captures.
- **`config/`** — Python helpers (stdlib-only where possible): `vless-parse.py` (share-link → sing-box outbound; vless/vmess/anytls/hysteria2 + subscriptions), `sr-import.py` (Shadowrocket), `foreign-import.py` (Clash Verge/V2Box/FlClash → the shared review JSON), `geosite-lookup.py` (the `add`-time "also covered by geosite:…" hint), and lane-template `*.txt`. Tests: `test_parse.py`, `test_foreign.py`.
- **`README.md`, `DESIGN.md`, `FUTURE.md`, `rowt-monitor/{README,METRICS,DESIGN}.md`** — keep in sync with behavior when you change it.
- **`.githooks/pre-commit`** — the auto version-bump (enable in a fresh clone: `git config core.hooksPath .githooks`).

## CI gates — run before every commit

- **Shell:** `bash -n bin/rowt install.sh .githooks/pre-commit`. (shellcheck is advisory.)
- **Python (`config/`):** `ruff check config/` · `ruff format --check config/` · `python3 -m py_compile config/*.py` · smoke tests `python3 config/test_parse.py` and `python3 config/test_foreign.py`. New/edited files must be `ruff format`-clean — run `ruff format config/<file>` to conform.
- **Monitor:** `cd rowt-monitor && cargo test` (golden byte-diff + interaction tests) and `cargo build`. **Do NOT `cargo fmt`** — the tree is intentionally hand-formatted with long lines and is not `cargo fmt`-clean; match the surrounding style manually. Golden tests compare glyphs with a mask (`rowt-monitor/tests/golden.rs`); when you change layout, update the goldens/assertions deliberately. A pre-existing clippy "very complex type" note (metrics.rs) is expected.

## Versioning & commits

- The pre-commit hook **auto-bumps the PATCH** version in `bin/rowt` on any commit that touches `bin/|config/|lima/|install.sh|README.md|DESIGN.md`, **unless** a `ROWT_VERSION=` change is already staged (a manual minor/major, or the initial add). **`rowt-monitor/`-only changes do NOT bump** — a monitor fix commits at the current version; to release it, bump `ROWT_VERSION` in `bin/rowt` yourself in the same commit.
- Commit only when the user asks. On the default branch, branch first. End commit messages with `Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>`; end PR bodies with the Claude Code footer.
- Testing a change against the **live** `~/.config/rowt`: run the repo `bin/rowt`, but remember a later `rowt up`/`reload` from the *installed* binary re-renders `host.json`, so features must land in the installed version too — after releasing, have the user `brew upgrade rowt`. And per the `rowt` skill, run any reload/restart **in the foreground**.

## Cut a release (only when the user says to)

Repo `tanghong123/rowt`; tap **`~/personal/homebrew-tap`** (`Formula/rowt.rb`), installed as `tanghong123/tap`.

1. Clean tree at the intended `ROWT_VERSION` (confirm `grep ROWT_VERSION= bin/rowt`).
2. `git tag -a vX.Y.Z -m "…"` && `git push origin vX.Y.Z`.
3. `gh release create vX.Y.Z --title "rowt X.Y.Z" --notes "…"`.
4. **Prebuilt binary asset (Apple Silicon pours it — the formula's `resource "rowt-monitor"`).** The name says "monitor" for historical reasons; it now carries every prebuilt binary the formula installs, from **two** cargo workspaces. Tar them **at the archive root** and attach:
   ```
   (cd rowt-monitor && cargo build --release --bins)   # rowt-monitor, rowt-collector
   cargo build --release --workspace                   # rowt-render, rowt-watch-tick, rowt-rs
   COPYFILE_DISABLE=1 tar --no-mac-metadata -czf /tmp/rowt-monitor-aarch64-apple-darwin.tar.gz \
     -C rowt-monitor/target/release rowt-monitor rowt-collector \
     -C "$PWD/target/release" rowt-render rowt-watch-tick rowt-rs
   tar tzf /tmp/rowt-monitor-aarch64-apple-darwin.tar.gz   # must list exactly those five names
   gh release upload vX.Y.Z /tmp/rowt-monitor-aarch64-apple-darwin.tar.gz -R tanghong123/rowt
   MON_SHA=$(shasum -a 256 /tmp/rowt-monitor-aarch64-apple-darwin.tar.gz | awk '{print $1}')
   ```
   The **second `-C` is absolute** on purpose: `tar`'s `-C` is relative to the previous one, so `-C target/release` after `-C rowt-monitor/target/release` would look for `rowt-monitor/target/release/target/release`.

   **Every name in that list is a name the formula calls `.install` on, so a missing one fails the install, not the build.** This step used to tar only `rowt-monitor` and `rowt-collector` while the formula installed four — following it literally produced an asset that `brew install` rejected. If you add or drop a prebuilt binary, change both this command and `Formula/rowt.rb` in the same pass.

   (Build on an `arm64` mac — native, not cross.) Intel still gets no asset, but since 3.3.7 it is no longer a lesser install: the formula's `on_intel` branch builds **`rowt-monitor` and `crates/rowt-cli`** from source (`depends_on "rust" => :build`). That second `cargo install` is what lets `depends_on "python@3.12"` be gone from BOTH architectures — `bin/rowt` runs the Rust helpers now, and without `rowt-rs` present it would fall back to Pythons that are no longer guaranteed. It is the one install path this machine cannot test; if you touch it, say so in the release notes. The collector/render/watch-tick sidecars are still Apple-Silicon-only.
5. Source `sha256`: `curl -sL https://github.com/tanghong123/rowt/archive/refs/tags/vX.Y.Z.tar.gz | shasum -a 256` (or download then shasum).
6. Update `Formula/rowt.rb`: the source `url` (`…/refs/tags/vX.Y.Z.tar.gz`) + its `sha256`, the **`resource "rowt-monitor"` `url` (`…/releases/download/vX.Y.Z/…`) + its `sha256` (`$MON_SHA` from step 4)**, and the `assert_match "rowt X.Y.Z"` test. Keep the `caveats`/`depends_on` current with any new user-facing command. `git pull --rebase` the tap first (it may have other commits), commit `rowt X.Y.Z`, push.
7. Validate: `brew update && brew fetch tanghong123/tap/rowt` (errors on either sha mismatch) and `brew info tanghong123/tap/rowt` (shows `stable X.Y.Z`).

Monitor-only releases still need step 1's manual `ROWT_VERSION` bump first, and still rebuild + re-upload the step-4 asset (the pinned resource must point at the new tag).

## Notes

- **`rowt onboard` is the shared getting-started + reference surface** for both CLI users and the `rowt` (user) skill — it always prints the stateful checklist *plus* the full command reference and the on-disk doc paths. Keep it current when you add features (import sources, lanes, `geosite:`). The formula ships **README.md + DESIGN.md into `libexec`** (`File.exist?` guard) so `$HERE/README.md` resolves for brew users and `onboard` can point at them; `install.sh` already copies the whole tree. So a new user-facing command/flow should be reflected in `onboard`, `README.md`, and the `rowt` skill together.
- The Python parsers/importers are stdlib-first; `foreign-import.py` shells out to `yq` for Clash YAML (PyYAML isn't a dep) and to the bundled `sing-box rule-set decompile` for geosite membership. Reuse `vless-parse.py`'s `parse_link`/`combine`/`key_of` rather than duplicating outbound/dedup logic.
- Keep the `rowt` (user) skill and `rowt help` as the source of truth for command names/flags; if you rename or add a command, update both docs and that skill.
