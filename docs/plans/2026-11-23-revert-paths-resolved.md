---
status: active
branch: fix/revert-paths
repos: [relais]
adrs: []
programme: docs/programmes/phase8-learner
ticket: t10-revert-paths-resolved
plan-id: p-01M82WFYV4NG72Q655M49QQEJ1
---
# Recorded reverts name the files they revert

## Review panel

👉 **Decide:** none — approve if a revert's files should hash only when the command shows they were reverted, skipping anything unsure.
📍 relais · 2 phases · 1 release · 1 step for you · next: Phase 1 via relais. Panel: backend, lang:typescript, tui, unix (opus), codex.
**Changed by review:** subshell and quote-aware parsing, `||`/pipe/heredoc/`:` pathspecs unattributed; `git show` hardened (`-z`, `--end-of-options`, `--show-cdup`, no prompt); lookups keep their session and finish before session end.
**Verdicts:** round 2: 2 approve, 2 approve-with-changes; codex approve-with-changes (3 high applied); 14 carried; words 775 → 1044 (×1.35, growth from high and correctness fixes); remove 0, merge 0.
📄 Full reviews: `2026-11-23-revert-paths-resolved.reviews.md`

## Picture

```
 Bash: (cd /w/wt && git restore $P docs/a.md .)       session cwd: /w
                 |
        revertedPaths(command, cwd, taskFiles)
                 |
   dir = /w -> cd -> /w/wt      ($P skipped, never hashed)
                 |
   docs/a.md -> /w/wt/docs/a.md          (a file)
   .         -> task files under /w/wt   (a directory: expanded)
                 |
   git revert <sha> -> git show --name-only in /w/wt (async, noteRevert)
                 |
   fileHash(abs, /w)  ==  fileHash(edit path, /w)   <- same hash as the edit
```

One revert command, resolved: each path is made absolute against the
command's own directory, then hashed exactly as the task's edits are.

## Goal

A `revert` reassess row hashes the same strings as the task file hashes it
should match, so the missed-failure measure (phase8 t02) can see reverts.
Today none of the 93 recorded reverts matches any task: `revertedPaths`
hashes `$P` literally, resolves paths against the session cwd instead of the
command's `cd`/`git -C` directory, and records `.` and `git revert` with no
usable files.

## Non-goals

- Repository keys on hashes and dropping out-of-repository paths (t11).
- Correction rows carrying files (t12).
- Any Rust, ledger or wire change: the row shape (`files` of 16-hex hashes)
  stays as it is.
- Deciding whether a save-and-restore around a tool call is an "undo" (t02
  item 3 noted it; the measure's stop list handles noise).

## Code review

- relais: lang:typescript, unix, backend (tui left out: no output a person reads)

## Behaviour

`revertedPaths(command, cwd, taskFiles)` (in `claude-plugin/hooks/router.ts`)
splits the command on `&&`, `||`, `;`, `|` and newlines outside quotes only
(quoted text never becomes a command), with a leading `(` and trailing `)`
taken off; a command holding a heredoc (`<<`) records no revert. A revert
segment that follows or precedes `||`, or sits in a `|` pipeline, is not
attributed: the call's success does not show it ran and succeeded. It
tracks the command's effective directory:

- It starts at the shell's directory (the session cwd).
- A `(` saves the directory and its matching `)` restores it, as a
  subshell's `cd` does not outlive it; a `cd` in a `|` segment moves nothing.
- `cd <dir>` moves it, relative to the current one, for the segments after
  it; `cd` alone, `cd -`, or a target holding `$`, `~` or a backtick, makes it
  unknown until an absolute `cd` (a hyphen inside a name, as in
  `relais-wt-x`, is an ordinary directory).
- `git` global options before the subcommand are read: each `-C <dir>`
  composes on the directory for that segment; `-c <k=v>`, `--git-dir=…`,
  `--work-tree=…`, `--no-pager` and the like are skipped.

Each path argument of `git checkout … -- <paths>` and `git restore <paths>`
(unless it only unstages; the values of `-s`/`--source` are not paths) then:

- is skipped when it holds `$` or a backtick (never hashed);
- a pathspec starting with `:` (magic, `:(exclude)…`, `:/`) leaves the
  whole segment's pathspec set unresolved: no paths from it;
- is skipped when it is relative and the directory is unknown;
- is made absolute against the directory and normalised (`.`/`..`
  segments, repeated `/`, trailing `/`);
- when it is `.`, `..`, ends in `/`, or has any of the task's files under
  it, is a directory: it expands to the task's files beneath it (none: no
  path), and is never hashed itself;
