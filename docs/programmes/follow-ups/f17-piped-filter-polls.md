+++
type = "task"
mode = "afk"
status = "open"
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
