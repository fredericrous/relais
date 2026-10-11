---
status: active
branch: feat/merge-outcome
repos: [amont-agent, relais, dotfiles]
canonical: amont-agent:docs/plans/2026-10-11-plan-review-edits-apply-without-opus.md
phases: [3]
adrs: []
---
# Plan-review edits apply without an Opus subagent (relais slice)

Pointer to the plan's home in amont-agent (its PR #98). This repository
implements **Phase 3** only: `relais native review-outcome` ingests
`amont-agent-review-merge-v1` rows whose `mode` is `subagent` and that carry
an `agent_id` as task outcomes.

## Behaviour (Phase 3)

- A `mode: "subagent"` merge row joins the router task through
  `Ledger::router_task_for_agent(agent_id)`; `verified: true` records
  `completed_verified`, `false` records `corrected`, through a new
  `Ledger::record_merge_outcome(task_id, agent_id, outcome, rank)`:
  `UPDATE router_tasks SET outcome=?, outcome_source='merge' WHERE task_id=?
  AND agent_id=? AND outcome='unknown' AND outcome_rank < ?`, ranks from
  `router/outcome.rs`. The `router-observe` UPDATE is not reused: it keys on
  the session and hardcodes `outcome_source='parent'`.
- Rows with `mode: "command"` (no agent id) are counted and skipped; unknown
  schemas stay skipped as today; a subagent row whose agent id names no task
  on record is counted as unmatched.
- The report line gains `merge: N applied by command, M by subagent
  (V verified)`, within 80 columns and in `relais report --json`.
- Exit codes unchanged; README table unchanged.

## Phases
- [ ] Phase 3 — the ingest, the ledger writer, the report line, tests with a
      fixture ledger holding review, command-merge and subagent-merge rows:
      the subagent row updates the matching `router_tasks` row once, a second
      ingest changes nothing, a row with no task on record is counted.

## Verification
- `cargo test -p relais review_outcome`, and a piloted
  `relais native review-outcome --ledger <fixture> --dry-run` printing the
  merge counts → observed.

## Decision log
- 2026-10-11 — pointer plan: the decision and the reviews live in amont-agent.
