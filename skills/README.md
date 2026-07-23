# rowt agent skills

Version-controlled home of rowt's Claude Code / agent skills. **This repo is the
source of truth** — edit the files here; the agent skill directories link back to
them.

| skill | for |
|---|---|
| [`rowt/SKILL.md`](rowt/SKILL.md) | **users** who install via `brew install tanghong123/tap/rowt` — setup (importing from an existing VPN client, choosing what tunnels), everyday operation, and debugging. Defers to `rowt onboard` for the live, version-current reference. |
| [`rowt-dev/SKILL.md`](rowt-dev/SKILL.md) | working on **rowt's own code / releases** (CI gates, the version-bump hook, the release-to-Homebrew flow). |

## Linking them into your agent

Skills load from `~/.agents/skills` (the standalone truth source, read directly by
Codex) and `~/.claude/skills` (Claude Code). Point those at this repo once:

```sh
ln -sfn "$PWD/skills/rowt"      ~/.agents/skills/rowt
ln -sfn "$PWD/skills/rowt-dev"  ~/.agents/skills/rowt-dev
# ~/.claude/skills/{rowt,rowt-dev} in turn symlink to ~/.agents/skills/*
```

New skills usually need a fresh agent session to load.
