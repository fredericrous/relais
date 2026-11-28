+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# A baseline retry runs out its work budget on Windows CI

## Question

On PR #640's CI (run 46089638652, 2026-11-28),
`runner::tests::timeout_rerun::a_baseline_retry_draws_on_the_total_not_on_work`
failed on `windows-latest` with `BudgetExhausted { … cause: Work { suspended_ms: 0 } }`
instead of the attempt-ceiling stop. The rerun of the same job passed. The
test gives the run `limits.wall_seconds = 6`; the base check sleeps 30 s
under a 10 s timeout, then passes on its retry. On a slow Windows runner, the
worker's own time and the rerun's bookkeeping seem to use up the 6 s work
budget before the attempt ends. Is the budget too tight for the slowest CI
host (#378 already widened it once), or does part of the baseline retry
still draw on work instead of on the total?

## Look at

- `crates/relais/src/runner/tests/timeout_rerun.rs:136-175`, the test and its
  `wall_seconds = Some(6)`.
- `911f5d343 test(runner): room for work on a slow host (#378)`, the last
  widening.
- The work and total budget split in `crates/relais/src/runner` (ADR-0013
  suspended time).

## Answer looks like

Either a measured fix (the base's retry time shown to land on the total, with
the work budget widened to fit the slowest observed host), or a fix to the
draw if part of the retry is charged to work, each passing the Windows job
three times in a row.
