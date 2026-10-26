---
canonical: decisions:docs/plans/2026-10-25-holdout-acceptance-in-relais.md
phases: [2, 3, 4, 5]
status: active
---
# Holdout acceptance in relais (pointer)

The plan lives in the decisions repository, which owns ADR-0045
(`work.holdout-acceptance`). This repository carries phases 2–5.

## Decision log

- 2026-10-25 — Phase 2 is parse and preflight only. Successor inheritance
  of the frozen definition (resume, repair, `--widen-scope`, `review-parts`,
  `--rerun-verify`) moves to phase 3, because only phase 3 runs a holdout
  and the successor paths are part of verification. Phase 2 records the
  definition digest in the manifest so that phase 3 can bind to it.
- 2026-10-25 — Phase 3 also owns two things the canonical plan lists in
  phase 2's text: the holdout command's dependency files joining the
  verification inputs, and protecting the overlay path in the scope
  check. Phase 2 neither fetches nor runs a holdout, so neither has
  anything to guard yet.
- 2026-10-25 — `docs/SPEC.md` §4 describes parse and preflight only, and
  says so: "This relais does not fetch or run a holdout".

## Implementation review

- Phase 2: approve-with-changes (82k tokens, 69 s). relais verified the candidate first (run-65eb09e231ab9-11e50, accepted, its reviewer passed).
  - Fixed: the Verification row now holds what was observed. A refusal exits 3 (`CliOutcome::Blocked`, `cli/outcome.rs:115`), not the 2 the plan expected; the manifest leaves the overlay out by design. Phase 2 is ticked in the canonical plan.
  - deliberate: when the repository name is missing, preflight refuses as `holdout_unknown (declared: none)`. That fails closed; phase 3 gives it a clearer detail.
  - Candidate `functions.no-flag-arguments` at `runner/tests/holdouts.rs:39`: fine, because the bool is data passed into `DeclaredCriterion.mandatory`.
- Delta after the Windows CI fix (`store` written as a TOML literal string): approve (88k tokens, 35 s). No new finding. The missing-repository-name detail is still deliberate and goes to phase 3.
- 2026-10-26 — Phase 3 is split into three relais tasks: 3a runs and
  settles holdouts (`docs/design/holdout-phase-3a.md`); 3b covers the
  overlay collision, the protected verification inputs and the repair and
  escalation feedback rule; 3c covers successor inheritance and the
  missing-repository-name detail. The Forgejo `holdouts` repository is
  created at the phase 5 pilot, its first use; phase 3 tests use local
  stores.

## Verification

Phase 3a, observed 2026-10-26 on `feat/holdout-run`
(`cargo test -p relais --lib holdout`: every test listed below ok; the pre-commit gate ran the full suite green).
Each test drives a full `RunEngine` run against a local bare-repo store.
- Pass: the script writes a passing JUnit report → expected `accepted`, `holdouts[0].outcome == passed`, test `h::escapes` → observed as expected, and the run's holdout dir is gone afterwards (`a_passing_holdout_settles_its_criterion_and_is_listed_on_the_receipt`).
- Fail: a failing test → expected criterion unmet, a gap naming only the criterion, `escaping` and `h::escapes` → observed as expected; the gap text holds no script line, store, path or overlay (`a_failing_holdout_test_leaves_the_criterion_unmet_without_leaking_the_holdout`).
- Skipped with exit 0 → expected not met → observed not met (`a_skipped_holdout_test_is_not_a_pass_even_on_exit_zero`).
- No report → expected `did_not_build` → observed (`a_holdout_that_writes_no_report_did_not_build`).
- Store deleted after preflight → expected `blocked (holdout_unfetchable)`, candidate ref kept → observed (`a_store_deleted_after_preflight_blocks_the_run_and_keeps_the_candidate`).
- `{config}` → expected the store's absolute config path in argv → observed (`the_config_placeholder_reaches_the_holdout_command_as_the_stores_config_path`).
- No holdout log among the profile's `CheckLog` evidence rows → observed (checked inside the pass test).
- Fetch once, reused across a two-attempt run; the dir is removed at the end → observed (`the_store_is_fetched_once_and_reused_across_attempts`).
- Sweep: the dir of an ended run is removed at the next run's start and by the `--retire` sweep, while the dirs of a live run and of a run unknown to the ledger are kept → observed (`ended_run_holdout_dirs_are_swept_at_run_start_and_live_or_unknown_ones_kept`). A no-op mutation of `sweep_ended` turns it red.
- Not shown by 3a, left to 3b: the repair and escalation inputs holding nothing of the holdout, and the overlay collision.

