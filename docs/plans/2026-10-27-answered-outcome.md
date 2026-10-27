---
status: active
branch: feat/answered-outcome
repos: [relais]
adrs: [ADR-0002, ADR-0003]
---

# relais learns from what it can judge: an `answered` outcome, a parent link, unmatched checks, and 26 replays

## Review panel

👉 **Decide:** none — approve if `answered` (never a label, question/review only in backfill), a parent link, counted unmatched checks and 26 replays make the loop honest.
📍 relais · plan reviewed · next: Phase 0 dry runs, then the v0.36.0 branch. Panel: architect, backend, lang:rust, po, tui, unix (opus-5-5), codex (gpt).
**Changed by review:** install binary before plugin + version check; `maybeWrote`/aborted keep `unknown`, backfill 277 rows; unmatched prefixes safe and per repository.
📄 Full reviews: [2026-10-27-answered-outcome.reviews.md](2026-10-27-answered-outcome.reviews.md)
**Verdicts:** round 1: 6 approve-with-changes (1 blocking, fixed); round 2 backend approve-with-changes ×2; codex approve-with-changes (1 high applied); 9 findings carried into implementation.

## Context

`relais report --since 2026-10-01` (2026-10-27) says the loop has learned nothing: the last learn cycle (05:12) promoted nothing (0 test records for the selected route, abstention 1.00, negative calibration temperature); outcome-grade labels are 25 yes / 87 no, and the test-split floor projects to about 2027-10-03. Session routing reports `410 completed, 15 corrected, 430 unknown (unknown rate 50%)`, and `counterfactual gap: 83 accepted task(s) lack a cheaper arm`.

An inspection run (relais run-65ed17afa8b1e-6662, accepted) and two read-only explorations found:

- **Why unknown.** 432 unknown rows (857 tasks since 2026-10-01): 179 subagent review tasks, 109 main-session questions, 118 main-session debug/edit/design/review tasks, 26 other subagent tasks. Only one has a recorded check. Live ledger: **374 of the 433 unknown rows have no edited files** (`files_json = '[]'`); 59 edited something. `outcomeOf` (`claude-plugin/hooks/router.ts:1065-1071`) has no outcome for "nothing to verify", so a question and an unverified edit look the same. A subagent inherits its parent's outcome only when that outcome is not unknown (`routing.ts:557-567`), and the parent link is not stored, so the 205 subagent unknowns cannot be explained. A Bash command that does not match the checks list leaves no trace (`evidenceOf`, `router.ts:~776-790`), so "checks not matched" cannot be measured.
- **The gap.** Of the 83 gap tasks, only **26 can be replayed today**: all in relais, with an accepted (not salvaged) source run, the current verification profile hash (`8c60286c96aa…`; replay blocks on a changed hash, `cli/replay.rs:~190-205`) and a base commit still in the object store (checked with `git cat-file -e`). Types: refactor 11, review-fix 6, feature 4, mechanical 2, pattern 2, ui 1. The other 57: 23 relais tasks under an older profile hash or with no plain accepted run, and 34 in 15 repositories whose `relais.toml` has no `[[recipes]]` (`validate_candidate` requires a revision bump of an existing recipe, `route/candidate.rs:535-760`).

The person chose (2026-10-27): reduce unknown outcomes and run the replays first, before any Laya work; treat questions and no-edit tasks as a distinct `answered` outcome, reported apart from unknown and never used as a learning label.

