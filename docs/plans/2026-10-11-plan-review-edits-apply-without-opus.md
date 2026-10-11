---
status: done
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
- [x] Phase 3 — the ingest, the ledger writer, the report line, tests with a
      fixture ledger holding review, command-merge and subagent-merge rows:
      the subagent row updates the matching `router_tasks` row once, a second
      ingest changes nothing, a row with no task on record is counted.

## Verification
- `cargo test -p relais review_outcome`, and a piloted
  `relais native review-outcome --ledger <fixture> --dry-run` printing the
  merge counts → observed 2026-10-11: `cargo test -p relais --test
  review_outcome` 13 passed (new: `a_subagent_merge_row_settles_its_router_task_once`,
  `a_merge_row_with_unknown_fields_or_another_schema_is_invalid`); `--lib`
  suite green; clippy `-D warnings` clean; `exit_codes_follow_the_documented_table`
  passes with no new codes. Pilot on a scratch ledger holding one `command` and
  one `subagent` row: `--dry-run` printed `labelled 0: clean 0, miss 0, unknown 0;
  merge: 1 applied by command, 1 by subagent (1 verified)`, exit 0, and created
  no state.

## Decision log
- 2026-10-11 — pointer plan: the decision and the reviews live in amont-agent.
- 2026-10-11 — the amont gate does not run `scripts/check-module-cycles.py` while `make lint` and CI do (`ci.mirrors-the-local-check`); adding it to `amont.conf` needs the person's `amont trust`, so it is left for them.
- 2026-10-11 — the report's merge line counts only what the relais ledger holds
  (router tasks with `outcome_source = 'merge'`): a `command` merge leaves no
  task and no row here, so "N applied by command" is printed by the ingest's
  summary, not by `relais report`. A table for it would be a ledger step for a
  count the amont-agent ledger already has.
- 2026-10-11 — relais itself could not run this phase (`run blocked
  (operational_failure)`: its ledger's `decisions` table holds four rows whose
  actor `claude (approved plan 2026-10-11-session-scoped-status)` schema v27
  refuses to read); done in the session.

## Implementation review

- Round 1 `approve` (32k, 21 s), Delta `approve` (35k, 9 s) after CI's module-cycle check failed: `record_merge_outcome` takes the outcome's name and rank so the ledger does not import the router; relais Phase 3 only.
- deliberate: the EMPTY sentence stays above a merge line when no class exists yet — it is true, no reviewer outcome has been labelled.
- deliberate: a merge row whose task is already settled is a silent no-op — the `WHERE outcome = 'unknown'` guard is the rule, not an omission.
- deliberate: `#[allow(dead_code)]` with a doc comment rather than `#[expect(reason)]` — cosmetic, not worth a code change after the review.
- Would still check by hand (reviewer): a real `--record-fallback` flow's agent id against `router_tasks.agent_id` (Phase 4 observes it).
