---
status: done
branch: feat/budget-stop-by-cause
repos: [relais]
adrs: [ADR-0006, ADR-0010]
plan-id: p-01M7Z42BAXT1AKBRAJ36CX4CHM
---
# Settle budget-exhausted trials by cause

## Review panel

👉 **Decide:** none — approve if a budget the arm spent itself should count against its acceptance, not its validity.
📍 relais · 3 phases · 1 release · next: Phase 1. Panel: backend, lang:rust, tui, unix (opus), codex (gpt).
**Changed by review:** readings move onto `ledger::Clock` with a sleep-counting continuous clock; the detail line names the cause only; total-bound package stops stay `Total`.
**Verdicts:** 3 approve, 2 approve-with-changes; 11 carried; words 1450 → 1934 (×1.33, the growth is blocking, high and correctness fixes); remove 1, merge 0.
📄 Full reviews: `2026-11-21-budget-stop-by-cause.reviews.md`

## Picture

```
 today                            after this plan
 -----                            ---------------
 trial ends budget_exhausted      trial ends budget_exhausted
        |                                |
        v                                v
     errored                      BudgetCause on the terminal
 (validity only)                         |
                        +----------------+------------------+
                        |                                   |
             Attempts,                         Work with a suspend,
             Work with no suspend              Spend, Admission,
                        |                      Total, RepairFloor
                        v                                   v
                    rejected                             errored
            (streak and Wilson bound)               (validity only)
```

A trial whose run exhausted its budget used to settle errored whatever
the cause; after this plan its `BudgetCause` decides, so a budget the arm
spent itself counts as a rejection and a queue, host or configuration
stop stays errored.

## Context

The blue-factory programme ticket *Count budget_exhausted as a failure, not
errored* (t22) asks whether a trial that ends `budget_exhausted` should keep
settling as `errored`. Today `trial_outcome_of`
(`crates/relais/src/runner/live_trial.rs:424`) maps every `BudgetExhausted`
end to `Errored`. As a result:

- under the 80% validity floor (ADR-0006 §1, amended 2026-11-20), an arm that
  burns its own work budget counts against *validity* instead of against
  *acceptance*;
- the 15-trial streak never sees it at all.

The five `budget_exhausted` trials in the ledger, read 2026-11-21. All five
stopped on `limit = wall_clock`, the work budget. Waits have been excluded
from that budget since #267 (2026-10-19).