**Intended outcome:** `relais report` separates `answered` from `unknown` (unknown rate from about 50% to about 18% on today's rows: 277 backfilled, 156 left), the remaining unknowns are the tasks that edited code, or could have, without a recognised check, a subagent's unknown can be traced to its parent, unmatched check commands are counted by program, and 26 cheaper-arm observations enter the dataset.

## Phase 0: replay the 26 tasks (operations, no code)

- **Recipe.** One candidate file outside the repository (the session scratchpad), `relais-replay-research.toml`: the current `relais.toml` unchanged, then appended `[[recipes]]` revision 2 entries at `tier = "research"` for each typed base recipe (`change-mechanical`, `-pattern`, `-review-fix`, `-feature`, `-refactor`, `-ui`, and the three `-designed` variants), each with its base's `kind`, `scope_within = ["**"]` and `task_type`. No untyped `change-routing` entry (refused at `candidate.rs:650-672`).
- **Dry run first, all 26** (`relais dataset replay --task <id> --recipe <file> --dry-run` from the relais main checkout; spends nothing, allowed outside the plugin). Record per task: exit code, covering recipe, tier, model, or the refusal (stderr). A dry run exits 0 even when no recipe covers the task (`cli/replay.rs:271-287`), so a task is kept only when exit is 0, the `candidate recipe:` line names a recipe, and the `arm:` line says `tier research`. A task whose routing floor keeps it above research is dropped, not forced.
- **Spend through the plugin only**, one `mcp__relais__replay {task, recipe, cwd: ~/Developer/Perso/relais}` at a time, waiting for each outcome. Replay has no dedupe, so the task list is kept in the plan's verification record and a task is never launched twice. The machine ceilings bound each run; this plan also stops at **$20 cumulative** (estimate: 26 × $0.44 ≈ $11.40, the gap's own per-task mean).
- **Then** `relais learn` runs at its scheduled time; the plan records `counterfactual gap` before (83) and after (expected about 57, if every replay settles).
- **Not done here:** the 57 others. They need recipes in 15 repositories' `relais.toml` and, for 23 relais tasks, a profile hash that no longer matches; both are listed for a later plan.

## Phase 1: the `answered` outcome (plugin + Rust)

