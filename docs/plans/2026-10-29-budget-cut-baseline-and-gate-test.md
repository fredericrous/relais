---
status: done
branch: fix/budget-cut-baseline-and-gate-test
repos: [relais]
adrs: [ADR-0010]
---

# A baseline cut by the run's budget ends on that budget, and the gate test observes service order

## Review panel

👉 **Decide:** none — approve if a baseline check or setup killed because the run's budget ran out ends `budget_exhausted`, and the gate test is made deterministic.
📍 relais · plan reviewed · next: branch fix/budget-cut-baseline-and-gate-test, then #333. Panel: backend (opus-5-5), codex (gpt).
**Changed by review:** budget cut only when the limit was really clipped; worktree released, budget passed; loops must fail on main first.
📄 Full reviews: [2026-10-29-budget-cut-baseline-and-gate-test.reviews.md](2026-10-29-budget-cut-baseline-and-gate-test.reviews.md)
**Verdicts:** backend approve-with-changes ×2 (1 high fixed); codex approve-with-changes; 4 findings carried into implementation.

## Context

PR #333 (answered outcome) is green everywhere but `test (windows-latest)`, and main's CI fails the same way: three of its last four runs, each on a timing test the change does not touch. The person chose (2026-10-29) to fix the flaky tests in a separate PR before merging #333.

