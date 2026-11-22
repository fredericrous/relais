+++
type = "research"
mode = "afk"
status = "open"
blocked_by = []
+++
# How a missed failure is detected for a drawn task

## Question

The session-router plan defines a missed failure as a later correction that
edits the same files within 24 h, or a revert, on a task the router drew
down. The ledger records `corrected` on the task itself, `files_json`,
`parent_task_id` and the run-level revert detection (`learn::revert`). What
query, over which tables, yields a per-stratum missed-failure count and the
denominator the Clopper–Pearson upper bound needs, and what is undetectable
(an `answered` question has no files; a correction after the next task
starts never reaches the earlier task)?

## Look at

- `docs/plans/2026-10-07-session-router.md` lines 170–180 (the measure)
  and §5 gate 4.
- `crates/relais/src/ledger/router.rs` (`files_json`, `completed_at_end`,
  `outcome_source`, the rank rule), `crates/relais/src/learn/revert.rs`,
  `claude-plugin/hooks/router.ts` (`outcomeOf`, correction detection,
  `startTask`).
- `docs/router-protocol.md` (the outcome enums and `completed_at_end`).

## Answer looks like

The SQL (or the Rust accessor to add) producing, per stratum, drawn tasks,
drawn tasks with a missed failure, and the same for undrawn; a list of the
cases it cannot see, each with why; and the figure for the two ready review
strata on a `.backup` copy.
