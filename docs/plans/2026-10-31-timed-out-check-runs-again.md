---
status: done
branch: fix/timed-out-check-runs-again
repos: [relais]
adrs: [ADR-0010]
---

# A check that times out on its own limit runs once more before anyone is paid to fix it

## Review panel

👉 **Decide:** none — approve if one automatic re-run of a check that hit its own limit, with no worker, is how a contended machine stops buying repairs.
📍 relais · plan reviewed · next: branch fix/timed-out-check-runs-again, then resume Phase 0. Panel: backend (opus-5-5), codex (gpt).
**Changed by review:** applied limit and clip recorded; flaky reason field; holdouts in, setup out; holdout timeouts reach the budget guard.
📄 Full reviews: [2026-10-31-timed-out-check-runs-again.reviews.md](2026-10-31-timed-out-check-runs-again.reviews.md)
**Verdicts:** backend approve-with-changes ×2 then approve; codex approve-with-changes (1 high applied); 4 findings carried into implementation.

## Context

Phase 0 of `2026-10-27-answered-outcome.md` replayed 16 tasks on 2026-10-30/31, on a Mac shared by about ten sessions (load averages 40–200 on 12 cores). Six replays gave no verdict, about $2.5 spent for nothing, and most of them the same way: a candidate check ended `TimedOut` on its own declared limit (`make lint` 300 s, `make test` 1020 s), the run recorded `repair · behavioral_failure`, paid a repair worker, and the re-verification was then cut by the wall clock (`run-65f1e63f347e0-4802`, `run-65f189f19f7da-17b36`). The person chose (2026-10-31) to pause the replays and fix this first.

From the code (`origin/main`):

- `judge_candidate` (`runner/attempt.rs:~915-1101`): a `TimedOut` check is special only when a run budget is spent (`:921-934`, ends on the budget). Otherwise `CheckOutcome::failed()` (`verify/mod.rs:~147`, `!ended.succeeded()`) puts it in `failures` (`:1001-1008`) beside any red check, and a change run buys a repair (`:1041-1101`). The ending (`Ended::TimedOut`, `procs.rs:685`) is on the outcome but dropped there.
- The baseline already refuses to blame code for a timeout: `TimedOut` is `baseline_unrunnable`, "not a failure in the code: either this machine was contended when it ran, or the limit is too low" (`runner/baseline.rs:~170-225, :385`), and its comment says the candidate side deliberately does not do the same.
- A retry path exists and already treats a timeout as retryable: `CommandSpec.retry` drives `run_observed_within` (`verify/mod.rs:1160-1210`); each attempt gets its own log, re-draws its clipped timeout and a new lease and host slots, and a fail-then-pass emits `flaky`. It is opt-in per check (only `make plugin` has `retry = 1`) and not timeout-specific.
- relais records no contention fact at the decision point (heartbeat CPU and `load_average()` are not kept on `CheckOutcome`), so "was the machine busy" cannot be decided reliably; a second run can.

A timeout can still be the code's fault (a hang the worker introduced), so it must not become free: one more run tells a slow machine from a hanging change.

**Intended outcome:** a check that ends `TimedOut` on its own, unclipped limit is run once more, by itself and with no worker, before it counts. If the re-run passes, the check passes and is marked `flaky` with its reason; if it times out again or fails, it counts as today, so a real hang still gets its repair.

## 1. One implicit re-run for a timeout (Rust)

