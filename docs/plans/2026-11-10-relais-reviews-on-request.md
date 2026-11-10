---
status: active
branch: feat/review-opt-in-lazy-baseline
repos: [relais]
adrs: [relais:ADR-0013, relais:ADR-0010, relais:ADR-0011, decisions:ADR-0045]
---
# relais reviews on request and checks the base lazily

## Review panel

📄 Full reviews: [2026-11-10-relais-reviews-on-request.reviews.md](2026-11-10-relais-reviews-on-request.reviews.md)
👉 **Decide:** none. Approve if a downgraded relais refusing `report_exists` receipts is acceptable, and the cache going read-only is too.
📍 relais · planned, nothing built · next: Phase 1 on `feat/review-opt-in-lazy-baseline`. Panel: backend, lang:rust, tui, unix (all opus-5-5); codex not installed.
**Changed by review:** base logs keep their profile index, setup runs once and the memo is single-flight; the on-demand base gets its own step and is kept off the work budget; edited-but-unreviewed tests print a plain line.
**Verdicts:** 4 approve-with-changes in rounds 1 and 2; 9 findings carried into implementation (listed below).

## Picture

```
 today                               after this plan
 -----                               ---------------
 run --> [base: all checks]          run --> worker --> candidate checks
     --> worker --> candidate checks              |
     --> reviewer (tests edited)          green --+--> accepted
     --> accepted                           red --+--> [base: red checks]
                                                       --> new or old?
```

A run used to check the whole base before its worker and review every
candidate that edited tests. After this plan, it starts the worker at once,
checks only the red checks on the base when a candidate fails, and is
reviewed only under `review: required`.

## Context

relais ADR-0013, accepted 2026-11-10 (relais#429), settles three decisions in
the checks-once programme. The first, profiles split per gate, is being built
outside this plan as one PR per repository: amont, amont-agent, adr-lint, grh,
aval and fleet-lsp. This plan builds the other two, in relais's run engine:

- **`acceptance.semantic-review`:** relais's reviewer runs only under
  `review: required`.
  - Today a candidate that edits test or verification inputs is always
    reviewed (`runner/attempt.rs:900`, `runner/scheduler.rs:1060`), and an
    inspection's report review always runs (`runner/verification.rs:519`).
  - Review was 38% of relais's spend (checks-once t02), and
    implementation-review reads the same diff again.
- **`acceptance.baseline`:** proof, else lazy.
  - Today `RunEngine::baseline` (`runner/baseline.rs:14`) runs setup and the
    whole profile on the base before any worker, unless it hits the cache or
    the base is proven.
  - Over 11-03 to 11-10 that was 0.74 uncached baselines per verified run
    (decisions one-verdict-per-tree t11).

## Goal

A run without `review: required` is accepted on its checks and criteria,
and pays no reviewer. A run whose base is neither proven nor cached starts
its worker at once, and checks the base only when a candidate goes red, and
only on the red checks.

## Non-goals

- Deleting `runner/review/*`. It stays as the opt-in path; deleting it is a
  later choice, once the opt-in rate is known (t05).
- The profile split per gate, which ships in each repository's own PR.
- Reclaiming the total budget's reserved baseline seconds
  (`BudgetLimits::resolve`, `policy/budgets.rs:78`). The total still
  reserves them.
- Removing the legacy `needs_review` / `Review*` values, the `review_parts`
  table or `verification_snapshots`. They stay readable.
- The learner. `LABEL_POLICY_VERSION` stays 4.

## Code review

- relais: backend,lang:rust,unix,tui

## Behaviour

**Review (Phase 1)**
- `review_required` becomes `decision.review >= Review::Required` in both
  places. Editing test or verification inputs no longer forces review.
  - Those inputs are still recorded as the receipt's
    `verification_inputs_changed`.
  - Under `review: required`, the reviewer's prompt still flags them.
  - Without it, `relais run` and `relais report` print one plain line,
    "tests edited; not reviewed (review: optional)", and a test asserts it.
  - The policy-class stop (`touched_inputs.policy` →
    `needs_decision (verification_inputs_changed)`) is unchanged.
  - The `Verifying` transition that explained a forced review
    (`attempt.rs:932-940`) goes.
- An inspection under `review: required` keeps its report review exactly as
  today.
- An inspection without it dispatches no report review. Its bare,
  `llm_review` and unnamed-`test` criteria are met by the report existing.
  - They are recorded with a new `settled_via = "report_exists"`
    (`verify/mod.rs:814`, `SettledVia::ReportExists`), and counted as not
    independent.
  - An empty report still fails, `empty_report`, with one repair: the guard
    is upstream (`attempt.rs:926`).
  - A `check` or named-`test` criterion is still settled by the checks.
  - Human sign-offs are unchanged.
- The routed level decides, which is `preflight.decision.review`, including
  `[[risk]] review = "required"` floors and risk hints. A floor still forces
  review.

**Baseline (Phase 2)**
- `baseline()` keeps only its cheap half: `logs_dir`, the cache key and
  lookup, and the amont proof.
  - A cache hit and a proven base behave as today.
  - Otherwise it returns a **deferred** baseline (`Baseline.deferred = true`,
    no failures, no checks), and emits the decision `baseline_deferred`.
  - The run's steps print `baseline · deferred`: the step name stays
    `baseline` and `deferred` is its detail, so the pane's step contract
    (`release_scenarios.rs:1493`) holds.
  - An on-demand base run prints its own `baseline` step before its commands
    (`baseline · on demand · N checks`), and its result after them through
    `baseline_summary`.
- **When a candidate has failed checks and the baseline is deferred,** relais
  runs the base on demand.
  - It uses the same throwaway `verify-base` worktree and the profile's full
    setup, then only the commands whose `check_label` is among the red labels
    (`verify/mod.rs:1691`), in a profile clone with the other commands
    filtered out.
  - Logs use the prefix `base`, and timings the phase `base`.
  - Results are memoised on the `RunEngine` behind a single-flight guard, so
    two red candidates never run the base twice or share `verify-base` at
    once; a later red attempt runs only labels not run yet.
  - Each selected command keeps its original profile index in its log stem
    (`base-cmd{index}`, `verify/mod.rs:1744`), so a later attempt never
    overwrites an earlier one's log and `base-cmdN` still names profile
    command N. The base setup runs once per run; a later attempt reuses its
    tree and its `base-setup*` logs. A red label with no command on the base
    profile counts as a new failure.
  - A partial base run is never written to the baseline cache.
- **A candidate whose tree is identical to the base** (an inspection always,
  or a no-op change) needs no base run. Its own red checks are the base's
  failures.
- **The memoised base result feeds the existing consumers, through one merged
  view (`ctx.baseline` plus the `RunEngine` memo) that replaces their
  `ctx.baseline.failures`/`.checks` reads:**
  - `new_failure_labels` (inspection, `attempt.rs:1106`);
  - `all_preexisting` (`attempt.rs:1166`), which ends
    `needs_decision (baseline_failure_not_waived)` once repairs are spent, as
    today;
  - `guarded_preexisting` (`attempt.rs:1179`), the JUnit subset rule, which
    still saves the repair on attempt 1;
  - the receipt's `baseline_failures` (`verification.rs:1046`,
    `evidence.rs:264`).