- otherwise is the file itself.

`git revert <commits>` (not `--abort`, `--quit`; `--continue`/`--skip`
give no commits) yields its commits and the directory. Value-taking options
(`-m`, `--mainline`, `--strategy`, `-X`, `--strategy-option`, `--cleanup`)
are not read as commits, and a word starting with `-` or holding `$` or a
backtick is dropped. Without `-n`/`--no-commit`, a commit relative to HEAD
other than bare `HEAD`/`@` (`HEAD~1`, `@^`) is skipped: HEAD has moved by the
time it is resolved.

`noteRevert` in `routing.ts` resolves the commits off the hook's critical
path, with the `task`, `state` and session captured at the call (never
re-read from the store after the await). In that directory, with the gate
timeout, `--no-pager` and `GIT_TERMINAL_PROMPT=0` (a missing commit must
fail, not prompt): `git rev-parse --show-cdup`, then `git show -z
--name-only --format= [--diff-merges=first-parent when -m was given]
--end-of-options <commits>`. The output is split on NUL, and each file is
made absolute against the directory joined with the cdup (not
`--show-toplevel`, which resolves symlinks). A failing git still queues the
row, with no files. `endTask` and `onSessionEnd` wait for unfinished
lookups (each bounded by the gate timeout) before the task row and the
final send.

Each resulting absolute path is hashed with the unchanged
`fileHash(path, sessionCwd)`, the function task rows use, so a revert of a
file the task edited yields the same hash as the edit.

## Phases

- [ ] Phase 1 — resolve revert paths, with plugin tests for every shape
  (`$P`, `cd X &&`, `(cd X && …)`, `git -C X`, nested `-C`, `.`, `dir/`,
  `git revert <sha>` through a fake `fx.process.run`, a failing git still
  queuing the row, `git revert -m 1 <sha>`, `git revert HEAD~1`, `--abort`,
  `--staged` alone, a subshell `cd` not leaking, an unknown `cd`): `claude-plugin/hooks/router.ts`, `claude-plugin/hooks/routing.ts`,
  `claude-plugin/tests/router.test.ts`, `claude-plugin/tests/routing.test.ts`
- [ ] Phase 2 — close the ticket and file the live count as a dated check
  after the release and the plugin reinstall:
  `docs/programmes/phase8-learner/t10-revert-paths-resolved.md`,
  `docs/programmes/phase8-learner/map.md`,
  `docs/programmes/phase8-learner/t13-revert-overlap-live-count.md`

### 🧑 Your steps
- `/plugin` reinstall of relais after the next release — because the hooks
  run from the installed plugin (0.72.0), and the live count measures only
  rows the new code wrote.

## Verification

- `amont run --stamp plugin-test`: each shape's test asserts its exact list
  of hashes (e.g. `git -C a -C b restore x` gives fileHash(`<cwd>/a/b/x`)); a
  `git revert` through a fake run that resolves after `onSessionEnd` starts
  puts the row in the final batch under the original session.
- Worked case from t02 (session 943bdf5e): `cd <wt> && git restore $P` gives
  no path; `cd /…/relais-wt-replay-floors && git restore docs/plans/x.md`
  with session cwd `~/Developer/Perso` hashes the same as an Edit of
  `/…/relais-wt-replay-floors/docs/plans/x.md`.
- Live (Phase 2's dated ticket): new `revert` rows after the reinstall,
  counted by whether they overlap their own task's files, with rows from a
  `.`/directory expansion reported apart (they overlap by construction), and
  `git revert` rows with no files paired with their transcript commands to
  give the share whose lookup failed (expected: under 10%).

<!-- panel: repos=relais adds= reviewers=backend,lang:typescript,tui,unix,codex body-sha=bc249e7651e5 -->
