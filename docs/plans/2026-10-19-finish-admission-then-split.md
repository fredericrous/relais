---
status: active
branch: feat/resource-admission (PR 2), feat/verification-evidence (PR 3), then one branch per split PR
repos: [relais]
adrs: [relais ADR-0010, ADR-0011, ADR-0030, ADR-0040]
---
# Plan: relais, finish resource admission, then split three large modules

## Review panel

👉 **Decide:** none. Approve if this order holds: land PR 2 (rebased, ledger steps renumbered to v39/v40), build PR 3, release v0.27.0, then four split PRs (a preparatory test PR and A, B, C), move-only, and a patch release.
📍 relais · reviewed, nothing built · next: rebase `feat/resource-admission` onto main. Panel: backend (opus-5-5), codex (gpt).
**Changed by review:** a measured check that no branch binary touched the real ledger; a move-only check per commit (moved-line diff); test names compared, not counts; a budget, freeze and conflict policy for the split; a test-only commit first that makes the source-inspection test layout-independent.
📄 Full reviews: [2026-10-19-finish-admission-then-split.reviews.md](2026-10-19-finish-admission-then-split.reviews.md)
**Verdicts:** backend approve-with-changes four times (round 1, round 2, a binding pass, the final bind); codex approve-with-changes. **5 carried into implementation**, applied in the PR they concern:
- the move-only script drops `use`/`mod` lines and strips a leading `pub`, `pub(super)` or `pub(in …)`;
- a scripted check over added lines finds no new `pub(crate)` (rustdoc JSON cannot see one);
- step 0's "extracted layout" is a temporary fixture tree, since PR A does not exist yet;
- the budget is 51 commits (A 21, B 14, C 16), with a time and spend ceiling set from the first measured run;
- before PR A, LSP settles whether `init` is a top-level variant (it stays flat) or belongs with `doctor`.

## Context

The SOLID audit (2026-10-15) left one finding open: relais's three largest files each have several reasons to change. Today on `origin/main` (5d412a4f):

| File | Total | Tests | Production | Shape |
|---|---|---|---|---|
| `crates/relais/src/main.rs` | 10,941 | 1,190 | 9,750 | about 25 subcommands' handlers in one binary file; `dispatch` at `:1372` |
| `crates/relais/src/ledger/mod.rs` | 11,575 | 4,393 | 7,180 | one `Ledger` with a 3,700-line `impl`, plus migrations (1,040 lines of SQL data) |
| `crates/relais/src/runner/mod.rs` | 13,911 | 7,860 | 6,050 | one 4,400-line `impl RunEngine` spanning preflight, launch, attempts, verification, review and evidence |

An unfinished programme edits these same files. That is resource admission, plan `docs/plans/2026-10-14-runs-survive-a-loaded-machine.md`: reviewed, PRs 0 and 1 merged. Its PR 2 is built but exists only as 8 local commits in `relais-wt-resource-admission` (+746 lines in `runner/mod.rs`, +293 in `ledger/mod.rs`, +72 in `main.rs`, plus the new `verify/heavy.rs`), with no implementation review yet. PR 3 has not been started. A move-only split and those edits cannot both go first without one of them re-applying its changes by hand.

The person decided the order (2026-10-19): "finish the resource-admission programme. take on the work and merge all, then do the split". No live session holds that worktree (ListAgents, 2026-10-19).

The intended outcome:
- The programme is shipped and observed live.
- Each of the three files becomes a thin facade over child modules, each module with one reason to change.
- No behaviour, CLI, `--json`, exit code, ledger schema or public library API changes in the split.

## Design

### Part 1: finish the programme (its own plan stays canonical)

The programme's plan was reviewed by its own panel (body-sha `29618604254a`), and this plan does not redesign it. Its Verification items 4–11 and Delivery section govern PR 2, PR 3 and the release. What has changed since it was written, and what this plan adds:

- **PR 2's ledger steps collide with main.** Main went from schema 36 to 38 with steps `v37` and `v38` (#259, #262). PR 2 also takes 36 → 38 under the same names. On rebase, PR 2's two steps become `v39` and `v40`, and `LEDGER_SCHEMA_VERSION` becomes 40. Both are expand-only, as before. `CHANGELOG.md` also conflicts and is merged by hand. No other file conflicts (`git merge-tree`, 2026-10-19).
- **The release number moved.** v0.25.0 and v0.26.0 went out meanwhile, so the programme's release is v0.27.0, or the next free number when it is cut.
- **The worktree changes owner.** This session continues `feat/resource-admission` in place, from its last commit `64ac012a`. Before the rebase, `git status` must be clean. The branch then gets a backup ref (`git branch backup/resource-admission-64ac012a`) so the rebase can be undone.

### Part 2: the split, after v0.27.0 is out

Principles, one for all three files:

- **Move only, two hats** (`refactoring.two-hats`). A commit moves one cluster and fixes visibility, nothing else. No rename, no signature change, no logic change. A smell found on the way is written in the Decision log, not fixed.
- **One reason to change per module** (`solid.single-responsibility`), not a line target (`modules.deep`: line count is not depth).
- **Visibility widens only inside its tree:** to `pub(super)` or `pub(in crate::<module>)`. Every item already reachable from outside keeps its path through a `pub use` in the facade. Nothing new becomes `pub` in the library. Clap types and `CliError` stay in the binary.
- **Tests move with their cluster.** The number of tests in each test binary is unchanged (ADR-0040 `code.test-removal-evidence`: nothing is deleted, so no mutation run is owed).
- **Prepare source-inspection tests before PR A.** In a separate test-only preparatory commit, make `nothing_a_model_can_reach_names_a_trials_arm` inspect `main.rs` plus the complete binary CLI source tree, and locate the actual `NewReplayTrial` declaration throughout the ledger source tree. Resolve source roots independently of the test file's location, supporting both the original and extracted layouts. Keep every existing assertion and fail if the declaration or expected source tree is missing. Demonstrate with temporary negative controls that forbidden identifiers in an extracted CLI child and forbidden fields in the relocated declaration still fail. This preparatory commit is reviewed separately from the move-only commits and is exempt from their line-multiset equality check; record its evidence before and after PRs A and B.

**PR A: `main.rs` into `src/cli/`**, a module tree of the binary declared from `main.rs` (`mod cli;`), not added to `lib.rs`.
- `cli/mod.rs`: `Cli`, `Command`, `dispatch`, and the body of `main` (protocol, `catch_unwind`, exit).
- `cli/outcome.rs`: `CliOutcome`, `CliError`, `exit_code`, `operational`, and the module doc's exit-code table.
- `cli/ctx.rs`: the shared plumbing at `:3879-4180` (`load_repo_policy`, `load_machine`, `load_contract`, `open_ledger`, `project_dir` …).
- `cli/protocol.rs`: `REFUSAL`, `refuse_run`, `announce_unstarted`, `open_protocol`.
- One file per command group (existing clap variants keep their shape; flat ones such as `Doctor` and `Init` stay flat; no new enums): `install`, `learn` (train, evaluate, promote, dataset, rollback), `recipe`, `coordinator` (with `quiet`), `trust`, `doctor` (with `hook` and `init`), `plan`, `native` (status, cancel, router, usage, gate), `run`, `replay`, `status` (with `explain`), `resume` (retire, reconcile), `report` (with `usage import`), `feedback`, `decide`, `salvage` and `evidence`.
- `main.rs` keeps `mod cli;` and `fn main`. The 25 unit tests in `mod tests` move with the handlers they test.
- Moving order: outcome, ctx, protocol, then leaf commands (evidence, salvage, decide, feedback, report), then the rest, then the entangled ones last (native, resume, status, plan, replay, run), then `dispatch`.
- Before each move, LSP findReferences on cross-cluster helpers (`native_gate`, `acknowledged`, `reap_if_driverless`, `retire_*`).

