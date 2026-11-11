# Reviews: A repair gets a guaranteed wall time


**Round 1** (all opus-5-5):
- **backend**, approve-with-changes (44k tokens, 63 s):
  - #1 high: the refusal detail cannot go through `end_on_exhausted`. Accepted, merged.
  - #2 medium: the extension came before the spend ceilings. Accepted.
  - #3 medium: no cost numbers. Accepted.
  - #4 medium: the refusal check had no total. Accepted.
- **tui**, approve-with-changes (33k, 49 s):
  - #1 high: `explain` prints only `detail`. Accepted.
  - #2 high: the refusal blamed work. Accepted, merged.
  - #3 medium: name the way out. Accepted.
- **unix**, approve-with-changes (41k, 60 s):
  - #1 high: ledger writes shave the floor. Accepted.
  - #2 low: the refusal was recorded as WallClock. Accepted, merged.
- **lang:rust**, approve-with-changes (47k, 79 s):
  - #1 high: the `wall_seconds` test and the 60 s claim. Accepted.
  - #2 high: the extension was computed too early. Accepted.
  - #3 medium: real waits in tests. Accepted.
  - #4 medium: verification of an extended repair is clipped. Accepted.
  - #5 low: unsafe i64 conversion. Accepted.

**Round 2:**
- **tui**, approve (39k, 20 s). Carried:
  - name the key for the attempt's kind (`min_escalation_seconds` for an escalation);
  - the refusal check also expects `limits.total_seconds` in `explain`.
- **unix**, approve (48k, 28 s). Note: report the wall time actually applied at launch.
- **backend**, approve-with-changes (60k, 51 s). Carried:
  - #5 medium: `exhausted()` returns `Work` before it checks the wait budgets. Use `exhausted_except_work()` with its guards, plus a blocked test.
  - #6 medium: seat unix in the code review.
  - #7 low: the Picture's wording.
  - #8 low: give the clip test a value.
  - #9 low: if the escalation rung is fable, the cost bound is about $83.
- **lang:rust**, approve-with-changes (57k, 51 s):
  - #2 high: the extension came after the transition meant to record it. Applied.
  - Carried, low: say which tests use the scripted clock; put the verification test in Phase 3's list.

**Re-run on the blocker edit:**
- **lang:rust**, approve (59k, 15 s). Note: total time can dip a few ms between the check and the launch.
- **backend**, approve (63k, 19 s). Carried, low: round `work_extended_s` up to whole seconds. Note: a failed transition write leaves only an in-memory extension, which is harmless.

**codex:** ⚠ not run. The CLI exited 1 on its usage limit, which resets on 2026-11-11 at 23:43; a relaunch would hit the same limit.

**Ledger:** recording round 2 failed. The record command does not accept the "carried" disposition, and the round should have been recorded before the edit. Round 1 is recorded.

