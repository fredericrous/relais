---
status: active
branch: feat/prune-verify-and-runs
repos: [relais]
canonical: decisions:docs/plans/2026-11-01-one-verdict-build.md
phases: [K1]
---
# One verdict per tree, build: K1 cleanup (pointer)

The canonical plan is decisions `docs/plans/2026-11-01-one-verdict-build.md`. This branch is phase **K1**: relais removes the empty `verify/<run>/` directories it leaves, and `relais resume --prune-runs <days>` prunes old `runs/<run>/` directories, previewing first.

## Verification record (K1)

- **Unit tests** (`prune::`, 4 passed):
  - an empty tree goes, and a tree holding a file stays whole;
  - the sweep removes ended runs' directories, and unknown ones only after a day; live, unreadable and non-empty ones stay;
  - a preview lists and deletes nothing, and applying deletes only the prunable;
  - a cache citation is counted.
- **Scenario tests** (`it`, 8 passed, `resume` filter):
  - `a_leftover_verification_checkout_is_removed_by_resume` now also asserts that the empty `verify/<run>/` goes;
  - `a_finished_run_leaves_no_empty_verify_dir_and_pruning_previews_first`:
    - a finished run leaves no `verify/<run>/`;
    - `--prune-runs 30` does not list a fresh run;
    - a run without a receipt is kept, even with `--write`;
    - with a receipt, the run is listed as prunable, and the dangling count rises by exactly 1 per citing row;
    - the preview deletes nothing, and `--write` deletes it and says so on stderr.
- **Real state, read-only.** `RELAIS_STATE_DIR` pointed at a scratch dir holding a `.backup` copy of the ledger and a copy of `baseline-cache/`, with `runs/` symlinked. The stored absolute paths were rewritten to the scratch prefix in the copies, in 8,901 evidence rows and in the cache entries, so the prefix checks can match. The branch binary ran `relais resume --prune-runs` (default 30). The first attempt, without the rewrite, could not match any path, so its "0 dangling" was not a measurement and is not recorded here.

  | Input | Expected | Actual |
  |---|---|---|
  | preview of 134 directories older than 30 days | some prunable, the rest kept with a reason | 18 prunable (8.1 MB, 128 evidence rows left dangling); kept: 98 no receipt, 10 `needs_review`, 5 `needs_decision`, 3 `interrupted` |
  | any of the 134 cited by a baseline-cache entry | counted and kept | none: no `cited by` line |
  | `runs/` afterwards | untouched | 1033 directories before and after |

  stderr said "preview only: add --write to delete them". Exit 0.
- **`make lint`:** pass (CYCLES: none). Clippy `--all-targets -D warnings`: clean.
- **Not done here:** `--write` on the real state is the person's call after the merge. It frees about 8 MB today, because most old runs have no receipt in the ledger.

## Decision log

- 2026-11-01: implementation review round 1 fixes:
  - `Prune` enum instead of a bool;
  - a `runs/` read error exits non-zero, and unreadable entries are named;
  - talk on stderr;
  - the 30-day default;
  - a preview no longer sweeps verify directories;
  - a deterministic integration test;
  - the export under its own name.
- 2026-11-01: a run directory cited by a baseline-cache entry is kept. Cached outcomes name their retry logs by absolute path under `runs/<run>/logs/`; 6 entries do today.
- 2026-11-01: Delta review: all eight round-1 findings resolved, and the real-state numbers corrected as above.
  - `if prune == Prune::Preview` stays an equality test: `deliberate:` the enum has two variants, the early return reads as a guard clause, and a third variant would also need its own arm in `apply`.
  - Whoever runs `--write` on the real state records what it deleted: 18 directories and 8.1 MB are expected.

## Implementation review

- Approve-with-changes after a Delta. Round 1 (66k tokens, 72 s): 3 blocking, 1 high, 3 medium and 1 low finding, all fixed. Delta (95k tokens, 48 s): all resolved, plus one medium finding (the real-state numbers were not measured) and one low finding.
- Fixed: the numbers were re-measured with path-matched copies.
- Deliberate: the equality test on the two-variant `Prune`.
- Next: PR, merge on green. No release until S2.
- 2026-11-01: rebased review (tree b9692e68), low finding fixed: `age()` read a hidden entry or listing with `flatten()`, which made a directory look older than it is. An unreadable entry or listing now counts as modified now, and `a_directory_that_cannot_be_listed_is_never_judged_old` covers it. `make lint` clean after the change; `prune::` 5 passed; the scenario test passes.