**PR B: `ledger/mod.rs` into `impl Ledger` children**, the pattern `router.rs`, `review.rs`, `successor.rs` and `task_types.rs` already use (`use super::{Ledger, Result}`, their own `impl Ledger`).
- `mod.rs` keeps `Ledger` (`conn`, `path`, `clock`), `open*`, `write_tx`, `retry_while_busy`, `now`, `path` and the `pub use` re-exports.
- New children:
  - `error.rs` (`LedgerError`, `parse_*`) and `clock.rs`;
  - `schema/mod.rs` (`migrate`, `apply_step`, `prepare_*_step`) and `schema/migrations.rs` (`MIGRATIONS` moved verbatim, same order, same `LEDGER_SCHEMA_VERSION`);
  - `runs.rs` (runs, attempts, contract revisions, models, workspace, tasks);
  - `transitions.rs`, holding transitions and decisions together (`append_transition`, `write_transition`, `resolve_decision_in`, salvage, landing, nonces, signoffs), because they share transactions;
  - `trust.rs`, `evidence.rs` (with receipts), `usage.rs` (usage events, dispatches, bookings, cost queries, `COVERED_BY_AN_INCLUSIVE_PARENT`);
  - `outcomes.rs` (with reverts), `trials.rs` (with recipe promotions), `learning.rs` (both learning `impl` blocks merged) and `sessions.rs` (predictions, features, orchestration usage).
- Row mappers stay with their only callers.
- The 94 tests split with their clusters. Shared fixtures (`temp_ledger`, `run`, `new_trial`, `v26_ledger_with`) go in `#[cfg(test)] mod test_support`.

**PR C: `runner/mod.rs` into phase modules.**
- First commit: `mod tests` moves out whole to `runner/tests/mod.rs` (7,860 lines), beside the existing `tests/native.rs` and `tests/verify_kind.rs`.
- Then the pure leaves, with no `RunEngine`: `error.rs` (`RunError`, its `From`s, `Finish`), `config.rs` (`RunConfig`, `DesignSource`, the path helpers), `prompt.rs` (`build_prompt`, `worker_rules`, `data_block`, `fence_safe` …), `spend.rs` (`RunSpend`, `Ceiling`, `spend_limit`, `Progress`), and `review/types.rs` (`ReviewRequest`, `ReviewOutcome`, `review_verdict` …).
- Then `types.rs` (the phase data: `Preflight`, `Baseline`, `AttemptContext`, `Candidate`, `Verified` …).
- Then `impl RunEngine` split by phase, the separable seams first: `evidence.rs` (D10), `verification.rs` (acceptance and `verify_candidate`), `baseline.rs`, `launch.rs` (admission and `launch_admitted`), `workspace_ops.rs` (in place, retire), `review/engine.rs`; then `preflight.rs`, `attempt.rs` and `engine.rs` (the struct, `step`, `run`, `run_inner`).
- `RunEngine`'s private fields widen to `pub(super)`. `scheduler.rs` imports from the new paths; items it imports from nested modules (e.g. `review/engine.rs`) use `pub(in crate::runner)`, not `pub(super)`.

**Not in this plan:** reshaping `RunConfig` (35 fields), `Progress` (shared mutable state) or `RunEngine` itself. Those are design changes with behaviour risk, and they get their own plan if the split shows they are the next problem. The other large files (`admission`, `verify`, `report`, `doctor`, `route`, `coordinator`; 3.3k–5.5k lines each) are measured after the split, not split now.

## Changes

