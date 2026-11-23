# Full reviews: Task file hashes are scoped to one repository

Plan: `2026-11-23-file-hash-repo-scope.md`. All seats ran on opus. Codex
was not run.

## Round 1 (body dbfb0cca974c)

- **backend**: approve-with-changes (28k, 28 s).
  1. [medium] A `null` was cached for a missing directory and for a
     directory that is not a repository. Accepted, through unix#3.
  2. [medium] No git version floor was stated (`--path-format` needs
     2.31). Accepted.
  3. [medium] Dropped paths should be counted in a debug log. Rejected:
     the plugin has no debug log for router rows. The partial-list test
     and the protocol doc name the loss instead.
  4. [low] Cost numbers. Accepted.
  5. [low] Expected live number. Accepted.
- **lang:typescript**: approve-with-changes (33k, 31 s).
  1. [high] Parse untrimmed output, since the prefix is empty at the
     root, and treat exit 129 as unresolved. Accepted, merged with unix#2.
  2. [high] In-flight promise cache. Accepted.
  3. [medium] A permanent `null`. Accepted, through unix#3.
  4. [medium] A real-git worktree test. Accepted.
  5. [low] A timeout returns a partial list. Accepted.
- **tui**: approve-with-changes (29k, 31 s).
  1. [medium] Give the real reinstall commands. Accepted.
  2. [low] Add a `relais doctor` check. Accepted.
- **unix**: approve-with-changes (25k, 24 s).
  1. [high] Pass `git show -z`. Accepted.
  2. [high] Parse at the root. Accepted, merged.
  3. [medium] Cache `null` only on "not a git repository". Accepted.
  4. [medium] Cap in-flight children at 4. Rejected: each edit is its
     own hook call, the in-flight promise dedupes by directory, and
     `fx.process.run` kills on timeout.
  5. [low] Strip `GIT_DIR` and related variables. Accepted.

## Round 2 (body 729b65a3006e)

- **backend**: approve-with-changes (35k, 13 s). Carried into
  implementation:
  1. [medium] Set `LC_ALL=C`. Also folded into lang:typescript#6.
  2. [medium] Tests for the cache rules: expiry, drop on edit, missing
     directory.
  3. [low] The protocol doc notes that a timeout can drop a path.
- **tui**: approve-with-changes (32k, 10 s). [medium] `LC_ALL=C`, the
  same as backend#1.
- **unix**: approve (28k, 10 s).
- **lang:typescript**: approve-with-changes (38k, 22 s).
  6. [high] The env-unset claim was unverified. Applied: it is now
     conditional, and `LC_ALL=C` was added.
  7. [medium] Cache-rule tests. Carried.

## Backend re-run (final body b54f8ff2dcf0)

- **backend**: approve-with-changes (37k, 10 s). Carried: [medium] if
  `$.process.run` cannot unset a variable, run
  `env -u GIT_DIR -u GIT_WORK_TREE -u GIT_COMMON_DIR git …` instead of
  dropping the clause.
