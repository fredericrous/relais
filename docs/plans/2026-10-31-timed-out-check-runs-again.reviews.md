# Reviews: a check that times out on its own limit runs once more

## Full reviews (reference)

- **backend r1** (30k, 97 s) approve-with-changes. 1 [high] nothing records whether a timeout was clipped: accepted (applied limit and `clipped` on CheckOutcome and Attempt). 2 [high] `timed_out_then_passed` has no field: accepted (`Event::Flaky.reason`, `CheckOutcome.flaky_reason`, SPEC §29). 3 [medium] holdouts and setup: accepted (holdouts in, setup out). 4 [medium] `retry = 1` would give a third run: accepted. 5 [medium] numbers: accepted. 6 [low] production check: accepted.
- **backend r2** (25k, 53 s) approve-with-changes; 1-6 resolved. Carried: 1 [medium] compare the extra time with the run's limits and count each ending kind on the replays (its arithmetic is corrected by codex #3); 2 [low] SPEC wording "only that check, not the suite, and with no worker".
- **codex** (gpt, high effort, exit 0) approve-with-changes. 1 [high] holdout timeouts bypass the budget guard: accepted. Carried: 2 [medium] keep sanitized attempt endings, applied limits, clip flags and the flaky reason on `HoldoutRun`, private output untouched, with a serialization test; 3 [medium] the baseline consumes total, not work (`runner/engine.rs:587, :943`): document it and test that a baseline retry reduces total but not work.
- **backend rebind** (22k, 36 s) approve; the holdout guard bullet resolved. Note: the holdout test should assert which budget ran out.
