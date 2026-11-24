+++
type = "task"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais"]
+++
# The loop guard counts a piped filter only on failure text

## Question

The loop guard's piped-filter clause (`CommandShape::ends_in_output_filter`
in `crates/relais/src/adapter/loopguard.rs`) counts a command whose last
segment is a piped `tail`, `head` or `grep` by its output hash, whether it
failed or not. The replay in
[f15](f15-loop-breaker-wider-fingerprint.md) found it firing at 3 or more
only on wait loops that later made progress:

- `65f3d9db`: `for …; sleep 5; done; grep … | head`, ×6, after which the
  suite passed with 817 tests;
- `65ec661c`: ×3.

Its other clause, failing Bash results, never fired. The plan's carried item
fixes this: count a piped filter only when its output also matches a
failure marker. Should that land before `loop_threshold` is ever enabled,
and which markers does it use?

## Look at

- `docs/plans/2026-11-08-loop-breaker-native-workers.md`: the Carried line
  and the Rollout bar.
- `crates/relais/src/adapter/loopguard.rs`: `CommandShape` and the
  `holds-until` comment near line 195.
- The f15 Resolution, for the transcripts and how to match them.

## Answer looks like

A change in which an empty or non-failing piped-filter output no longer
counts, with a test built from the `65f3d9db` poll shape. Alternatively, a
recorded reason to keep the clause as it is. Either way, the plan's Carried
line is updated.

## Resolution

**Yes, before `loop_threshold` is ever enabled, and it is done.** A
command whose last segment is a piped `tail`, `head` or `grep` is now
counted by its output hash only when that output shows a failure marker.
Otherwise it is read like any other Bash result: a failing one
(`is_error`, as under `pipefail`) counts by its first error line, and a
succeeding one clears that command's counts.

**The markers** live in one place, `shows_failure` in
`crates/relais/src/adapter/loopguard.rs`, matched with case as written:

- `FAIL` or `FAILED` as a word;
- `error:`, `error[`, `Error:`, `panicked`, `fatal:`, `Traceback`;
- a count of at least one before `failed`, `error` or `errors`
  (`1 failed`, `Found 2 errors.`).

`0 failed`, `Found 0 errors.` and a vitest summary with only passes are not
markers. Plain `failed` or `error` would have matched those zero counts.

**Tests.** `a_wait_poll_with_no_failure_text_does_not_fire` feeds the
`65f3d9db` poll command 6 times. It uses the literal
`(Bash completed with no output)` result the transcript holds, and an empty
one. `failure_markers_ignore_zero_counts` pins the marker list both ways.
`a_piped_filter_that_failed_still_counts_by_its_error_line` covers
`pipefail`. The three earlier piped-filter tests pass unchanged.

**Replay** (2026-11-24). Two subagent transcripts of the f15 replay
session, `a6a522f6`, hold the poll shapes. One is the `s9a_run.txt` vitest
poll. The other is a `vt-full.out` `DONE-RC` poll. They were fed through
`LoopGuard::push_chunk` in a temporary test that was not committed:

| transcript | old rule fires | new rule fires |
|---|---|---|
| `agent-a4ecee556c8370d82` (`s9a_run.txt` poll, up to ×6) | 6 | 0 |
| `agent-a710d4d756a995dba` (`vt-full.out` poll, ×3) | 1 | 0 |

The plan's Carried line and its fingerprint rule are updated.
