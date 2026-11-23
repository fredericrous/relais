---
status: active
branch: prog/phase8-learner/t11-file-hash-repo-scope
repos: [relais]
adrs: []
programme: docs/programmes/phase8-learner
ticket: t11-file-hash-repo-scope
plan-id: p-01M83190QFZQ9W77DB761MJKVR
---
# Task file hashes are scoped to one repository

## Review panel

👉 **Decide:** none — approve if a file hash should name one file of one repository, dropping files in no repository.
📍 relais · 2 phases · 1 step for you · next: Phase 1 via relais. Panel: backend, lang:typescript, tui, unix (opus); codex not run.
**Changed by review:** rev-parse output parsed untrimmed, exit 129 unresolved; `null` cached only on "not a git repository", 60 s; in-flight promise cache.
**Verdicts:** round 2: 1 approve, 3 approve-with-changes; 7 carried; words 1180 → 1395 (×1.18); remove 0, merge 0.
📄 Full reviews: `2026-11-23-file-hash-repo-scope.reviews.md`

## Picture

```
 Edit /w/relais-wt-x/src/lib.rs        Edit ~/.claude/memory/a.md
          |                                    |
 dir /w/relais-wt-x/src (cached)       dir ~/.claude/memory
          |                                    |
 git rev-parse --path-format=absolute  git: not a work tree
   --git-common-dir --show-toplevel            |
   --show-prefix                       dropped from `files`
          |                            (task.files keeps it:
 common /w/relais/.git, prefix src/     scope, reverts read it)
          |
 sha256("/w/relais/.git" NUL "src/lib.rs")[0..16]
          ==  same file edited from /w/relais
          !=  src/lib.rs in another repository
```

Two edits, hashed: a repository file is keyed by its repository's common
dir and its path from the root; a file in no repository is not recorded.

## Goal

A file hash names one file of one repository. Today `fileHash` hashes the
path relative to the session cwd, so (phase8 t02, "cannot see" 1–2):
- a path outside the cwd stays absolute and collides across every session
  (memory files, `~/.claude/CLAUDE.md`: 45 false misses);
- `src/lib.rs` in two repositories hashes the same.

After this change, task and revert rows hash `(repository id, path relative
to the repository root)`, where the repository id is the git common dir, so
a worktree and its main checkout share it. Paths in no repository are not
recorded.

## Non-goals

- Correction rows carrying files (t12): it will reuse the hashing helper
  exported here.
- Rehashing old rows: the ledger stores only hashes. The t02 miss query
  keeps its stop list for rows written before the cutover.
- Any Rust, ledger or wire-shape change: `files` stays a list of 16
  lowercase hex strings; relais stores and compares them as opaque strings.
- A release: t10, t11 and t12 ship together (map Handoff).

## Code review

- relais: lang:typescript, unix, backend (tui left out: no output a person reads)

## Behaviour

**The hash.** `files` entries are the first 16 lowercase hex digits of
SHA-256 over the UTF-8 bytes of `<common dir>` `0x00` `<repo-relative path>`:
- `<common dir>` is what `git rev-parse --path-format=absolute
  --git-common-dir` prints, trailing newline removed (git 2.31 or later;
  this machine runs 2.56.1);
- `<repo-relative path>` is the file's path from the work tree root, `/`
  separated, no leading `./` (`--show-prefix` of its directory, then its
  basename).

The pure function is `scopedFileHash(commonDir, repoRelativePath)` in
`router.ts`. `fileHash` and `repoRelative` go.

**Roots, per directory.** `routing.ts` gains `resolveRoot(fx, store, dir)`.
It runs `git rev-parse --path-format=absolute --git-common-dir
--show-toplevel --show-prefix` in `dir`, with `GIT_TERMINAL_PROMPT=0` and `LC_ALL=C` (the stderr match below is English), `GIT_DIR`, `GIT_WORK_TREE` and `GIT_COMMON_DIR` unset only if `$.process.run` can unset a variable (an empty value is not unset; Phase 1 checks the API and drops this clause if it cannot), and the
gate timeout, and caches the answer in `store.router.roots` (a `Map` keyed by
the directory string; it holds the in-flight promise, removed on throw or timeout, so concurrent calls share one git run):
- exit 0 with exactly three lines (stdout split on `\n`, never trimmed
  or filtered: the prefix line is empty at the repository root) →
  `{ common, toplevel, prefix }`, cached; any other shape, or exit 129
  (git older than 2.31 rejects `--path-format`) → unresolved, not cached;
- git exit whose stderr says "not a git repository" → `null` ("in no repository"), cached with a 60 s expiry, and dropped when a later edit lands in that directory; any other git exit (missing directory, `safe.directory` refusal) → unresolved, not cached;
- a spawn that throws or a timeout → not cached; that call has no answer.

The cache lives for the session module, with at most 512 entries; past that,
answers are not cached. It is cleared when the session cwd changes (the
same rule as `router.repository`).

**Warm at edit time.** When `onEvidence` records an edit, the hook starts
`resolveRoot` for the file's directory without awaiting it (fire and
forget, failures swallowed, as `resolveRepository` is at line 504). A
directory later removed (`rm -rf`, a revert) still hashes from the cache.

