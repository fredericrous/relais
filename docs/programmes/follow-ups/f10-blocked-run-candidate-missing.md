+++
type = "task"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais"]
+++
# A run blocked `env_missing` keeps a patch that `verify` cannot find

## Question

On 2026-11-21, run `run-660cd84581580-1373d` (a Phase 1 change of the
learning-loop entry plan, worker sonnet, in place in a linked worktree)
ended `blocked (env_missing)`: the worker's focused `cargo test` returned
no result inside its budget (the full suite takes 15–28 minutes on the busy
host). Its artifacts hold a complete `candidate-final.patch` (10 files,
+191), yet a `kind: verify` run naming `candidate {run}` answered
`blocked (candidate_missing): run … produced no candidate`. The patch was
salvaged by `git apply` and checked by hand. Should the final patch of a
blocked run be registered as a candidate (so `verify` and `from_candidate`
find it), or should the blocked outcome say that none is kept?

## Look at

- `~/.local/state/relais/runs/run-660cd84581580-1373d/` (`attempt-1-result.txt`,
  `candidate-final.patch`, `events.jsonl`), the `env_missing` stop in
  `crates/relais/src/runner/` and where `candidate-N.patch` is registered
  for `verify`; compare with the `admission_wait_exhausted` outcome, whose
  message does name `candidate {run, index}`.

## Answer looks like

Either `verify` on such a run applies the final patch (with a test that a
blocked-with-patch run yields a candidate), or the `env_missing` message
says the candidate is not kept and the skill's recovery text matches; one
of the two merged.

## Resolution

The final patch of a blocked run is registered. A worker that claims a
blockage has its changed tree published as the blocked attempt's candidate
by the path a confirmed-stopped timed-out worker takes (`snapshot_stopped`):
`candidate-N.patch`, `candidate_sha` on the attempt, and a message that
names `candidate {run, index}`, as `admission_wait_exhausted` does. A
worker that changed nothing still leaves none. Tests:
`a_blocked_worker_that_edited_leaves_a_candidate_verify_applies` and
`a_blocked_worker_that_changed_nothing_leaves_no_candidate`.
