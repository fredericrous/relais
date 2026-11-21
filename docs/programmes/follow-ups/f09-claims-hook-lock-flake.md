+++
type = "task"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais"]
+++
# The claims hook lock tests are flaky on CI

## Question

Why do `hook::respond::tests::claims::a_session_start_with_the_lock_held_elsewhere_returns_within_100_ms_and_logs_a_skip` and its sibling `a_session_end_with_the_lock_held_elsewhere_leaves_a_tombstone_the_next_locked_write_applies` fail now and then on CI? They failed on windows-latest for main `95ab2fbe` (CI run 44860944014, green on the rerun) and on macos-latest for `c063fce2` (run 44864210605). The first failure held the v0.75.0 release gate ("every platform passed on main") until the rerun.

## Look at

`crates/relais/src/hook/respond.rs` near line 2693 (the two tests), and f01's Resolution: `ONCE_GRACE` (30 ms) under the 100 ms the SessionStart and SessionEnd hooks are held to, and the fork-holds-a-lock window it names.

## Answer looks like

The cause, and a fix with a loop that runs both tests 200 times green under load (or under fork+exec threads, as f01's loop did), merged. A wider time bound is acceptable only if the hook's own 100 ms limit is not what the test proves.

## Resolution

2026-11-21. **Cause:** file I/O on a loaded disk, not the lock. Both CI
failures were the time bound (110.9 ms on macOS for SessionStart; 124.5 ms
and the SessionEnd bound on Windows). Tagged probes around each phase of
the pass, in the runs that went red, showed the lock wait ending on its
grace every time (wait 28.3–28.5 ms, took 28.8–29.6 ms). The time went to
the writes after it: the `skipped` log append took 11–56 ms and the
`ended/<session>` tombstone 25–95 ms. Neither syncs; the disk queue
stalls them. f01's fork-holds-a-lock window plays no part here.

**What the 100 ms is:** not the hook's limit. SessionStart and SessionEnd
are killed at `OTHER_HANDLER_TIMEOUT` (10 s) and a fresh start gathers
for up to `GATHER_DEADLINE` (3 s). The 100 ms came from the claims plan's
review ("a 100 ms `SessionStart` test"), and stood for "a busy lock
does not cost the writer's `LOCK_WAIT` (2 s)". So, as this ticket allows,
the bound is wider.

**Fix:** the two tests assert `BUSY_LOCK_BOUND` (1 s): far above the
grace, under `LOCK_WAIT`, with a compile-time check of that order. The
test names say "returns before the lock wait". The grace's own limit
moved out of wall-clock time: `ONCE_GRACE < 100 ms` is a `const` assert
in `claim::store`. The test cannot hold the writes to a time bound, so
none is claimed for them.

**Loop:** both tests (and their sibling) run 200 times, each with
`--test-threads 2`, beside 12 `/usr/bin/true` spawn loops and 4
`dd`+`sync`+mkdir/rm loops on this 6-core Mac. Before: 7/200 and 4/200 red,
all on the SessionEnd 100 ms bound. CPU burners alone (24 busy loops) gave
0/200. After: 0/200.

**Red proven:** with the pass given `LOCK_WAIT` instead of `ONCE_GRACE`,
both tests fail at 2.001 s. With `ONCE_GRACE` set to 100 ms, the crate does
not compile (E0080, "a hook's lock wait stays well under 100 ms").
