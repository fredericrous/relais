+++
type = "task"
mode = "afk"
status = "open"
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
