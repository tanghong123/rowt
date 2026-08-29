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

New skills usually need a fresh agent session to load.