- **The on-demand base run keeps every base-side ending, now after a worker
  was paid, with the candidate kept for `resume`:**
  - exhaustion → `blocked (baseline_resource_exhausted)`;
  - no verdict → `blocked (baseline_unrunnable)`;
  - a failed base setup → `blocked (verification_setup_failed)`;
  - a lease stop, cancellation, and total-budget exhaustion.
  - Each message says the candidate is kept and gives the exact `relais resume <run>` command; a test asserts that text.
  - Its commands draw on the **total budget only** (ADR-0010): a
    `HeavyAdmission`/`Budgets` mode clips them by what is left of the total,
    not of work, and their elapsed time is excluded from work (a new
    `Budgets::charge_base` adds it to `waited_in_work`), so a base run never ends
    a run `Work`.
- **The receipt gains `baseline_deferred: bool`,** with `serde(default)` so
  old receipts read. It is true when no base run happened up front.
  `baseline_failures` holds what the on-demand run found.
- **Decomposed runs:** the root receipt reports the deferred state
  (`scheduler.rs:1226`). The scheduler never attributed failures to the base,
  so nothing else changes there.

## Phases

- [ ] Phase 1 — the reviewer only under `review: required`:
  `crates/relais/src/runner/attempt.rs`, `crates/relais/src/runner/scheduler.rs`,
  `crates/relais/src/runner/verification.rs`, `crates/relais/src/verify/mod.rs`,
  `crates/relais/src/runner/approval.rs`, `crates/relais/src/cli/evidence.rs`,
  `crates/relais/src/runner/tests/mod.rs`, `crates/relais/src/runner/tests/review_stop.rs`,
  `docs/SPEC.md` (§4, §9, §10 lines ~104, 323, 399-405, 425, 435, 445),
  `CHANGELOG.md`; one commit.
  - Tests that pin the old behaviour change:
    - `changing_verification_inputs_forces_review` becomes "review only
      under required".
    - `a_review_off_inspection_still_runs_its_report_review_and_says_so`
      becomes "an inspection without required settles by its report".
    - The report-review inspection tests set `review: required`.
  - New tests: a pre-change receipt (no `baseline_deferred`, no `report_exists`) still renders in `report` and `decide`; an edited-test candidate under `optional` is accepted with no
    review dispatch; one under `required` is reviewed; an unrequired
    inspection's criteria are `report_exists`; an unrequired empty report
    still fails.