| Trial | What the arm did | Who ran it out |
|---|---|---|
| 9e08 (10-13) | Its worker took 44 min (the incumbent's took 20). Attempt 1 failed `make`, and no work budget was left for a repair. | the arm |
| 160a0 (10-30) | The check `make test` started 21:18:06 and ended 22:00:24: 2,538 s of wall time, but 405 s on the monotonic clock. Nothing was logged between 21:23 and 21:59, so the host was asleep. | the machine |
| 17b36 (10-31) | Attempt 1 failed. The repair returned the **same** candidate sha, `a144560…`, and relais re-verified it until the budget ran out. | the arm |
| ddd6 (10-31) | Attempt 1 failed. The workers took 17 and 15 min, against the incumbent's 4 and 2. Verification ran at normal speed. | the arm |
| 4802 (10-31) | Attempt 1 failed. The repair returned the same sha. | the arm |

Across the whole fleet, 72 `budget_exhausted` stops were `wall_clock`, 3
were `attempts`, 1 was `total`, and none was `spend`. Of the 60 wall-clock
stops whose events survive, 6 hold a check whose wall time exceeds its
monotonic `duration_ms` by more than 5 min. Those six are host suspends.

**Verdict: split by cause.**

- A budget the arm spent itself is a rejection.
- A budget that a queue, the host or the configuration took is still
  errored.

## Goal

From this release, the trials of a revision count a stop by its cause:

- a budget stop the arm caused settles `rejected`, and counts toward the
  streak and the Wilson bound;
- a budget stop caused by a queue, the host or the configuration stays
  `errored`.

No settled trial is rewritten. A revision that is already mid-gate keeps the
bar it started under.

## Non-goals

- **The live rollback (ADR-0006 §3) and the dataset's labels stay as they
  are.** Both read `learn/dataset.rs` `state_labelling` through
  `learn/cycle.rs` `routed_outcome`, never `trial_outcome_of`.
  `BudgetExhausted` stays excluded there, because a training label needs a
  tested reasoning outcome. Changing what the rollback counts is a separate
  question, filed as a follow-up.
- **The work budget still charges suspended time.** Charging work on an
  awake clock, so that a sleep never exhausts a run, changes how long every
  run lasts and amends ADR-0010. It is filed as a follow-up.
- **A repair that returns the same candidate sha is not short-circuited.**
  That waste showed up in 17b36 and 4802; it is filed as a follow-up.
- **No settled trial is backfilled.** `settle_trial` writes an outcome once.
- **No promotion decision changes.** No arm in the ledger is near 14 of 15
  (the best has 7 accepted against 4 rejected).

## Code review

- relais: backend, lang:rust, unix, tui (the stop's detail text changes; the suspend reading depends on each platform's monotonic clocks)

## Behaviour

- **A structured cause on the terminal.** `Terminal::BudgetExhausted` carries
  a `cause: BudgetCause`, built in `runner/machine.rs` `decide` from the
  `Limit`. The match is exhaustive, so a new limit does not compile until it
  names its side. The causes are:
  - `Attempts`
  - `Work { suspended_ms }`
  - `Spend`
  - `Admission`
  - `Total`
  - `RepairFloor`
- **Mapping under the new rule (`BudgetRule::ByCause`):**
  - `Attempts` is **Rejected**: the attempt ceiling was reached with
    failures.
  - `Work { suspended_ms }` with `suspended_ms < 60_000` is **Rejected**:
    the worker and its checks spent the work budget, since waits are
    already excluded from it.
  - `Work` with `suspended_ms >= 60_000` is **Errored**: the host slept
    inside the work window.
  - `Spend`, `Admission`, `Total` and `RepairFloor` are **Errored**: a
    spending ceiling (per run or per day, never reached in the fleet so
    far), a queue, the end-to-end limit and configuration.
- **The suspended time.** Production budgets read `LedgerBudgetClock`
  (`engine.rs`), which wraps `ledger::Clock`. So the two new readings go on
  `ledger::Clock` and are forwarded by `LedgerBudgetClock` and `BudgetClock`:
  - `awake_millis()` comes from a process-wide `Instant` anchor, which does
    not advance while the host sleeps (trial 160a0: 405 s against 2,538 s);
  - `continuous_millis()` comes from a monotonic clock that counts sleep
    and never jumps: `mach_continuous_time` on darwin, `CLOCK_BOOTTIME` on
    Linux, and the awake reading elsewhere, where no suspend is detected.

  Both default to the parsed `now_rfc3339`, so a scripted clock reports no
  suspend unless its test overrides one, and `SkewClock`'s jumps are never
  read as sleep. The wall clock (`now_millis`) is not used here, so an NTP
  step is never a suspend.

  `Budgets` stamps both readings when work starts, and at each `Wait` and
  `BaseRun`. At a work exhaustion, `suspended_ms` is the continuous elapsed
  minus the awake elapsed, less the suspend inside charged waits (already
  outside work), floored at 0. It goes into the stop's evidence JSON
  (`"suspended_ms"`) beside `"limit"`.
- **The cutover.** `route/candidate.rs` gains
  `TYPED_GATE_BUDGET_CAUSE_FROM`, set to the release instant, and
  `budget_rule(first_trial_at) -> BudgetRule`, on the same pattern as
  `validity_floor_rule`:
  - a revision first trialled before the instant is `Errored` (the old
    mapping);
  - a revision first trialled at or after it, or not trialled yet, is
    `ByCause`.

  Both settle paths look up the arm revision's first trial before mapping:
  - `cli/replay.rs` `record_replay_trial`;
  - `runner/live_trial.rs` `settle`, called from `cli/run.rs`.

  They read the settled trial's `arm_recipe_id`, then the `created_at` of
  that recipe id's oldest trial through a new `Ledger` query (not
  `typed_gate::facts_of`, which loads every fact).
