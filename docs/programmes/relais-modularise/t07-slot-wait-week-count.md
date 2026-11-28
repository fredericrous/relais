+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
due = "2026-12-08"
+++
# A week of relais slot waits through greengate's loop

## Question

t05 moved relais's host-slot wait into greengate's one loop. Seven days
after the relais release that carries t05 (re-date this ticket if that
release is later than 2026-12-01), compare relais's waits with the 7 days
before it:

- the share of relais rows in `waits.jsonl` ending `stalled` or `io_error`;
- from the run events, not `waits.jsonl` (that path writes no row), the
  number of `EnvironmentWarning` events with `what` `host_slots_off` whose
  detail contains "a host slot could not be locked".

Record both before-counts first. If either more than doubles, on at least
3 rows, revert t05's relais commit (`refactor(verify): wait through
greengate's loop`).

## Look at

- `$GREENHARNESS_SLOT_DIR` or `/tmp/greenharness-slots-<uid>/waits.jsonl`,
  rows with `"tool":"relais"`.
- The relais run directories' `events.jsonl`.
- [t05](t05-wait-loop-cancel-seam.md)'s Resolution.

## Answer looks like

Both counts before and after the release, the ratio of each, and keep or
revert.
