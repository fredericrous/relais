+++
type = "task"
mode = "afk"
status = "closed"
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

## Resolution

2026-11-23, plan `docs/plans/2026-11-23-correction-attribution.md`. The
plan's rule is kept; the charge is decided when the ledger is read.

- **Files on the row.** The plugin keeps one entry per correction (the
  last 8) with the files edited after it, the task's earlier edits
  excluded. When the task ends, and at each later correction, it sends the
  same reassess row again (same `at`, so the same key) with those files
  hashed by `hashFiles`; relais stores the union of every send. Rows from
  before the release keep `[]`.
- **Which task is charged.** `Ledger::router_correction_charges(stop)`
  charges a correction row to each main task A, in any session, that ended
  `completed_verified` or `completed_accepted` in `[at − 24 h, at)` and
  shares a hash with the row outside `stop`. `corrections_charged_away`
  returns the tasks B whose every correction row is charged and that hold
  no `flag` row. For gate 2 such a B, and its parent-sourced subagent
  tasks, count with B's `outcome_uncorrected`: the outcome the plugin
  computed without the correction, on ended records only, first value
  kept (ledger step v49). A flag is never charged away.
- **Wire.** `outcome_uncorrected` is gated on router-state's `accepts`, as
  `answered` is: an older relais refuses unknown fields.

Plan §5 of `docs/plans/2026-10-07-session-router.md` and
`docs/router-protocol.md` say so. Gates 2 and 4 (t03/t05) call the read
rule; t13 counts the live rows after the release and the plugin reinstall.