- **What the person sees.**
  - The stop's detail line (`runner/machine.rs`) names the cause only:
    "the work budget is exhausted (host suspended 38 min)", or "the work
    budget is exhausted (spent by the run)". It never names a trial outcome,
    because the rule is known only at settle, per revision, and a run that
    is not a trial is not counted. The outcome is the end line's `outcome=`.
    Durations are whole minutes; milliseconds stay in the evidence JSON.
  - The replay end line (stderr) shows the new `outcome=`, and its grammar
    is unchanged.
  - The exit code stays 5 (`cli/outcome.rs`) for every `BudgetCause`.

## Phases

- [x] Phase 1 — a structured budget cause and the suspended time, in
  `crates/relais/src/runner/machine.rs`,
  `crates/relais/src/runner/scheduler.rs`,
  `crates/relais/src/runner/engine.rs`,
  `crates/relais/src/policy/budgets.rs`,
  `crates/relais/src/ledger/clock.rs` (`Clock::awake_millis` and `continuous_millis`, as in Behaviour; `LedgerBudgetClock` in `engine.rs` forwards them) and
  `crates/relais/src/policy/mod.rs`:
  - add `BudgetCause` on `Terminal::BudgetExhausted`;
  - add `awake_millis`, `continuous_millis` and `suspended_ms` on
    `Limit::WallClock`, and the cause in the detail line;
  - add the evidence keys;
  - keep which budget binds through `scheduler.rs`: `Budgets::remaining()`
    is the smaller of work and total, so the pre-package stop (~678) names
    the binding budget (`Total` when total is the smaller), and a child's
    inherited `wall_seconds` (~755) remembers when the root's total capped
    it, so the child's stop surfaces as `Total`, never as an arm's `Work`.
    The terminal rebuild (~924) keeps that cause. Tests: total binding
    before dispatch, total binding through a child, and a work-bound
    control.

  Tests:
  - a scripted clock whose awake scale lags yields `suspended_ms`;
  - the default clock yields 0;
  - a wall-clock step forward of 120 s with no suspend yields 0;
  - a 120 s suspend inside an admission wait yields 0;
  - an engine run (`runner/tests/budgets.rs`) whose ledger clock's awake reading lags stops with evidence `suspended_ms` >= 60000, and the existing `SkewClock` work stop reports `suspended_ms: 0`;
  - each `Limit` maps to its cause.
- [x] Phase 2 — the mapping and the cutover, in
  `crates/relais/src/runner/live_trial.rs`,
  `crates/relais/src/route/candidate.rs`, `crates/relais/src/route/mod.rs`,
  `crates/relais/src/cli/replay.rs`, `crates/relais/src/cli/run.rs`,
  `crates/relais/src/ledger/trials.rs` (the first-trial query) and
  `crates/relais/src/learn/typed_gate.rs` (its test):
  - `trial_outcome_of(terminal, escalated, rule)`;
  - `budget_rule`, with a boundary test at the cutoff instant;
  - a table test over every cause × both rules;
  - a `typed_gate` test, settled through `live_trial::settle` with a
    `Work { suspended_ms: 0 }` terminal: the stop breaks the streak under
    `ByCause`, and only costs validity under `Errored`;
  - a settle test where a revision first trialled before the cutoff stays
    `errored`, through the new `Ledger` first-trial query;
  - the bindings of `Terminal::BudgetExhausted { detail }` in `cli/run.rs`
    and `runner/tests/mod.rs` follow the new field.
- [x] Phase 3 — ADR-0006 §1 amendment, the spec and the release notes:
  `.adr/records/0006-a-change-runs-below-implementation-only-on-evidence.md`,
  `docs/SPEC.md` and `CHANGELOG.md`.
  - §1 gains one sentence: from `TYPED_GATE_BUDGET_CAUSE_FROM`, a budget stop
    the arm caused (the attempt ceiling, or work with no host suspend) settles rejected, and the others stay errored.
  - `aval heads --write`.

## Decision log

- 2026-11-21: split by cause, not a blanket reclassification. Of the five
  trials, four were the arm's and one was a host suspend, so a blanket
  rule would mislabel 160a0.
- 2026-11-21: detect the suspend at the stop, not by charging work on an
  awake clock. That fix is narrower and changes no run's duration; the
  awake-clock budget is filed as a follow-up ticket.
- 2026-11-21: a per-revision cutover, following the floor's precedent that
  a revision keeps the bar it started under (the person, 2026-11-20).
- 2026-11-21: every `Spend` stop stays errored, with no per-run or per-day
  split: none of 76 fleet stops was a spend stop (review, backend).