- **Rule.** A task ends `answered` when it is not corrected, not verified, not accepted, has no explicit accept quote, **edited no file**, and did nothing that could write one the hook cannot see: no Bash writer (`sed -i`, a `>` or `tee` redirection, `git apply|checkout|commit|merge|rebase`, a formatter with a write flag), no subagent the router did not create (its edits are ignored, `routing.ts:474-482`), and no relais run or replay launched. Any of those sets a sticky `maybeWrote` flag on `TaskState`; with it, or with an edited file and no verification, the task stays `unknown`. After the existing corrected, verified and explicit-accept rules, an `aborted` entry in `task.inferred` also prevents the `answered` fallback (`router.test.ts:289` keeps expecting `unknown`).
- **Ranks.** `unknown` 0, `answered` 0, `completed_verified` and `completed_accepted` 1, `corrected` 2 (unchanged numbers, so no rescale of stored `outcome_rank`). Replacement rule: a strong outcome replaces `unknown` or `answered`; at merged rank 0, `merge_task` (`ledger/router.rs:272-320`) lets `unknown` win over `answered` in either order (the plugin's rule is monotone within a task: once a write is seen it reports `unknown`), and a non-empty merged `files` forces `unknown`, so `answered` never carries a file; the `outcome = 'unknown'` guards at `ledger/router.rs:561-577` and `ledger/review.rs:141-150` become `outcome IN ('unknown', 'answered')`.
- **TS** (`claude-plugin/hooks/router.ts`): `Outcome` (`:25`) gains `'answered'`; `outcomeOf` (`:1065-1071`) returns it by the rule above using `task.files.length === 0` (the signal the v45 backfill sees; `hasEdit` can be true with no file, `router.ts:967-969`), with a test for an edit with no file. `endTask` (`routing.ts:557-567`) propagates only strong outcomes: an `answered` parent sends no `task_outcome`, like `unknown` today.
- **Rust** (`crates/relais/src/router/outcome.rs`): `TaskOutcome::Answered` with `as_str`/`parse`, `rank()` 0, `is_completed` false, `is_strong` false; the `ALL` test array grows to 5. `router/explore.rs:33-35` (`strong`) becomes `.filter(|o| o.is_strong())`. `router/state.rs:248-251` (`COMPLETED`) is an allowlist and stays unchanged.
- **Report** (`router/report.rs`): `SessionRoutingReport` gains `answered`; the outcomes line keeps its shape as `outcomes: N completed, N corrected, N unknown (unknown rate R, answered excluded)` and a new line follows it, `  answered (no file edited, never a label): N` (each line at most 80 columns with 4-digit counts), where `unknown_rate = unknown / tasks` (answered is not unknown). `report --json` schema 14 → 15.
- **Backfill.** Ledger step v45 (expand-only, `ledger/schema/migrations.rs`, `LEDGER_SCHEMA_VERSION` 44 → 45 and the pinned tests at `schema/mod.rs:960,1026,1084`): `UPDATE router_tasks SET outcome = 'answered', outcome_source = 'backfill_v45' WHERE outcome = 'unknown' AND files_json = '[]' AND ended_at IS NOT NULL AND class_kind IN ('question', 'review') AND NOT EXISTS (SELECT 1 FROM json_each(router_tasks.inferred_json) WHERE value = 'aborted')`. Old rows carry no `maybeWrote` signal, so only the two non-editing kinds are judged: 277 rows on 2026-10-27 (review 186, question 91; 2 aborted questions excluded); the 95 no-file debug, edit and design rows stay `unknown`. Every such row has `outcome_source = 'own'` today. Undo: `UPDATE router_tasks SET outcome = 'unknown', outcome_source = 'own' WHERE outcome_source = 'backfill_v45'`. The report shows `answered` by `class_kind`.
- **Docs.** `docs/router-protocol.md:104-140` (outcome set, ranks, `task_outcome`), `docs/SPEC.md:1131,1137` (§30 outcomes and unknown rate).

## Phase 2: the parent task id (plugin + Rust)

- **TS.** `TaskState` (`router.ts:80-120`) gains `parentTaskId?: string`; `afterSpawn` (`routing.ts:401-445`) passes `r.task?.taskId` to `newTask`; `taskRecord` (`router.ts:1196-1219`) emits `parent_task_id` (null for a main task or an unparented spawn).
- **Rust.** `wire.rs` `Task` gains `parent_task_id: Option<String>` with `#[serde(default)]`, so a record without it still parses; ledger v45 adds `router_tasks.parent_task_id TEXT` (same step as the backfill); `RouterTaskRow` and its column lists (`ledger/router.rs:84-110, 339-340, 487-529, ~935-944`) carry it; `merge_task` keeps the stored `parent_task_id` (`stored.or(incoming)`) and unions `unmatched_checks`, capped at 5 distinct entries, each with a test.
- **Report.** Under session routing, a header `subagent unknown by parent (rows since v0.36.0):` and then one indented line per bucket (`parent strong N`, `parent answered N`, `parent unknown N`, `no parent N`), each under 80 columns; older rows are counted on a separate line `recorded before v0.36.0: N`, not as `no parent`. Decision it drives: if `parent unknown` is the largest bucket, the next plan targets the parents' verification; otherwise the line is removed in the next release.

## Phase 3: unmatched check commands, counted (plugin + Rust)

- **What is kept.** When a Bash command matches no entry of the checks list (`checkIndex`, `router.ts:751-759`) and its program is a build or test runner (`make`, `just`, `cargo`, `npm`, `pnpm`, `yarn`, `bun`, `go`, `pytest`, `uv`, `mvn`, `gradle`, `./gradlew`, `deno`, `tox`, `nox`), the task records the argv prefix a person would paste into `checks`, and nothing more: leading `VAR=` words and flags are skipped, then the program, then the next word only if it matches `^[a-z][a-z0-9:_-]*$`, then after `run` or `exec` one more word under the same rule (`make check`, `cargo nextest`, `npm run test`, `uv run pytest`; `pytest tests/x.py`, `make -C /p check` and `cargo +nightly test` record `pytest`, `make`, `cargo`). Each entry is prefixed with the session's repository (the basename of its git root). At most 5 distinct entries per task. No path or value leaves the session (ADR-0002 spirit: only what is needed to count). Tests cover those inputs and `FOO=x make`.
- **Wire and ledger.** `taskRecord` emits `unmatched_checks: string[]`; `wire.rs` `Task` gains it with `#[serde(default)]`; ledger v45 adds `router_tasks.unmatched_checks_json TEXT`.
- **Report.** `unmatched check commands (top 5, tasks per entry):`, then one indented line per entry, `<repo>: make check 12` (a count is tasks, not invocations); with none, `unmatched check commands: none recorded`. It is the list a person reads to add commands to that repository's `relais.toml` or to the built-in list. Nothing is added automatically.

## Delivery

- One branch, one PR in relais; phases 1–3 share ledger step v45 and the wire change, so they ship together as v0.36.0 (CHANGELOG `## v0.36.0` section names schema 45, report schema 15, the backup advice, and "reinstall the plugin": an old relais refuses the new wire fields because of `deny_unknown_fields`).
- Install order: the v0.36.0 binary first, then the plugin. A new plugin against an old binary gets exit 2, and `observe` drops the whole batch, decisions and usage included (routing.ts:675-677). The PR states why the ADR-0030 steps are collapsed: nothing persisted is in an old form, the Rust side only adds and every new field is optional, and the plugin is the only producer.
- The plugin checks `relais --version` ≥ 0.36 before it sends `answered` or the new fields, and sends the v0.35 form otherwise, so a wrong order never drops a batch.
- Phase 0 runs before the release, against v0.35.0, since it needs no code.
- Each phase's implementation is routed through relais as a bounded change task in the task worktree.
- Rollback: v0.35 refuses a v45 ledger (`ledger/schema/mod.rs:52`), so going back is a fix-forward v0.36.x, or the pre-upgrade backup at the cost of the rows written since; the backfill alone is undone with the SQL in Phase 1.

## Verification

- `make check` (lint, test, msrv, audit, plugin) → exit 0.
- New or changed tests:
  - `router/outcome.rs`: round trip and rank for 5 values; `Answered` is neither completed nor strong.
  - `claude-plugin/tests/router.test.ts`: `outcomeOf` cases: no edit and no check → `answered`; edit and no check → `unknown`; edit and passed check → `completed_verified`; corrected wins.
  - `claude-plugin/tests/routing.test.ts`: an `answered` parent sends no `task_outcome`; a subagent record carries `parent_task_id`; an unmatched `make check` is recorded as `make check` with no arguments.
  - `ledger/router.rs`: `answered` then `unknown` (no files) → `unknown`; `unknown` then `answered` → `unknown`; `answered` then `answered` with files → `unknown`; `answered → completed_verified` replaced; `corrected` never replaced.
  - `router/wire.rs`: a batch with `answered`, `parent_task_id` and `unmatched_checks` parses; a batch without the new fields still parses.
  - Ledger migration test: v45 backfills only ended `unknown` question/review rows with `files_json = '[]'` and no `aborted` in `inferred_json`; an aborted row, an open row, a no-file `debug` row and a row with files stay `unknown`.
  - `router/report.rs`: the outcomes line, the `answered` field and the unknown rate on a fixture.
  - `router/explore.rs`: `answered` rows are not strong for the stop rule.
- Live, after release and plugin reinstall (ledger backed up first): `relais report --since 2026-10-01` shows `answered` = the count from `SELECT COUNT(*) FROM router_tasks WHERE outcome_source='backfill_v45'` and `SELECT COUNT(*) FROM router_tasks WHERE outcome='answered' AND files_json!='[]'` = 0; no refused batch in the 24 h after the plugin reinstall; a new subagent task row has a `parent_task_id`.
- Phase 0: dry-run table of 26 tasks; each spent replay's outcome; cumulative spend read from the ledger cost of the replay runs after each one, stopping before a launch that would cross $20 at the $0.44 mean; `counterfactual gap` after the next learn cycle.
- At day 7 after release: the test-split floor projection and outcome-grade label counts against today's (about 2027-10-03; 25 yes / 87 no), and `answered` by `class_kind`. The `answered` rule is rolled back (SQL in Phase 1) if more than 5% of `answered` tasks are later corrected or reverted, or if any row with a file is `answered`.

## Not in scope

- Laya (classifier distillation or routing); the person chose labels and replays first.
- Treating "no complaint" as success (`router.ts:691` stays a diagnostic).
- Recipes for the 15 other repositories and the 23 relais tasks under an older profile hash.
- Asking the person to accept answers at the end of a question.

## Decision log

- D1 (2026-10-27, person) Labels and replays before Laya: "Issues + rejeu (Recommandé)".
- D2 (2026-10-27, person) Questions and no-edit tasks get a distinct `answered` outcome, never a label: "Issue « answered » (Recommandé)".
- D3 (2026-10-27) `answered` shares rank 0 with `unknown` so stored ranks need no rescale; the equal-rank `unknown → answered` step is the one special case in `merge_task`.
- D4 (2026-10-27) Unmatched commands are kept as program + first subcommand only, capped at 5 per task.

<!-- panel: repos=relais adds= reviewers=architect,backend,lang:rust,po,tui,unix,codex body-sha=cf8c58d9122a -->
