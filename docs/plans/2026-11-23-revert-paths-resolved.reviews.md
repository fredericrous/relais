# Full reviews: Recorded reverts name the files they revert

## Round 1 (body a14691338098)

- tui (opus): approve, 26k, 23 s. 1 low: run git with `--no-pager` and `GIT_TERMINAL_PROMPT=0` (accepted).
- unix (opus): approve-with-changes, 33k, 39 s. high: `-` in `cd` targets broke the worked case; high: subshell `cd` leak; high: option injection into `git show`; medium: C-quoted paths (`-z`); low: merge commits (first-parent). All accepted.
- backend (opus): approve-with-changes, 37k, 53 s. high: detached lookup vs session end/`/clear`; medium: failed-lookup share in the live check; medium: Verification not observable; medium: add backend profile; low: `-s`/`--source` values. All accepted.
- lang:typescript (opus): approve-with-changes, 38k, 57 s. high: `HEAD~1` resolves after HEAD moved; medium: subshell leak (rejected: duplicate of unix#2); medium: `--show-cdup` not `--show-toplevel` (symlinks); medium: missing tests; low: capture task/state. Rest accepted.

## Round 2 (body 3940e93a9e38), all resolved; carried into implementation

- lang:typescript: approve. Carried: unknown directory gives a row with no files and no git call; `--no-pager` goes before the subcommand; git ≥ 2.31/2.24 needs fail open.
- tui: approve. Carried: state the gate timeout; a never-resolving lookup delays `onSessionEnd` by at most it.
- unix: approve-with-changes. Carried: pass `GIT_TERMINAL_PROMPT` through `fx.process.run`'s `env`; drop empty NUL entries; state the session-end wait.
- backend: approve. Carried: unknown directory runs no lookup; the session-end race test in Phase 1's list.

## Codex (gpt, high effort), on body 3940e93a9e38

approve-with-changes. high, applied: (1) quote-aware splitting, heredoc commands record nothing; (2) `||`/pipeline reverts not attributed; (3) `:`-magic pathspecs leave the segment unresolved. medium, carried: (4) `-m N` must use parent N, or leave it unresolved; (5) `--no-renames --no-relative` on `git show`; (6) `--git-dir`/`--work-tree`/`GIT_DIR`/`GIT_WORK_TREE` overrides leave the segment unresolved.

## Codex delta (body bc249e7651e5)

- backend: approve. Carried: a revert followed by `;`/newline is not attributed either (only an `&&` chain to the end, or the last segment).
- tui: approve. Carried: the timeout value (as above).
- lang:typescript: approve-with-changes. Carried: tests for quoted `;`/`|`, heredoc, `|| true`, `| cat`, `:(exclude)`; "not attributed" means no row.
- unix: approve-with-changes. Carried: `;`/newline as backend; a lone `&` records no revert.