### Phase 1: PR 2, resource admission
1. In `relais-wt-resource-admission`: check that the tree is clean, add the backup ref, then `git rebase origin/main`. Renumber the two ledger steps to `v39` and `v40`, and set `LEDGER_SCHEMA_VERSION = 40`. Merge the `CHANGELOG.md` conflict.
2. This machine's real ledger was checked before planning (a scratch copy, 2026-10-19). Its `schema_migrations` holds main's `v37` (2026-10-16) and `v38` (2026-10-17), with `landings` and `landing_searches`, and none of PR 2's tables (`group_bindings`, `run_epochs`, `check_disk_growth`). So no branch binary migrated it. The branch's own 38 exists only in test scratch directories, which are disposable, and no repair path is built. Re-check that just before the rebase. Add a migration test: a ledger built by main's 38 steps upgrades to 40 and holds PR 2's three tables. Re-run the gate.
3. Run the implementation review (worktree-task F4b), then rehearse, `amont land`, open the PR, and merge-when-green.

### Phase 2: PR 3, verification evidence
1. New branch `feat/verification-evidence` from `origin/main`. Build C1–C5 as the programme plan specifies, through relais runs (one bounded contract per item).
2. Record the PR 3 Outcome in the programme plan, set it `done`, and run the implementation review. Land, PR, merge.

### Phase 3: release and live checks
1. Release v0.27.0 through tag-release, then install it on this machine: `relais install --claude --user --write`, `relais coordinator stop`, and reinstall the plugin.
2. Run the programme plan's live step 11 and its deferred live check from Verification 0.

### Phase 4: the split
0. Preparatory test-only PR: make `nothing_a_model_can_reach_names_a_trials_arm` layout-independent (see Design), reviewed and merged before PR A.
1. PR A (`main.rs`), PR B (`ledger`), PR C (`runner`), in that order. Each starts on a new branch from `origin/main` after the previous one merges. While a split PR is open, no other PR touching its file merges; a conflict means rebase and re-run the move-only check, not a hand merge. Each cluster move is routed through relais (`task_type: refactor`, write scope the moved file and its new module), one cluster per commit.
2. Budget: about 40 cluster commits in total. Cap each relais refactor run at one retry; after two failures the move is applied by hand. Record the wall-clock time and retries per commit in each PR's Outcome.
3. A patch release after PR C, installed on this machine, then one live relais run.

## Verification

- Phase 1:
  - The new migration test passes: a ledger at main's v38 upgrades to v40 and holds PR 2's tables.
  - The programme plan's Verification items 4–7 still pass after the rebase (their tests are named in its PR 2 Outcome).
  - `make check` is green, and so is CI on all three platforms.
- Phase 2: the programme plan's Verification items 8–10 pass.
- Phase 3: its item 11 holds on this machine, `relais --version` shows the release, and the Verification 0 live check passes.
- Preparatory PR: the test passes on the original layout and on the extracted layout, and fails on each temporary negative control (a forbidden identifier in an extracted CLI child; a forbidden field in the relocated `NewReplayTrial`). Evidence is recorded before and after PRs A and B.
- Phase 4, for each split PR:
  - Each new child module's `use` dependencies are listed and checked against `scripts/check-module-cycles.py` before extraction, and the script passes after.
  - The sorted test names in each test binary (`cargo test -- --list`), with module prefixes stripped, are equal to those on `origin/main` before the PR, with 0 failed.
  - `make check` and CI are green.
  - The library's effective public API (item paths and signatures, e.g. a rustdoc JSON diff) is identical before and after; a moved `pub` item is not a new one, and no item gains `pub` or `pub(crate)` beyond what the facade re-exports.
  - Move-only, per commit: `git diff --color-moved=dimmed-zebra --color-moved-ws=allow-indentation-change` shows no non-moved lines except `use`, `mod` and visibility edits; a script comparing removed and added non-blank lines (those tokens excluded; whitespace trimmed; `super::` and `crate::` qualifiers normalised) reports an empty difference.
  - PR A: every `tests/golden` file passes unchanged, and `relais --help` plus each subcommand's `--help` are byte-identical to before.
  - PR B: a fresh ledger's `sqlite3 <file> .schema` dump is byte-identical to before, `LEDGER_SCHEMA_VERSION` is unchanged, and the migration tests pass.
  - PR C: `tests/release_scenarios.rs` and the runner's 190 tests pass, and `scheduler.rs` compiles against the new paths.
  - Afterwards, each of the three files is a facade. The size of every new module is recorded in the Outcome.
