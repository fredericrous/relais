# Reviews: fresh outcome notifications

## Full reviews (reference)

**plan-review-backend, round 1: approve-with-changes (45k tokens, 56 s).** (1, high) The stale instruction lives in the headline's `detail`, and there is no `NEXT_STEP` entry for `verification_inputs_changed`: rebuild the headline. (2, high) Deleting the entry in `freshen` lets a retried submit go out stale: delete it after `shift`. (3) Re-adding to `ownPrompts` leaks a mark. (4) `Ledger::open` writes and migrates: open it read-only. (5) Measure latency. (6) Add a production note. (7) Template for a decision-less run.

**plan-review-language (rust), round 1: approve-with-changes (49k, 55 s).** Same retry and `ownPrompts` findings as backend; `Ledger::open` creates a missing ledger, so the planned test passed for the wrong reason. Test 1 must put the stale outcome second in the queue, because the harness runs hooks before the hold. Gate on `pendingOutcomes.has(text)`, not on origin.

**plan-review-tui, round 1: approve-with-changes (39k, 56 s).** Retry finding; test 1 ordering; a 10 s timeout on the path to the next turn should be about 1–2 s; `null` decision template; drop the `ownPrompts` re-add.

**plan-review-unix, round 1: approve-with-changes (38k, 45 s).** Add `Ledger::open_read_only` with no directory creation, pragma or migrate; a short busy timeout and a separate freshen timeout; rewrite only on a non-null resolution, and test that the state strings are equal; stay silent on an unknown run and print stderr only for open/read errors.

**plan-review-backend, round 2: approve-with-changes (37k, 59 s).** All 7 round-1 findings resolved. New: (high) under WAL, `BEGIN IMMEDIATE` does not block readers, so the busy test had to change; add a test for a populated WAL ledger with writers closed; no empty parenthesis when `code` is null; note version skew once per session.

**plan-review-backend, final body: approve (28k, 22 s).** All round-2 findings resolved. Two low test refinements, carried into implementation without another body edit:
- test 3 queues two person-awaiting outcomes that lack `current` and expects exactly one pane note;
- the populated-WAL test asserts `current` is present and the database file is unchanged, not that no `-wal`/`-shm` files appear (SQLite may create them for a read-only reader).
