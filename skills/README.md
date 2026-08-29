# rowt agent skills

Version-controlled home of rowt's Claude Code / agent skills. **This repo is the
source of truth** — edit the files here; the agent skill directories link back to
them.

| skill | for |
|---|---|
| [`rowt/SKILL.md`](rowt/SKILL.md) | **users** who install via `brew install tanghong123/tap/rowt` — setup (importing from an existing VPN client, choosing what tunnels), everyday operation, and debugging. Defers to `rowt onboard` for the live, version-current reference. |

Working on **rowt's own code / releases** (gates, the version-bump hook, the
release-to-Homebrew flow) is not a skill: it lives in [`../CLAUDE.md`](../CLAUDE.md),
which an agent loads automatically whenever it works in this repo. It used to be a
`rowt-dev` skill, which had to be invoked to be read — and silently went stale
because nothing made a change to the gates touch it.

## Linking them into your agent

Skills load from `~/.agents/skills` (the standalone truth source, read directly by
Codex) and `~/.claude/skills` (Claude Code). Point those at this repo once:

```sh
ln -sfn "$PWD/skills/rowt"  ~/.agents/skills/rowt
# ~/.claude/skills/rowt in turn symlinks to ~/.agents/skills/rowt
```

Or let rowt do it: `rowt skill install` links the installed copy (from a brew
install, the stable `opt` path, so `brew upgrade` refreshes the skill).

New skills usually need a fresh agent session to load.

## Under a skill manager

`rowt skill <install|uninstall|status> --store` touches **only** the shared store
`~/.agents/skills/rowt` and leaves every agent directory alone.

That is the shape a manager like [knack](https://github.com/knackhq/knack) needs:
it points `~/.claude/skills/rowt` at the store itself, so if rowt also wrote that
path the two would fight over it — knack aiming at the store, rowt at the source.
One copy, N links, and `unexport` keeps meaning something. Plain `install` is
still the right call when nothing manages your skills for you.

knack adopts rowt as a *foreign* skill — rowt ships and upgrades its own skill,
so knack fronts the lifecycle through rowt's own commands rather than vendoring
a copy. The recipe is `~/.config/knack/recipes/rowt.toml`:

```toml
owner = "rowt"
store_dir = "~/.agents/skills"
[upgrade]      argv = ["brew", "upgrade", "tanghong123/tap/rowt"]
[skill_install] argv = ["rowt", "skill", "install", "--store"]
```

```sh
knack lib adopt rowt --via rowt:~/.config/knack/recipes/rowt.toml
```
