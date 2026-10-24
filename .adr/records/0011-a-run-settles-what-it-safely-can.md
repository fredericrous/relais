---
id: ADR-0011
status: accepted
decisions:
  - key: execution.settle-before-asking
    choice: a run settles three stops itself before asking a person - review findings all tagged `medium` or `low` send a `change` run back to its worker once (`review_repair`), a scope overflow made only of test paths outside protected prefixes is answered by relais as `revise` with the scope widened (actor `relais`, a `kind - verify` successor reviewed again; `[execution] widen_scope_for_tests`, default true), and a reviewer that ended with no terminal result is dispatched once more when it was not cancelled or out of budget; everything else stops as before, and each rule is bounded to once per run and recorded in the ledger
    first: true
    reason: 56 runs were salvaged by hand from 2026-10-19 to 10-23, 33 of them on these three stops (review findings 15, of which 8 parsed reviews held only `low` findings; scope overflow 10, mostly tests that read the changed code; reviewer unavailable 8, 13 of 14 recent ones "ended without a terminal result"), each one finished by the orchestrating session at Opus prices; the person chose all three fixes (2026-10-23)
---
# 0011 — a run settles what it safely can before asking a person

## Context

The salvage count was the largest hidden cost of relais: a run that stops for a person is finished by the main session, on the most expensive model, from a worker's partial candidate. From 10-19 to 10-23, 56 runs ended that way. Three stops made up 33 of them, and in each the next step was mechanical: fix the reviewer's minor findings, verify the tests the worker had to touch, or ask the reviewer again.

## Decision

1. **Review findings below `high` get one repair.** Reviewers tag each finding `[blocking|high|medium|low]`. When every finding is `medium` or `low`, a `change` run that has not had one goes back to the worker once with the findings quoted as data. The repaired candidate is checked and reviewed again; any finding then stops `needs_review`. Untagged findings, `high`, `blocking`, `verify` runs and parts reviews stop as before.
2. **A scope overflow made only of tests is verified with the scope widened.** When every out-of-scope path is a test path (the shared test-tree globs) outside protected prefixes, relais answers its own `scope_exceeded` as `revise --widen-scope`: actor `relais`, a `kind: verify` successor that is verified and reviewed again, its reviewer told which test paths were widened in. The run ends `cancelled (scope_widened_for_tests)` with the successor intent on its outcome; the plugin launches it. `[execution] widen_scope_for_tests = false` restores the stop. A scope violation is still never accepted (SPEC §8): the widened scope is a new contract.
3. **A reviewer with no result is retried once.** Only the cause `terminal_missing`, with no cancellation or exhaustion and time left on the work and total budgets, gets one more dispatch, clipped to those budgets (ADR-0010: exhaustion is evidence, never a repair).

## Consequences

- The actor `relais` is written only by relais; the CLI refuses it, and decisions it answers are excluded from decision-model labels.
- `review_repair` and automatic widens are counted in the report, so the 7-day check (salvages on these three causes below half their 33-in-4-days rate) reads them from the ledger.
- A run can take longer before it stops, and each rule spends at most one more worker or reviewer dispatch.
