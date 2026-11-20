+++
type = "task"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais"]
+++
# The end-session tombstone test is flaky on macOS CI

## Question

Why did `programme::claim::tests::an_ending_session_skips_a_failing_programme_and_still_leaves_its_tombstone` report `done 0 != 1` once on macOS CI (run 44678704861) while it passes locally and on the rerun?

## Look at

`crates/relais/src/programme/claim.rs` (the test, near line 565) and `claim/store.rs` `end_session_once` / `once_per_programme`, which try each programme lock once with `Duration::ZERO`.

## Answer looks like

The cause, a fix with a test that reproduces it under load (or a loop that runs it 200 times green), merged.

## Resolution

2026-11-20. **Cause:** a concurrent fork in the same test binary. A
`flock` belongs to the open file description, so a process forked while
the test's first block held the `good` lock keeps a copy of that
descriptor until it execs. The lib suite forks: verification checks run
`setsid` in `pre_exec` (`procs.rs`), which rules out `posix_spawn`.
`end_session_once` tried each lock once with `Duration::ZERO`, so for that
moment the freed lock read as held: `done` was 0, `skipped` was 2.
`LockFile`'s own doc names this window and says callers retry. The claims
pass did not retry.

**Loop:** the scenario run 200 times, with four threads doing fork+exec
(`pre_exec`) beside it: 0/200 red without the threads, 29–32/200 with them.

**Fix:** a refresh or end pass waits for the programme locks up to
`ONCE_GRACE` (30 ms). That is one deadline for the whole pass, not one per
programme. A lock still busy after it is skipped as before. The grace is
capped by the 100 ms the SessionStart and SessionEnd hooks are held to,
which two `hook::respond` tests enforce. A first try at 200 ms gave 0/1000
red under the forking threads but failed those two tests. At 30 ms the
loop gives 1/1000 red under four threads that fork nonstop, a load far
heavier than the suite's.

**Regression test:** `an_ending_session_waits_out_a_lock_held_for_a_moment`
holds the lock from another thread for 100 ms. It calls the pass through
`end_session_within` with a 2 s grace, so it tests the waiting and not the
timing. It was red on the zero-wait code, with `(done, skipped) = (0, 1)`.
The flaky test itself now calls `end_session_within` with a 2 s grace too:
it is about a failing programme, not timing.
`a_session_refresh_moves_only_its_own_work_claims_past_n` had the same
exposure, and the production grace covers it.

The production lock holders are the short-lived `relais programme` CLI and
the hook processes. It is inferred, not checked, that neither forks while
it holds a claim lock, so this window is mostly a test-binary effect.