- After the patch release: `relais --version` shows it, and a relais run started from the plugin reaches a terminal state with its events in the pane.

## Outcome (Phase 4, step 0)

Branch `test/layout-independent-source-scan`, built by relais run-65e417ee0e4c5-de6c (v0.27.0: the five gates ran as separate checks and passed, the first live use of C3's profile). `nothing_a_model_can_reach_names_a_trials_arm` now calls `check_arm_boundary`, which walks the CLI tree (`main.rs` and `src/cli/`), the contract tree and the ledger tree at run time; the fixture controls run the same function. The controls are kept as permanent tests, not temporary ones: they cost milliseconds and guard the check itself.

| Test | Input | Expected | Actual |
|---|---|---|---|
| `nothing_a_model_can_reach_names_a_trials_arm` | this repository's own layout | passes | passes |
| the same, with a planted `crates/relais/src/contract/zz_plant.rs` holding `arm_index` (then removed) | a real contract child | fails | `the task contract must not name `arm_index` (found in …/crates/relais/src/contract/zz_plant.rs)` |
| `a_clean_extracted_layout_passes_the_arm_boundary_checks` | fixture: `main.rs`, `cli/run.rs`, `contract/mod.rs`, `ledger/trials.rs` with `NewReplayTrial` | passes | passes |
| `a_forbidden_identifier_in_an_extracted_cli_child_is_reported` | `arm_index` in `cli/run.rs` | error naming it and the file | passes |
| `a_forbidden_identifier_in_a_nested_cli_file_is_reported` | `arm_index` in `cli/learn/train.rs` | error naming it and the file | passes |
| `a_forbidden_identifier_in_a_contract_child_is_reported` | `assignment_probability` in `contract/child.rs` | error naming it and the file | passes |
| `a_seed_field_in_a_ledger_child_new_replay_trial_is_reported` | `pub seed` in `ledger/trials.rs`'s `NewReplayTrial` | error naming `seed` and the file | passes |
| `a_ledger_tree_without_the_declaration_is_an_error` | no `NewReplayTrial` in the ledger tree | error | passes |
| `a_missing_contract_tree_is_an_error` | no `contract/` | error | passes |

Review: approve-with-changes, then the Delta (28k/56 s, 31k/28 s). Fixed: the real test and the controls share `check_arm_boundary`; two more controls; the optional `cli/` root carries a `holds-until:` (required from PR A on). Evidence after PRs A and B is recorded in their Outcomes.

## Outcome (Phase 4, PR A)

Branch `refactor/split-main-into-cli`. `main.rs` went from 10,941 lines to 10: its doc, `mod cli;` and `fn main`. The binary's module tree `src/cli/` holds `mod.rs` (`Cli`, `Command`, the shared clap value enums, `main`, `dispatch`), `outcome.rs` (the exit model and its table, with its tests), `ctx.rs` (shared plumbing), `protocol.rs`, and one file per command group: evidence, salvage, decide, feedback, report, trust, install, doctor, coordinator, learn, recipe, replay, native, resume, status, plan and run. The library is untouched: `git diff --stat origin/main -- crates/relais/src/lib.rs` and every library module are empty, so its public API cannot have changed (this stands in for the rustdoc-JSON diff).

| Check | Expected | Actual |
|---|---|---|
| `scripts/check-move-only.py` on each move commit | code only moved | `9f2d55e1`, `d340e027`, `ebd1be16`, `aeb77e1b`, `1bd333fc`, `a3ac9ea3`, `811c8f14`: yes. `8de9bf5c`: differs by the `fn main() { cli::main() }` shim and one equivalent path (`report::Dimension` → `relais::report::Dimension`), read by hand; the script cannot see path prefixes, which it strips |
| visibility (the script's report) | nothing new is `pub` | 246 items private → `pub(crate)`, none `pub`; in a binary crate `pub(crate)` reaches only `main.rs` and `cli/` |
| comments added | module docs only | one-line `//!` headers per module; `main.rs`'s doc now says it only wires `cli/` |
| `--help` of the binary and all 64 subcommands, recursively (scratch `RELAIS_STATE_DIR`) | byte-identical | identical (`diff -r` over 65 files) |
| sorted test names, all targets, at `e2496dd3` (after the tidy) | origin/main's set | its 2,053 plus `a_missing_cli_tree_is_an_error`; none lost |
| `scripts/check-module-cycles.py` | no cycle | 47 modules, no cycles |
| arm-boundary scan | `src/cli/` now required | `cli_roots` requires it; `a_missing_cli_tree_is_an_error` |
| implementation review | approve | approve-with-changes, then Delta approve-with-changes (26k/36 s, 34k/24 s); every finding fixed or recorded |
| each group's relais run (`make lint/test/msrv/audit/plugin`, `tests/golden` included) | accepted | 8 runs, all accepted at the first attempt, no retry; about 20 min each (checks 2 × 9.4 min, worker 1–3 min), $0.13–0.40 each |

## Decision log

- 2026-10-19, Phase 1: PR 2's steps renamed v39/v40 on rebase (main's v37/v38 shipped first); merged as #267. Its first Windows run failed on a `kill -9 $$` case (no SIGKILL on Windows), now unix-only.
- 2026-10-20, Phase 2: running the branch's relais (`cargo run -- doctor`) migrated this machine's real ledger from 38 to 40 (PR 2's three tables, empty). Brew's relais 0.26.0 then refused the ledger. The person chose to install main's relais (`c49141de`) in `~/.local/bin` until v0.27.0; it is to be removed after the brew upgrade. A branch relais runs only with `RELAIS_STATE_DIR` pointed at a scratch directory.
- 2026-10-20, Phase 2: the semantic reviewer's dispatch exits 1 at once in this relais (C2, C4, C5: `review_unavailable`). The orchestrator read each patch and approved on `make check`; the PR's implementation review was the first independent read. Follow-up: diagnose the reviewer launch.
- 2026-10-20, Phase 2: until v0.27.0 is installed, relais cannot run in this repository (its `relais.toml` uses `weight` and `retry`), so C3 and the review fixes were made by hand.
- 2026-10-20, Phase 4: the first relais refactor run took 20 min, 19 of them the five checks on the base and on the candidate, so one cluster per run would cost about 7 h for PR A and 17 h for the split. The ceiling set from it: one cohesive group of clusters per run, at most 6 runs per PR, each commit checked move-only. PR A overran it with 8: the last group split in two because the clap definitions and dispatch were too entangled for one worker, and a tidy moved leftover plumbing to `ctx.rs` and replaced glob imports. PRs B and C keep to 6 by planning the leftovers into their groups (each group's contract names its helpers and imports), and an overrun there is a stop and a report, not a seventh run. Commits are per group, not per cluster.
- 2026-10-20, Phase 4: the move-only check is `scripts/check-move-only.py` (token multiset; impl splits, rustfmt's end-of-line commas, moved test modules and `use` statements are structure; visibility changes are reported). It strips path prefixes, so a changed path is read by hand.
- 2026-10-20, Phase 4: in the binary (PR A) visibility widens to `pub(crate)`, not `pub(super)`/`pub(in crate::cli)`: a binary crate's `pub(crate)` reaches only `main.rs` and `cli/`, so the two are the same set. PRs B and C are in the library, where `pub(crate)` reaches every module, so they keep the written rule.
- 2026-10-20, Phase 4: deliberate for PR A: the move check keys visibility on kind and name, so two same-named items moved in one commit with different visibility could hide a widening. Fixed first on PR B's branch (key on the enclosing type too).

<!-- panel: repos=relais reviewers=backend,codex body-sha=4b2cbf579c05 -->
