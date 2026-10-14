---
canonical: amont:docs/plans/2026-10-14-agents-land-through-amont-land.md
phases: [3]
status: active
---
Part of [Agents land through `amont land`](https://github.com/fredericrous/amont/blob/main/docs/plans/2026-10-14-agents-land-through-amont-land.md).

This repository carries the relais half of Phase 3: the generated `relais-verified-push` skill lands with `amont land --json` (amont 1.54.0, amont-agent 2.41.0 judges a land as a push) and keeps `git push` + `git ls-remote` as the fallback below those versions. The dotfiles half is `worktree-task` F6. Reaching a session needs a relais release and the person's plugin reinstall.

## Implementation review

approve-with-changes, then a Delta: the worker deny floor now refuses `amont land` too (it runs `git push` itself, and the skill teaches it as the way to push; the trust hash does not move, the floor being a code constant), code spans kept on one line, CHANGELOG entry. Same exit semantics as the dotfiles half (exit 3 split into unreachable and unverified). Reviewed tree 13d83be.
