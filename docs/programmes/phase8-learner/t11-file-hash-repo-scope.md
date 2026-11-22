+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# Task file hashes are scoped to one repository

## Question

`fileHash` hashes `repoRelative(file, cwd)`, so two kinds of path
collide across unrelated sessions (t02, "cannot see" 1–2):
- a path outside the cwd stays absolute, so memory files and
  `~/.claude/CLAUDE.md` match everywhere (45 false misses);
- the same relative path in two repositories hashes the same.

Drop out-of-repository paths from `files`, and key the hash by the
repository (its git common dir).

## Look at

- `claude-plugin/hooks/router.ts` `fileHash`, `repoRelative`, the edit
  evidence that fills `task.files`, `grownScope` (it reads the same
  files).
- `docs/router-protocol.md`, the `files` field, and whether the change
  needs a protocol version bump.

## Answer looks like

Edits and reverts hash `(repo id, repo-relative path)`. Paths outside any
repository are not recorded. Scope growth still reads every edited file.
The protocol doc states the new hash. Old rows keep their hashes: the
miss query (t02) still needs its stop list for them.
