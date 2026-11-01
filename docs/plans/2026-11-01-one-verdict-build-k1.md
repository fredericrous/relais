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
- **Real state, read-only.** `RELAIS_STATE_DIR` pointed at a scratch dir holding a `.backup` copy of the ledger, with `runs/` and `baseline-cache/` symlinked. The branch binary ran `relais resume --prune-runs` (default 30):

  | Input | Expected | Actual |
  |---|---|---|
  | preview of 134 directories older than 30 days | some prunable, the rest kept with a reason | 18 prunable (8.1 MB, 0 dangling evidence rows); kept: 98 no receipt, 10 `needs_review`, 5 `needs_decision`, 3 `interrupted` |
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
