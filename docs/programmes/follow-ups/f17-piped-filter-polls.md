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
- `error:`, `error[`, `Error:`, `ERROR:`, `FAILURE:`, `npm ERR!`,
  `make: ***`, `panicked`, `fatal:`, `Traceback`, and vitest's `×`;
- a count of at least one before `failed`, `error` or `errors`
  (`1 failed`, `Found 2 errors.`).

`0 failed`, `Found 0 errors.` and a vitest summary with only passes are not
markers. Plain `failed` or `error` would have matched those zero counts.

**Tests.** `a_wait_poll_with_no_failure_text_does_not_fire` feeds the
`65f3d9db` poll command 6 times. It uses the literal
`(Bash completed with no output)` result the transcript holds, and an empty
one. `shows_failure_reads_markers_and_skips_zero_counts` pins the marker list both ways.
`a_piped_filter_that_failed_still_counts_by_its_error_line` covers
`pipefail`. The three earlier piped-filter tests pass unchanged.

**Replay** (2026-11-24). Two subagent transcripts on disk under session
`a6a522f6` hold the same poll shape as `65f3d9db` and `65ec661c`. Their call
counts (140 and 267) do not match the f15 table, so they are not claimed to
be those runs. One is an `s9a_run.txt` vitest poll, the other a
`vt-full.out` `DONE-RC` poll. They were fed through `LoopGuard::push_chunk`
in a temporary test that was not committed:

| transcript | old rule fires | new rule fires |
|---|---|---|
| `agent-a4ecee556c8370d82` (`s9a_run.txt` poll) | 6 | 0 |
| `agent-a710d4d756a995dba` (`vt-full.out` poll) | 1 | 0 |

**What is left.** A piped filter that fails under `pipefail` with no
marker now counts by its first error line (`Exit code N`), like any failing
Bash command. That includes a `| head` wait poll under `pipefail`: when
`head` exits early the writer gets SIGPIPE and the pipeline exits 141, so a
succeeding poll can count as `Exit code 141`. A failing run without
`pipefail` whose filtered output shows no marker reads as a success and
clears its counts. A read-only `grep -rn "error:" src | head` repeated
unchanged, or a vitest progress line such as `(3 tests | 1 failed)`, can
still count by hash. A wait poll whose filtered output shows a stable early `FAIL`
line while the suite still runs is still counted by hash and can reach 3.
The f15 data had no such case; the shadow measurement will show whether it
matters.

The plan's Carried line and its fingerprint rule are updated.

**Implementation review** (round 1, 2026-11-24):

- implementation-review: approve-with-changes; restored a `holds-until:` marker naming the ceiling; noted the silent non-pipefail case; renamed the marker test. 62k tok, 47 s.
- unix: approve-with-changes; added `make: ***`, `npm ERR!`, `ERROR:`, `FAILURE:`; SIGPIPE 141 recorded above. 45k tok, 32 s.
- lang:rust: approve; the word scan no longer allocates; two test names fixed; vitest `×` added. 46k tok, 42 s.
- backend: approve-with-changes; the early-`FAIL` limit now sits beside the gate in the `holds-until:` marker. 53k tok, 46 s.
