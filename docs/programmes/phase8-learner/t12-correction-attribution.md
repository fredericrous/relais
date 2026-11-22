+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# A correction records its files and charges one task

## Question

Two gaps in correction handling (t02, "cannot see" 5–6):
- **No files on the row.** A `correction` reassess row carries
  `files_json = '[]'`, so the miss overlap uses everything task B ever
  edited, files from before the correction included.
- **Double count.** The plan (§5) says a correction inside open task B on
  an earlier task A's files is A's miss only. The hook marks B
  `corrected` regardless, so gate 2 counts it too.

Record the files edited after the correction, and settle which task is
charged.

## Look at

- `claude-plugin/hooks/router.ts`, the `isCorrection` branch and
  `escalate`.
- The plan, `docs/plans/2026-10-07-session-router.md`, §5 "Missed
  failures".
- `crates/relais/src/ledger/router.rs`, `router_reassess` writes.

## Answer looks like

A correction's row, or a later update of it, carries the hashed files
edited between the correction and the next task start. B stays
`corrected` only when none of those files belong to an earlier completed
task inside the 24 h window; otherwise only A is charged. Tests cover
both cases. If the plan's rule is changed instead, the plan says so.