**`hashFiles(fx, store, paths)`** (exported for t12): each absolute path's
directory (bare names against the session cwd, as `directoryOf` does)
resolved through `resolveRoot`, all at once, bounded by one gate timeout in
total; a path whose root is `null` or unresolved is dropped; the result is
the sorted, unique hashes.

**Task rows.** `taskRecord(task, endedAt, files, repository?)` takes the
hashed list instead of the cwd. `endTask` runs the gate read and
`hashFiles` together (`Promise.all`), so the task end waits for one gate
timeout at most, not two. `readGateChecks` reads its toplevels from
`resolveRoot` (one git call per directory serves both). `queueCorrected`
hashes the same way.

**Revert rows.** `revertRecord(state, task, hashes)` takes the hashes.
`noteRevert` always queues through `r.reverts` (the path list is now
hashed asynchronously too), with `task`, `state` and `session` captured at
the call, so `settleReverts` still puts the revert before its task row and
session end still waits for it:
- checkout/restore paths: `hashFiles` (their directories were usually
  warmed by the edits);
- `git revert` lookups: `lookupRevertedFiles` replaces `--show-cdup` with
  `--path-format=absolute --git-common-dir` in the lookup directory and
  hashes each `git show -z --no-relative --name-only` name (NUL-split, so no `core.quotePath` quoting; already repo-relative) with
  that common dir. It never re-resolves a directory by name: a revert that
  removed a directory still hashes its files.

**Unchanged.** `task.files` keeps every raw edited path: `grownScope`,
`outcomeOf` (`answered` needs no edit at all, so a task that edited only a
memory file is still `unknown`) and `revertedPaths`' directory expansion
read it as before. Rust's `answered_with_files` merge rule needs no change:
the plugin already sends `unknown` for any edit.

**Protocol doc.** `docs/router-protocol.md`, the `files` bullet, states the
new hash bytes, that out-of-repository paths are not recorded, and the
cutover: rows from a plugin before the release carrying t11 use the old
cwd-relative hash; a query that mixes them keeps the t02 stop list for the
older rows, which t13's "after the reinstall" window already excludes.
No `accepts` gate and no version bump: the wire shape is unchanged and an
older relais stores the new hashes as it stores the old.

## Phases

- [ ] Phase 1 — scoped hashes in the plugin, with tests:
  `claude-plugin/hooks/router.ts`, `claude-plugin/hooks/routing.ts`,
  `claude-plugin/tests/router.test.ts`, `claude-plugin/tests/routing.test.ts`,
  `claude-plugin/tests/routing-support.ts`, `docs/router-protocol.md`
- [ ] Phase 2 — close t11 (Resolution, Decisions line, Handoff naming t12
  next and the joint release): `docs/programmes/phase8-learner/`

### 🧑 Your steps
- After the release that carries t10–t12, reinstall the plugin:
  `relais install --claude --user --write`, then `relais coordinator stop` —
  because the hooks run from the installed plugin, and only rows the new
  code writes carry scoped hashes. Check with `relais doctor`: its `plugin`
  finding names the new release.

## Verification

- `amont run --stamp plugin-test`, with tests asserting exact hashes:
  - `scopedFileHash('/r/.git', 'src/lib.rs')` equals a pinned vector
    computed independently (`printf '/r/.git\0src/lib.rs' | shasum -a 256`);
  - an edit in a directory git calls "not a repository" yields no hash;
    an edit in a repository and one outside give exactly one hash;
  - the same `src/lib.rs` under two fake repositories (different common
    dirs) gives two different hashes;
  - a worktree path and its main checkout path, whose fake rev-parse both
    answer the same common dir, give one hash;
  - a timed-out or throwing rev-parse is not cached: a second resolution
    runs git again; a `null` answer is cached (git runs once);
  - a restore of an edited file hashes equal to the edit;
  - with real git in a temporary repo and `git worktree add`: both paths hash equal, and a root-level file (empty prefix) hashes;
  - a `hashFiles` timeout returns the partial list;
  - a `git revert` lookup hashes from `--git-common-dir`, with no
    rev-parse in the file's own directory (the directory may be gone);
  - a revert with paths only is queued before the task row, and inside
    the final batch when the session ends at once.
- Real git, piloted: in this worktree and in the main checkout,
  `git rev-parse --path-format=absolute --git-common-dir --show-toplevel
  --show-prefix` from `claude-plugin/` prints the same common dir and
  prefix (checked during planning: `/…/relais/.git`, `claude-plugin/`); in
  `~/.claude/projects/` it exits non-zero; under `/tmp` and `/private/tmp`
  for one scratch repository it prints the same common dir.
- Cost: `resolveRoot` cold and warm timed on a real session (expect cold
  around 5-10 ms, warm under 1 ms); the report states the measured values.
- Live, after the reinstall: t13's count reads the new rows; the t02 miss
  query run on rows after the cutover returns 0 false misses from absolute
  out-of-cwd paths (t02 found 45), with no stop list for memory files
  (folded into t13's report, no new ticket).

<!-- panel: repos=relais adds= reviewers=backend,lang:typescript,tui,unix body-sha=b54f8ff2dcf0 -->
