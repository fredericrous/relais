+++
type = "task"
mode = "afk"
status = "closed"
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

## Resolution

Plan: [2026-11-23-file-hash-repo-scope.md](../../plans/2026-11-23-file-hash-repo-scope.md),
fix commit `d527b1e1` on `prog/phase8-learner/t11-file-hash-repo-scope`.

- **The hash.** A `files` entry is the first 16 lowercase hex digits of
  SHA-256 over `<git common dir>`, a NUL byte, and the path from the
  work tree root (`scopedFileHash`). The common dir is what `git rev-parse
  --path-format=absolute --git-common-dir` prints, so a worktree and its
  main checkout share it. `/tmp` and `/private/tmp` agree because git
  prints the realpath. Both were checked with real git.
- **Out-of-repository paths are not recorded.** Memory files and
  `~/.claude/CLAUDE.md` no longer reach the ledger. `task.files` keeps
  them for scope growth, `outcomeOf` and revert expansion, as before.
- **Roots per directory.** `resolveRoot` runs one `git rev-parse
  --git-common-dir --show-toplevel --show-prefix` per directory, under
  `env -u GIT_DIR -u GIT_WORK_TREE -u GIT_COMMON_DIR` (an `env` overlay
  cannot unset a variable), with `LC_ALL=C` and the prompt off. It caches
  the in-flight promise.
  - A "not a git repository" answer is cached for 60 s, and is dropped
    when a later edit lands in that directory.
  - Any other failure, exit 129, an output of the wrong shape, or a
    timeout is not cached.
  - Edits warm the cache in the background, and the gate read shares it.
- **Reverts.** A `git revert` lookup hashes the `git show -z` names with
  the lookup directory's common dir. It never re-resolves a file's own
  directory, which may be gone. A revert of named paths is hashed through
  the same cache, and its row still comes before its task row.
- **Protocol.** `docs/router-protocol.md` states the bytes, the dropped
  paths, the loss on a timeout, and the cutover. The version is not
  bumped, because the wire shape is unchanged. Older rows keep the
  cwd-relative hash, so the t02 miss query keeps its stop list for them.
  [t13](t13-revert-overlap-live-count.md) also runs that query on the new
  rows. Expected: 0 false misses from out-of-repository paths, where t02
  found 45.
- **Run.** relais `run-660e7d22ce063-e296` wrote the candidate. Its two
  verify runs hit `admission_wait_exhausted` because the host was jammed.
  The candidate was applied by hand and recorded as `salvaged`.
  `scripts/plugin-test.sh` passed 512 of 512. A cold rev-parse takes
  about 35–40 ms per directory; a warm directory spawns nothing.
- **Not released.** It ships with t10 and t12.
