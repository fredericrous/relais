---
status: active
branch: feat/prune-verify-and-runs
repos: [relais]
canonical: decisions:docs/plans/2026-11-01-one-verdict-build.md
phases: [K1]
---
# One verdict per tree, build: K1 cleanup (pointer)

The canonical plan is decisions `docs/plans/2026-11-01-one-verdict-build.md`. This branch is phase **K1**: relais removes the empty `verify/<run>/` directories it leaves, and `relais resume --prune-runs [<days>]` prunes old `runs/<run>/` directories, previewing first.

## Verification record (K1)

Every test below is in this branch's diff.

- **Unit tests** in `crates/relais/src/prune.rs` (`prune::`, 5 passed):
  - `an_empty_tree_goes_and_a_file_keeps_its_ancestors`: removal is bottom-up with `remove_dir`. A file is never removed, and neither are its directory and every directory above it.
  - `the_sweep_removes_ended_and_old_unknown_empties_only`: ended runs go at once, unknown ones only after a day; live, unreadable and non-empty ones stay.
  - `a_preview_lists_old_runs_and_deletes_nothing_until_applied`
  - `a_directory_that_cannot_be_listed_is_never_judged_old`. Mutation-checked as uid 501: with `age()`'s unreadable-listing branch reverted, the test fails; with the fix, it passes.
  - `a_cache_entry_citing_a_run_is_counted`
- **Scenario tests** in `crates/relais/tests/it/release_scenarios.rs` (4 passed):
  - `a_leftover_verification_checkout_is_removed_by_resume` (extended): the emptied `verify/<run>/` goes too.
  - `a_finished_run_leaves_no_empty_verify_dir_and_pruning_previews_first` (new):
    - a finished run leaves no `verify/<run>/`;
    - `--prune-runs 30` does not list a fresh run;
    - a run without a receipt is kept, even with `--write`;
    - with a receipt, the dangling count rises by exactly 1 per citing row;
    - the preview deletes nothing, and `--write` deletes it.
  - `pruning_keeps_live_and_waiting_runs_and_names_dangling_rows` (new):
    - a `running` run and a `needs_review` run, each with a receipt and an old directory, get `keep` lines and survive `--write`;
    - a prunable run's dangling row is named by kind and path;
    - `--prune-runs` with `--log` exits 2, and the logged resume never runs.
  - `a_package_runs_leftover_verification_checkout_is_removed_by_resume` (unchanged): passes.
- **`make lint`:** fmt, clippy `--all-targets -D warnings` and the module-cycle scripts all pass (CYCLES: none).
- **Real state, read-only.** `RELAIS_STATE_DIR` was spelled `/tmp/…/k1-state4`, which is a symlink path, since `/tmp` is `/private/tmp`. The directory held a `.backup` copy of the ledger and a copy of `baseline-cache/`, with `runs/` symlinked to the real one. Stored paths were left as they are, real absolute paths, so a citation can match only through canonicalisation. The branch binary ran `relais resume --prune-runs` (default 30).

  | Input | Expected | Actual |
  |---|---|---|
  | preview of 134 directories older than 30 days | some prunable, the rest kept with a reason | 18 prunable (8.1 MB); kept: 98 no receipt, 10 `needs_review`, 5 `needs_decision`, 3 `interrupted` |
  | dangling rows, stored under the real path while the run is seen through the symlinked spelling | found through the canonical path, each named | 128 rows, each printed as `dangling <kind> <path>` (e.g. `context_manifest …/run-65c083902dd9a-32f2/manifest.json`) |
  | baseline-cache citations of the 134 | counted and kept | none |
  | files under the real `runs/` written during the preview (`find -newer start ! -newer end`) | none | 0 |

  stderr said "preview only: add --write to delete them". Exit 0.
  - **Two earlier rehearsals are not counted.**
    - One left the stored paths unrewritten while `RELAIS_STATE_DIR` named the scratch dir, so no citation could match.
    - One rewrote them to the scratch prefix while `runs/` resolved through its symlink to the real path, which again gave 0 rows.
    - Neither wrote under the real `runs/`. Its window showed one directory touched, by a live run's own `attempt1-cmd0.log`.
- **Not done here:** `--write` on the real state is the person's call after the merge: 18 directories and 8.1 MB are expected.

## Decision log

- 2026-11-01: a run directory cited by a baseline-cache entry is kept. Cached outcomes name their retry logs by absolute path under `runs/<run>/logs/`; 6 entries did on 11-01.
- 2026-11-01: a citation is looked for under both the directory's spelling and its canonical path. A symlinked state directory, or `/tmp` against `/private/tmp`, must not hide one.

## Implementation review

- **Round 1 on tree bb6524bd, by the parent: approve-with-changes.**
  - Fixed: [blocking] every `State` variant is matched explicitly in the prune judge, with no wildcard; `match prune` has one arm per variant.
  - Fixed: [high] the preview names each dangling evidence row by kind and path.
  - Fixed: [high] the live-run guard is tested.
  - Fixed: [medium] `--prune-runs` conflicts with `--log`.
  - Fixed: [medium] this record was rewritten.
  - Fixed: [low] `remove_empty_tree` removes bottom-up with `remove_dir`.
- The reviews recorded in this file before were run by a fork. A fork's reviews do not count, so none of them is recorded here.
