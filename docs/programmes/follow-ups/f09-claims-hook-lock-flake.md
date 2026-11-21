+++
type = "task"
mode = "afk"
status = "open"
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