- **`runner::tests::budgets::total_seconds_caps_the_run_end_to_end`** (#333, and `the_total_ends_a_queued_run_before_its_wait_budget_does` on main run 40767733343) end `blocked (baseline_unrunnable): sh@… timed out (30s wall limit) at the base revision` instead of `budget_exhausted (total)`. Cause, from the code:
  - the run clock starts before preflight (`runner/engine.rs:576-581`), and every command's timeout is clipped to what is left (`verify/mod.rs:1228`, `Budgets::clip`, `policy/budgets.rs:257`), as ADR-0010 decides;
  - with `total_seconds = 1` on a slow runner, preflight and the base worktree spend the second, the baseline check starts with a near-zero timeout and ends `TimedOut`;
  - the candidate path already tells a budget cut from a slow check (`runner/attempt.rs:~920-933`: a `TimedOut` check while `budgets.exhausted()` is `Some` ends `end_on_exhausted`), but the baseline does not: `runner/baseline.rs:180-202` turns any `TimedOut` into `BaselineUnrunnable`, and a base setup cut the same way ends `VerificationSetupFailed` (`:122-128`). The message then prints the declared 30 s, not the clipped limit.
  So this is a product gap the tests found, not only a flaky test: a run whose total runs out during its baseline is reported as a broken base.
- **`common::gate_tests::a_two_place_waiter_is_served_before_later_one_place_waiters`** (`crates/relais/tests/it/common/mod.rs:~185-215`, failed on #333's Windows re-run and on main runs 40808806364 and 40799429770) asserts `order[0] == "two"`, but each waiter pushes its name after `take` returns, and `take` hands the turn on before that push. The gate served `two` first; a one-place waiter can still push first. The test observes push order, not service order.

**Intended outcome:** a baseline check or base setup killed because the run's budget ran out ends the run `budget_exhausted (<budget>)`, as the candidate path does; the two budget tests pass whatever the machine's speed; the gate test is deterministic.

## 1. The baseline asks the budget before blaming the base (Rust)

- In `runner/baseline.rs`, where a setup step fails (`:~122`) and where a check's `TimedOut | Cancelled | Signalled | None` makes the base unrunnable (`:~180-202`): when the step ended `TimedOut` and `self.budgets.exhausted()` is `Some(exhausted)`, end through `end_on_exhausted(exhausted)` (`runner/engine.rs:890`), the same call the candidate path makes at `attempt.rs:~928`. Every other ending is unchanged: a base check that hits its own limit with budget left stays `baseline_unrunnable`.
- Factor the candidate's test into one helper both paths call, e.g. `Engine::budget_cut(check) -> Option<Exhausted>`, so the rule "a timeout under a spent budget says nothing about the code" lives once. It fires only when the check ended `TimedOut` and its clipped limit was below its declared one; `exhausted()` alone can return `AdmissionWait`/`DiskWait` for a check killed by its own limit (`policy/budgets.rs:271-280`).
- Each new early exit releases the base worktree first, as `baseline.rs:116-121` does, and passes `fresh_progress(preflight).budget` to `end_on_exhausted`, as `baseline.rs:104` does.
- Carry the clipped limit computed at `verify/mod.rs:1228` on the check outcome, for `budget_cut`. `unrunnable_baseline_detail` is unchanged: a clipped timeout now ends `budget_exhausted`, so a `baseline_unrunnable` timeout always ran its declared limit.
- Docs: `docs/SPEC.md:358` ("A baseline that times out stays `blocked (baseline_unrunnable)`") gains "unless a budget of the run ran out under it, which ends the run on that budget"; same clause in the ADR-0010 consequences is not needed (the decision already says commands are clipped).

## 2. The gate test observes service order (test only)

- `a_two_place_waiter_is_served_before_later_one_place_waiters`: after queueing `two`, `one-a`, `one-b` with 1 of 4 places free, `gate.give(1)`: 2 places are free, so only the head (`two`) can be served and it takes both, leaving none for the one-place waiters. Join the `two` thread, assert `order == ["two"]`, then `gate.give(2)` and join the rest. Deterministic: no waiter but `two` can take a place before the assertion.

## Verification

- `make check` → exit 0.
- New runner tests (`runner/tests/budgets.rs`):
  - a run whose total is spent before its baseline check runs ends `budget_exhausted (total)`, not `baseline_unrunnable`; forced deterministically (a fixture clock or a total already at zero at the baseline, not a sleep);
  - the same for a base setup step;
  - each new baseline test turns the baseline cache off, so the base commands actually run;
  - a baseline check that times out on its own limit with budget left still ends `baseline_unrunnable` (unchanged), and its detail names the declared limit;
  - the existing `total_seconds_caps_the_run_end_to_end` and `the_total_ends_a_queued_run_before_its_wait_budget_does` stay as they are and now pass on a slow machine: run each 20 times under CPU load locally (`for i in $(seq 20)`): on unfixed `origin/main` at least one fails (the race reproduces); on the branch 0 of 20 fail.
- Gate test: run it 200 times (`cargo test -p relais --test it gate_tests -- --test-threads=1` in a loop), 0 failures.
- CI: the PR's `test (windows-latest)` green; then #333 rebased on it and its checks re-run.

## Verification record (2026-10-29, before push)

- `make check` on the applied candidate (`d5368133`): Rust 2042 + 34 + 258 + 1 passed, 0 failed, including `a_baseline_check_cut_by_the_total_ends_on_the_total`, `a_base_setup_cut_by_the_total_ends_on_the_total`, `a_baseline_check_that_hits_its_own_limit_stays_unrunnable`, `after_a_spent_admission_wait_a_check_on_its_own_limit_is_not_a_budget_cut`, `a_check_outcome_without_an_applied_limit_still_parses`, `an_applied_limit_round_trips`, `total_seconds_caps_the_run_end_to_end`, `the_total_ends_a_queued_run_before_its_wait_budget_does` and `a_two_place_waiter_is_served_before_later_one_place_waiters`. The plugin suite (untouched by this branch) had 16 failures at load 224: 14 were 5000 ms per-test timeouts and 2 assertions; the commit's pre-commit gate (31 checks, plugin included) passed afterwards.
- Red proof: with `origin/main`'s `runner/baseline.rs` put back, `a_baseline_check_cut_by_the_total_ends_on_the_total` and `a_base_setup_cut_by_the_total_ends_on_the_total` both fail (budgets.rs:449, :516); restored, both pass.
- Stress loop, 20 runs each at load 60-96: `total_seconds_caps_the_run_end_to_end` and `the_total_ends_a_queued_run_before_its_wait_budget_does` failed 0/20 on `origin/main` and 0/20 on the branch. The plan's "at least one fails on `origin/main`" was not met (D3).
- Gate test: 200 runs, 0 failed.

## Not in scope

- The other timing-sensitive tests not seen failing.
- Changing ADR-0010's clipping or the run clock's start.
- PR #333 itself.

## Decision log

- D1 (2026-10-29, person) Fix the flaky tests in a separate PR before #333: "Corriger les tests instables d'abord".
- D2 (2026-10-29, person) Both, with a small plan: "Les deux, avec un petit plan (Recommandé)".
- D3 (2026-10-29) The local stress loop did not reproduce the Windows failure on `origin/main` (0/20 each at load 60-96): the race needs a process spawn slower than the 1 s total, which this Mac never reached. The deterministic regressions stand in for it: both fail on `origin/main`'s baseline and pass with the fix. CI on `windows-latest` is the remaining check.
- D4 (2026-10-29) Carried findings applied in the code: the rule lives in `budget_cut` (applied limit known and below the declared one), the applied limit is an optional serde-default field with millisecond precision, and the regressions use a base command that runs until killed.
- D5 (2026-10-29, implementation review) Deliberate departure from "not a sleep": the two end-to-end regressions run a real `sleep 30` base command under a 1 s total, because the bug is a real child killed at a real clipped deadline and a fixture clock kills no process; each costs about 1 s and passes on any machine speed (a slow one still ends `budget_exhausted (total)`). The helper's rule is also covered without real time by `after_a_spent_admission_wait_a_check_on_its_own_limit_is_not_a_budget_cut`.
- D6 (2026-10-29) The refreshed decisions pack and its `HEADS.md` row (ADR-0047) ride this branch's first commit, as `guidance.generated-block-rides-the-change` asks of every task branch.
- D7 (2026-10-29) On the candidate path, a check that ran its full declared limit while a wait budget ran out is now judged on its own (a failed check, repaired), not `budget_exhausted`: that is backend r1 finding 1 applied to both paths, so the run names the budget only when the budget shortened the command.

## Implementation review

- Verdict: approve on tree f0d32385 (Delta), after approve-with-changes on c3773bdb (round 1).
- Fixed: the own-limit test asserts the declared limit; the three baseline tests pin the cache off.
- Deliberate: real `sleep 30` base commands in the two end-to-end regressions (D5); the pack refresh rides the branch (D6); the candidate narrowing (D7).
- Round 1 53k tokens, 70 s; Delta 58k tokens, 16 s. Remaining check: `test (windows-latest)` on this PR, then on #333 rebased on it.

<!-- panel: repos=relais adds= reviewers=backend,codex body-sha=f8799191435f -->