- In `run_observed_within` (`verify/mod.rs:1160-1210`): when an attempt ends `Ended::TimedOut` and its applied limit was its own declared limit (not clipped below it by a budget, `Budgets::clip`), grant one extra attempt even if `spec.retry` is 0 or already used. At most one such extra attempt per check per verification. `CheckOutcome` keeps no applied limit on main (`verify/mod.rs:115-141`): `run_attempt` records the applied limit and `clipped = timeout < own_timeout` (`:1236`) on the outcome and on `Attempt`, `#[serde(default)]`, and the decision reads that field.
- It goes through the same path as a declared retry: its own log (`<stem>-retry<N>.log`), a fresh clip of the timeout, a new heavy lease and host slots; a budget that runs out meanwhile ends the run on the budget as today (`attempt.rs:921-934`).
- A pass after a timeout emits `flaky` with a reason, `timed_out_then_passed`, so the pane and the receipt say why it ran twice (new `Event::Flaky.reason: Option<String>`, skipped when none, `protocol.rs:197`; new `CheckOutcome.flaky_reason`, `#[serde(default)]`; SPEC §29 updated); a second timeout or a red re-run ends the check as today.
- Applies to baseline, candidate and holdout checks (`holdouts.rs:546`; the holdout keeps every attempt's log, not only the last, `:562`); setup steps (`run_setup_gated_within`, `verify/mod.rs:1715`) are excluded, because re-running a killed install is not known to be safe; a baseline that passes on its re-run is no longer `baseline_unrunnable`.
- `Cancelled`, `Signalled`, exit != 0 and resource exhaustion are untouched: the extra attempt is for a timeout only.
- Extend the budget-exhaustion guard in `runner/attempt.rs` to inspect timed-out holdout outcomes as well as profile checks, before gap or behavioral-failure handling. Add a runner test with passing profile checks and a holdout whose first attempt hits its own limit and whose retry exhausts work or total: the run must end on the exhausted budget, with no behavioral-repair transition.

## Numbers

- Worst-case added wall time per timed-out check = its own limit: `make test` +1020 s, `make lint` +300 s, both +1320 s per verification, repeated on every re-verification after a repair. A re-run that its budget clips and that times out again ends on the budget (`attempt.rs:921-934`): no repair is paid.

## 2. Docs

- `docs/SPEC.md` (the retry paragraph near `:894` and the budgets paragraph near `:360`): "A check that ends on its own time limit runs once more, alone and with no worker, before it is judged; a pass on that run is `flaky` (`timed_out_then_passed`). A check cut by a run budget is not re-run (§budgets)."
- `CHANGELOG.md` `## Unreleased`: one bullet.

## Verification

- `make check` → exit 0.
- New tests (`verify/mod.rs` tests, `runner/tests/`):
  - a check that sleeps past its limit on the first run and exits 0 on the second (a marker file the first run creates) passes, with two attempts, two logs and a `flaky` event whose reason is `timed_out_then_passed`;
  - a check that times out twice fails, with exactly two attempts (no third);
  - a check with `retry = 1` that times out every time runs exactly twice: the extra attempt is granted only when no earlier attempt already followed a timeout;
  - a check that exits 1 is not re-run when `retry` is 0 (`a_check_without_retry_runs_once` still holds);
  - a check whose timeout was clipped by a spent budget is not re-run and the run ends on the budget (existing budget tests still pass);
  - at runner level, a change run whose only failure is a first-run timeout that passes on the re-run is accepted with no repair dispatched (no second worker in the events);
  - a baseline check that times out once and passes on the re-run is not `baseline_unrunnable`.
- Production: the resumed Phase 0 replays → expected: each first-run timeout shows a `flaky` event with reason `timed_out_then_passed`, or ends on the budget, and no `repair · behavioral_failure` has a `TimedOut` as its only failure → actual: recorded in `2026-10-27-answered-outcome.md` Phase 0 (not part of this branch).
- The existing `timeouts_kill_the_check_and_count_as_failures` and `a_timeout_relais_sent_is_not_memory` are updated to the new rule (a timeout that also times out on the re-run still fails), not deleted.

## Verification record (2026-10-31, before push)

- `make check` on `df05d4fb` (after the implementation-review fixes): exit 0. Rust 2118 + 41 + 284 + 1 passed, 0 failed; plugin 355 pass, 0 fail.
- `make check` on the applied candidate, committed as `4e9e29e8`: exit 0 in 1435 s at load about 10. Rust 2117 + 41 + 284 + 1 passed, 0 failed; plugin 355 pass, 0 fail.
- Named tests, all pass: `a_check_that_times_out_then_passes_is_flaky_timed_out_then_passed`, `a_check_that_times_out_twice_runs_exactly_twice`, `a_check_with_retry_one_that_always_times_out_runs_exactly_twice`, `a_clipped_timeout_is_not_run_again`, `a_check_outcome_without_the_new_fields_still_parses`, `a_setup_step_that_times_out_is_not_run_again`, `a_change_whose_only_failure_is_a_first_run_timeout_is_accepted_without_a_repair`, `a_baseline_check_that_times_out_once_then_passes_is_not_unrunnable`, `a_holdout_retry_that_exhausts_the_budget_ends_on_that_budget`, `a_baseline_retry_draws_on_the_total_not_on_work`; the updated `timeouts_kill_the_check_and_count_as_failures` and the unchanged `a_timeout_relais_sent_is_not_memory` and `a_check_without_retry_runs_once` pass (`a_timeout_relais_sent_is_not_memory` needed no change: a timeout, on one run or two, is still never classified as memory).
- The relais run that wrote it (`run-65f224f342308-1606d`) spent $1.40 on its worker, then waited 3 h for a heavy slot and verified nothing; recorded `salvaged` at `4e9e29e8`.
- The production check named in Verification is not part of this branch: it is the next step of `2026-10-27-answered-outcome.md` Phase 0 (the resumed replays), and its counts are recorded there.

- `a_holdout_that_times_out_then_passes_is_flaky_and_keeps_both_attempts` (added after the implementation review): pass, flaky reason, two attempts (the first `TimedOut`, not clipped), the kept `log-attempt1.txt`, a serde round trip of the receipt's `HoldoutRun`.

## Not in scope

- Recording load or CPU on `CheckOutcome`, or deciding on contention directly.
- Changing any declared `timeout_seconds` or `retry` in `relais.toml`.
- The 10 remaining and 4 retried replays of Phase 0 (they resume after this ships).

## Decision log

- D1 (2026-10-31, person) Pause the replays and fix relais first: "Pause, corriger relais d'abord (Recommandé)".
- D2 (2026-10-31) One re-run, not a contention probe: relais keeps no contention fact, a second run is the cheapest evidence, and a hang still fails twice and gets its repair.
- D3 (2026-10-31, implementation review) Deliberate: `a_baseline_retry_draws_on_the_total_not_on_work` shows that the work budget is not spent by a baseline re-run, not that the total went down by the timed-out limit: `Budgets` is not observable from a run's outcome, and the total's charge for pre-work time is covered by the `policy::budgets` unit tests.
- D4 (2026-10-31, implementation review) Deliberate: `a_holdout_retry_that_exhausts_the_budget_ends_on_that_budget` cannot show that its holdout ran twice. With a 4 s total, preflight and the baseline often spend more than 2 s on this machine, so the holdout's first attempt is already clipped and is not re-run; an assertion on the kept `log-attempt1.txt` failed for that reason (tried 2026-10-31). The test keeps what it guarantees: the run ends on the total, with no behavioral repair. The re-run of a holdout is shown by `a_holdout_that_times_out_then_passes_is_flaky_and_keeps_both_attempts`.
- D5 (2026-10-31, implementation review) `a_timeout_relais_sent_is_not_memory` was not changed, against the Verification line that planned to update it: it asserts only that a timed-out check is not classified as memory, which holds whether the check ran once or twice.

## Implementation review

- Verdict: approve on tree d8e4f4f3 (Delta), after approve-with-changes on 887ba339, af420a63 and 157482a7.
- Fixed: six test cleanups explained; a holdout timed-out-then-passed test with a serde round trip; the production check moved to the answered-outcome plan; a test comment and the SPEC cross-reference corrected.
- Deliberate: D3 (the total's decrease is not observable from a run), D4 (the budget holdout test cannot show a re-run), D5 (`a_timeout_relais_sent_is_not_memory` needed no change).
- Open for the person, noted by the reviewer: with `retry = 1`, a check that exits 1 then times out runs a third time; holdouts record the flaky reason on the receipt only, not as an event.

<!-- panel: repos=relais adds= reviewers=backend,codex body-sha=670fecbc6c98 -->