- 2026-11-21: the 60 s suspend threshold. The check skews measured were
  either 0 s or 58 s and more; native `SUSPEND_GAP` is 10 s, but work also
  holds short scheduler stalls.
- 2026-11-21: Phases 1–2 came from relais run `run-660ca667b0445-5b52`
  (sonnet). Its worker hit its one-hour wall time with the candidate whole
  but unverified. A verify run then found two failures, and both were fixed
  by hand:
  - the sleep-counting clock read now lives in `procs.rs`, the one module
    that may name `libc` (`check-module-cycles`);
  - the `typed_gate` test's trial now records the research model's
    dispatch, without which the gate never counts it.
- 2026-11-21: `TYPED_GATE_BUDGET_CAUSE_FROM` is `2026-11-22T00:00:00Z` on
  the branch. The release sets it to its own instant, as the floor's
  cutoff was.

## Implementation review

- implementation-review (opus): approve after 4 rounds (49k, 43k, 38k, 42k). Fixed: helper names, the changelog's stop, process statics in one documented exemption, the output parameter.
- backend (opus): approve after 3 rounds. Fixed: one clock source, a real-clock test, the tested fallback. Deliberate: the cutover is set in the release PR; no clock warning.
- lang:rust (opus): approve after 3 rounds. Fixed: `WorkBound` enum, `Option<Mark>`, an exhaustive match, imports, saturating maths. Deliberate: `first_trial_at` stays a String; `suspended_ms` stays u64 ms.
- unix (opus): approve after 2 rounds. Fixed: the changelog names `outcome=`. Answered with a measurement: darwin `CLOCK_MONOTONIC` counts sleep (5,253 s ahead of `CLOCK_UPTIME_RAW`).
- tui (opus): approve after 2 rounds. Fixed: next steps on the work-budget line. Deliberate: minutes there, seconds on the repair-floor line.

## Verification

| Check | Input | Expected |
|---|---|---|
| Unit | `cargo test -p relais --lib -- learn::typed_gate:: live_trial:: route::candidate:: policy::budgets:: runner::tests::budgets::` | green, with the new table and boundary tests. **Actual:** 116 passed, 0 failed |
| Gate | `cargo test -p relais --lib learn::typed_gate::` | green: a `Work` stop breaks the streak under `ByCause`. **Actual:** passed (in the run above), after the dispatch fix |
| Suite | `make check` | green. **Actual:** the pre-commit gate on `f5b5e04f` passed 33 checks, the full Rust and plugin suites included |
| Piloted | the branch binary with `RELAIS_STATE_DIR` set to a scratch directory (never the live ledger), running `relais dataset replay` of one task against a recipe revision with no earlier trial, with `limits.wall_seconds` low enough to stop in work | a stop with evidence `{"limit":"wall_clock","suspended_ms":0}`, the trial row `outcome = rejected`, the end line `outcome=rejected`, and a detail line that names the cause; read with `NO_COLOR=1` and wrapped at 80 columns. **Actual:** not run. A scratch ledger holds no replayable source task, and `relais run` is refused outside the plugin. The engine tests (`a_work_stop_with_no_suspend_reports_none`, `a_work_stop_after_a_host_suspend_names_the_suspend`) pin the evidence and the detail line; the 7-day recount (blue-factory t25) observes it live |
| Ledger | `sqlite3 -readonly` over `trials` | the five old trials are still `errored`. **Actual:** 5 errored |
| Suspend | engine test with a scripted clock whose awake scale skips 120 s inside work | evidence `suspended_ms >= 60000`, trial `errored`; a 120 s skip inside an admission wait gives `suspended_ms` 0 and `rejected`. **Actual:** passed (`a_host_sleep_inside_work_is_read_as_suspended_time`, `a_sleep_inside_an_admission_wait_is_not_suspended_work`) |
| Observe, 7 days after release (blue-factory t25) | `sqlite3 -readonly`: trials since the release whose run ended `budget_exhausted`, by outcome and by `suspended_ms >= 60000` | rejected about 80%, suspended about 10%; a ratio far off is a mapping bug |


<!-- panel: repos=relais reviewers=backend,lang:rust,tui,unix,codex body-sha=5a529fff8124 -->