## Implementation review (3a)

- Round 1: approve-with-changes (109k tokens, 112 s).
  - The blocking finding was the missing Verification record, written above.
  - Fixed the low finding: an empty failing list now reads "did not exit 0 (a non-zero exit, a timeout, a signal or a cancel)".
  - Candidates: `f` in `Display::fmt` is fine, and so is `_ =>` over a tuple.
- Delta: approve (113k tokens, 16 s). Carried to 3b: keep the kept `log.txt`/`report.xml` out of reach of `read_hints` and successor context; check the fetch-by-ref fallback against the real Forgejo store at the phase 5 pilot.
- Windows CI fixes after the delta. `{config}` is now rendered per platform through a `PathStyle` sum type: Unix keeps the path as is, Windows strips canonicalize's `\\?\` prefix and uses `/`.
  - The first fix took a `windows: bool`, which I had asked for in the contract. The review flagged it under `functions.no-flag-arguments`, and it was replaced.
  - Fresh round 1 on the final tree: approve (33k tokens, 47 s).

## Phase 3b

- 2026-10-26 — 3a merged (relais#311, 30a54188). 3b is built to `docs/design/holdout-phase-3b.md`. It covers:
  - the reserved overlay (`failed (holdout_overlay_collision)`);
  - the holdout's declared `inputs` as protected verification inputs;
  - the proof that the repair, escalation and reviewer prompts and the exhaustion detail hold nothing of a holdout;
  - `needs_decision (holdout_failed)` at the attempt ceiling;
  - the missing-repository-name detail, carried from phase 2.
- 2026-10-26 — Deferred from 3b with a reason. The widen-scope overlay filter and the `read_hints`/successor-context assertion are the design's second guards. The overlay collision ends the run before any scope can be widened, and a hint under the state dir is already refused. They come back if phase 5 counts a `holdout_exposures` that bypassed the collision.

### Verification (3b)

Observed 2026-10-26 on `feat/holdout-guards`. relais run run-65ebabd29ba10-11f16 was accepted with all 5 profile checks at exit 0; the pre-commit gate ran the full `cargo test` green. Each row names its test. Rows 1–4, 6 and 7 drive a full `RunEngine` run against a local store; the `decide`, log-hint and repository-name rows are unit tests.
- A worker writes the overlay path → expected `failed (holdout_overlay_collision)`, no repair, and no artifact besides the patch and events naming the overlay → observed (`a_worker_that_writes_the_overlay_path_ends_the_run_failed_with_no_repair`).
- A worker changes a declared holdout input → expected `needs_decision` → observed (`a_worker_that_changes_a_declared_holdout_input_stops_for_a_person`).
- A failing holdout with repair and escalation → expected: the repair and escalation prompts carry the statement and `h::escapes`, and none of the store, path, overlay, config, script lines or kept paths → observed (`every_attempt_failing_on_the_holdout_ends_for_a_person_with_what_a_worker_never_sees`). The reviewer sees `name: outcome (ids)` only → observed (`the_reviewer_is_given_each_holdout_as_name_outcome_and_test_ids_only`).
- Out of attempts on a holdout alone, with `max_attempts = 2` → expected `needs_decision (holdout_failed)`, not `budget_exhausted`; the raising transition's gap reads `holdout `escaping` failed: h::escapes` → observed (`running_out_of_attempts_on_a_holdout_alone_ends_holdout_failed_not_budget_exhausted`). A mutation of the gap wording turns it red.
- `relais decide --answer approve` on that stop → expected a refusal naming the holdout gap → observed (`cli::decide::tests::approving_a_holdout_failed_stop_is_refused_naming_the_holdout_gap`).
- A holdout beside a failing check → expected `failed`; beside another gap → expected a gap stop naming both → observed (`a_holdout_failure_beside_a_failing_check_still_ends_failed`, `a_failed_holdout_beside_another_gap_is_named_in_the_gap_stop`).
- A holdout log saying "No space left on device" → expected no exhaustion stop → observed (`a_holdouts_output_is_never_quoted_by_the_exhaustion_detail`).
- The repair log hint: a holdout line has none, while a check whose label mentions "(criterion:" keeps it → observed (`a_holdout_failure_line_omits_the_log_hint_and_a_check_line_keeps_it`, `a_check_label_that_mentions_a_criterion_still_gets_the_log_hint`).
- A missing repository name → expected `holdout_unknown` with its own detail → observed (`a_missing_repository_name_is_refused_by_its_own_detail`).

## Implementation review (3b)

- Round 1: approve-with-changes (81k tokens, 88 s). Both findings fixed:
  - `holds-until:` markers now sit at `widened_contract` and `widen_for_tests` for the deferred overlay filter;
  - the Verification wording now says which rows drive a full run.
- Candidates: both `mandatory`/`unchanged` bools are data, not selectors, so fine.
- Delta: approve-with-changes (85k tokens, 21 s). The holds-until markers are resolved. The row numbering was wrong and is corrected; that change is plan-only, outside the reviewed tree. Two notes are carried to phase 3c: a test that a `kind: verify` reaches the overlay check, and the ledger rows on the scheduler's collision path.
- Windows CI fix after the rebase review: two holdout tests now match kept-artifact paths with `Path::ends_with`, and the `detail` check now looks for the exact recorded log path. Review on tree f5db2e2c: approve (27k tokens, 59 s). The local commit gate hit the `budgets::total_seconds_caps_the_run_end_to_end` timing test under a load average of about 214; the test passed alone and on a retry.

## Phase 3c

- 2026-10-26 — 3b merged (relais#314, b527f74f). 3c is built to `docs/design/holdout-phase-3c.md`: a successor inherits its predecessor's frozen holdout definition from the ledger (schema 43), plus the two checks carried from the 3b review.

### Verification (3c)

Observed 2026-10-26 on `feat/holdout-frozen-definition`; the pre-commit gate passed the full `cargo test` on both commits. Rows 1, 2, 4 and 5 drive a full `RunEngine` run against a local store; row 3 is a ledger test.
1. A predecessor resolves `escaping` at sha A. The test then edits machine.toml's declaration and moves the store to sha B. Expected for the `--widen-scope` successor: judged at sha A with the frozen digest; the new name `fresh` is resolved at sha B; the events carry `holdout_definition_changed` with the frozen digest and the current one (computed from the edited declaration and sha A). Observed (`a_successor_is_judged_by_its_predecessors_frozen_holdout_and_resolves_a_new_name_fresh`).
2. The store is deleted before the successor starts. Expected: preflight does not ask the store, and the run ends `blocked (holdout_unfetchable)` at fetch, not `holdout_unreadable`. Observed (`a_successor_does_not_ask_the_store_again_for_a_frozen_holdout`). I wrote this test by hand after the worker described it without writing it; a mutation that re-resolves a frozen name through `ls-remote` turns it red.
3. A schema-42 ledger upgrades to 43 with an empty `holdout_definitions` table → observed (`a_v42_ledger_takes_the_holdout_definitions_table_empty`).
4. A `kind: verify` whose candidate wrote the overlay → expected `failed (holdout_overlay_collision)` → observed (`a_verify_whose_candidate_wrote_the_overlay_ends_failed_with_the_collision_code`).
5. A decomposed run's assembled candidate writes the overlay → expected the collision with the "assembled candidate" detail, and no attempt row left open → observed (`a_decomposed_run_whose_assembled_candidate_wrote_the_overlay_leaves_no_attempt_open`). The scheduler checks for the collision before it opens an integration attempt, so no scheduler change was needed.

### Implementation review (3c)

- Round 1 was approve-with-changes (63k tokens, 92 s); the scheduler claim was judged to hold.
- Fixed the blocking finding (`errors.explicit-absence`): the `undeclared` sentinel is now an `Option`. Also fixed: one transaction for the frozen rows, and a test for the undeclared case.
- Delta: approve (68k tokens, 27 s).
- Carried to phase 4: test that the `--rerun-verify`, `--successor-contract` and `review-parts` intents reach `intent_of_run`, and a chain of three runs.

## Phase 4

- 2026-10-26 — 3c merged (relais#317, 8f306673), and phase 3 is recorded in decisions#100. Phase 4 is built to `docs/design/holdout-phase-4.md`:
  - `holdout_outcomes` and `holdout_exposures` tables (schema 44);
  - exposures counted from the workers' own transcripts at run end;
  - `relais report` counts;
  - the three cases carried from phase 3.

### Verification (4)

Observed 2026-10-26 on `feat/holdout-report-counts`; `cargo test -p relais --lib holdout` passed, and the pre-commit gate passed the full `cargo test`.
- Scan: each marker (store, state_dir, machine_toml, overlay, config) is found in a `tool_use` input and only its name is kept; a transcript naming none gives no row; a double hit is one row (`each_marker_is_found_in_a_tool_use_input_and_only_its_name_is_kept`, `a_transcript_naming_none_gives_no_row`, `a_marker_hit_twice_in_one_tool_is_one_row`).
- Catches: a failing holdout on green checks counts 1, a passing one 0, and a failing holdout beside a red check counts 0 (`a_failing_holdout_on_green_checks_is_one_catch_and_a_passing_one_is_none`, `a_holdout_failing_beside_a_red_check_is_not_a_catch`).
- Positive control: a worker transcript that reads machine.toml → `holdout_exposures` = 1 with marker `machine_toml` (`a_worker_transcript_that_reads_the_settings_file_is_one_exposure_by_marker`). A mutation that makes the scan find nothing turns it red. An unreadable transcript is no exposure and emits `holdout_exposure_unread` (`an_unreadable_worker_transcript_is_no_exposure_and_says_so`).
- Report: `--json` carries `holdouts.{holdout_runs, holdout_catches, holdout_exposures}`, schema 14 (`the_report_json_carries_the_holdout_counts_under_holdouts`).
- Carried from phase 3:
  - a verify of a candidate with green checks and a failing holdout is not accepted, launches no worker and counts one catch (`a_verify_of_a_candidate_with_green_checks_and_a_failing_holdout_is_not_accepted`);
  - `--rerun-verify`, `--successor-contract` and `review-parts` successors reach their intent and are judged by the frozen definition (`a_successor_of_each_kind_reaches_its_intent_and_is_judged_by_the_frozen_definition`);
  - a successor of a successor is judged by the first run's definition (`a_successor_of_a_successor_is_judged_by_the_first_runs_frozen_definition`).
- Migration: a schema-43 ledger takes both tables, empty (`a_v43_ledger_takes_the_holdout_count_tables_empty`).

### Implementation review (4)

- Round 1: approve-with-changes (98k tokens, 289 s). This was the only review of this candidate. All six findings are fixed: 85f8ecb7 (relais run-65ec58b0ce88a-18238) fixed five and changed the catch query, and the delta found the did-not-build test missing, so I wrote it by hand:
  - placeholder names renamed;
  - only `failed` is a catch, so `did_not_build` is not;
  - the bare `machine.toml` marker is dropped, so ordinary work in relais or amont no longer counts as an exposure;
  - `label_kind` is in the `holdout_outcomes` key;
  - package workers' transcripts are counted on the root run;
  - `relais report --json` is tested through the binary.
- Verification added: `holdout_report::the_report_json_counts_one_catch_and_one_exposure_under_holdouts` runs the binary on a fixture ledger and asserts the printed `holdouts` counts. ok.
- More verification:
  - A holdout that did not build on green checks gives `holdout_runs` = 1 and `holdout_catches` = 0 (`a_holdout_that_did_not_build_on_green_checks_is_not_a_catch`). A mutation back to `outcome != 'passed'` turns it red.
  - The bare text `machine.toml` gives no exposure, while the absolute path gives one (`the_bare_file_name_is_no_exposure_and_the_absolute_path_is`).
  - A package worker's transcript that names a marker counts on the root run (`a_package_workers_transcript_counts_as_an_exposure_on_the_root_run`).
- Delta 1: approve-with-changes (113k tokens, 34 s). Fixed: the did-not-build test above; the one-level package query now carries a `holds-until:` marker (package contracts set `decomposition: None`); this record is completed. Schema v44 was edited in place, which is safe: 43 and 44 are unreleased (the latest relais release is v0.33.0, schema 42).