- [ ] Phase 2 — proof, else lazy: `crates/relais/src/runner/baseline.rs`,
  `crates/relais/src/runner/types.rs`, `crates/relais/src/runner/engine.rs`,
  `crates/relais/src/runner/attempt.rs`, `crates/relais/src/runner/verification.rs`,
  `crates/relais/src/runner/evidence.rs`, `crates/relais/src/runner/scheduler.rs`,
  `crates/relais/src/verify/mod.rs`, `crates/relais/src/policy/budgets.rs`,
  `crates/relais/src/runner/tests/mod.rs`, `crates/relais/src/runner/tests/exhaustion.rs`,
  `crates/relais/src/runner/tests/budgets.rs`, `crates/relais/src/runner/tests/timeout_rerun.rs`,
  `crates/relais/tests/it/release_scenarios.rs`, `docs/SPEC.md` (§9, §10 preflight
  paragraph line ~419 and the base-budget sentence ~429, §18 cache), `CHANGELOG.md`;
  one commit.
  - Tests that pinned "before any worker" become "at the first red
    candidate":
    - `a_baseline_whose_commands_cannot_run_blocks_before_any_worker`,
      `…wall_clock_kills…`, `a_failing_setup_at_the_base_blocks…`;
    - `release_scenarios.rs:1493` step order and `:2043`.
  - New tests:
    - a green candidate on an unproven base runs no base command;
    - a red candidate runs only its red labels on the base, and a pre-existing
      one ends `baseline_failure_not_waived`;
    - a second red attempt reuses the memo;
    - an identical-tree inspection on a red base is accepted without a base
      run;
    - the on-demand base draws on the total only: a base run longer than the
      work left does not end the run `Work`;
    - a JUnit pre-existing failure on a deferred base ends
      `PreexistingTestFailures` on attempt 1 (the memo keeps full `CheckOutcome`s);
    - the rewritten base-ending tests assert `finish_attempt` `Blocked` and the
      kept candidate's sha.
- [ ] Phase 3 — release: `CHANGELOG.md`, `Cargo.toml`, `Cargo.lock`, via the `tag-release`
  skill (v0.53.0). Installing it and reinstalling the plugin are the
  person's.

## Decision log

- 2026-11-10 — One branch and one PR, with one commit per phase, because the
  person prefers few PRs (`feedback_few_prs_reviewable_commits`).
- 2026-11-10 — `fix/budget-cut-baseline-and-gate-test` does not block Phase
  2: its PR relais#340 was closed unmerged on 10-29, and no session has
  touched it since.
- 2026-11-10 — The older ADRs keep their text. ADR-0013 records how it
  narrows ADR-0004, 0006, 0010 and 0011, and SPEC carries the new behaviour.
  decisions ADR-0045 is untouched: holdouts still settle as `llm_review` only
  under `review: required`, and that change is in SPEC.
- 2026-11-10 — No other repository reads relais receipts (`git grep settled_via
  baseline_failures` in amont, amont-agent, grh, skills, sre-agent, the site:
  only decisions docs), so no `adds=contract`. Rollback caveat: a relais older
  than this release refuses a receipt carrying `settled_via = report_exists`
  (unknown variant), so a downgrade cannot re-seal such receipts.
- 2026-11-10 — The baseline cache becomes read-only legacy, because it stores
  only an all-green full base and a lazy base run checks red commands only, so
  it can never write one. A hit still serves; removing the cache code is a
  later cleanup.
- 2026-11-10 — The baseline is lazy with no policy switch back to up-front.
  ADR-0013 decided it for every repository, and a switch would change the
  policy hash of every `relais.toml`.

## Verification

- **Phase 1:**
  - A scripted-backend change run that edits `tests/x.rs` under `review:
    optional` → no review dispatch, accepted, with
    `verification_inputs_changed` on the receipt.
  - The same run under `review: required` → reviewer dispatched.
  - An inspection without required → no `report-review-*.txt`, criteria
    `settled_via = report_exists`, `independence` not independent.
  - `cargo test -p relais` green.
  - A real `relais run` inspect on this repository: its receipt shows
    `report_exists` and its cost shows no `report_review` phase.
- **Phase 2:**
  - A scripted run on a green but uncached base → no `base-cmd*` log; a
    `baseline_deferred` event; the receipt has `baseline_deferred: true`.
  - A red candidate whose failure is on the base → only the red label's
    `base-cmd*` log, ending `needs_decision (baseline_failure_not_waived)`.
  - A scripted green run on an uncached base → `check_timings` has no `base` phase.
  - `cargo test -p relais` green.
  - A real `relais run` change on grh after its profile split → no base run in `check_timings`.
  - A `blocked (baseline_*)` after a paid worker increments a counter shown in cost output.
  - Estimate in the plan: red rate x base subset seconds vs 0.74 full baselines per run.
- **Pins:**
  - `cargo test -p relais --lib install::`, which pins the skill text.
  - The policy-hash test in `policy::tests`, if `relais/relais.toml` changes.
    It shouldn't.

## Outcome

<!-- panel: repos=relais reviewers=backend,lang:rust,tui,unix body-sha=8e960c48cbd2 -->
