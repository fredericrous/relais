# relais learns which model a stratum needs

## Destination

Phase 8 of `docs/plans/2026-10-10-learning-loop.md` is live: relais reads
the exploration evidence per stratum (relation × kind × difficulty band ×
scope), evaluates the four activation gates of
`docs/plans/2026-10-07-session-router.md` §5 for each, serves a learned
tier adjustment through `router-state` for every stratum that passes all
four, and rolls it back on the early trigger. `relais router evaluate
[--json]` shows every stratum and why it passes or fails; `relais router
rollback` undoes an activation; `router_activations` records both. The
first strata to pass are the review-subagent ones (30 and 24 drawn on
2026-11-21), and the person can read, in `relais report`, what each
activation saved per completed task.

## Notes

- **Origin:** plan `docs/plans/2026-11-21-learning-loop-phase-8-entry.md`
  (D34–D36): the gate's start condition was met on 2026-11-21 on
  `sub review d3 module` (30 drawn) and `sub review d4-5 cross-cutting`
  (24 drawn); a question's own `answered` now counts as evidence
  (`ROUTER_LABEL_POLICY_VERSION` 2); the report excludes unpriced sessions
  per arm. Nothing of the learner exists in source (`router/mod.rs`: "R1 has
  no learner: `adjustments` is always empty").
- **What exists:** `router::explore` (strata, stop rule, projection),
  `router::stats` (`wilson`, `clopper_pearson_upper`, `median`,
  `cluster_bootstrap_ratio`), `router::report` (per-arm cost per completed
  task over priced sessions), `router-state`'s empty `adjustments`
  (`router/state.rs`), the plugin's state parser and exploration draw
  (`claude-plugin/hooks/router.ts`), `cli/mod.rs` `RouterCommand` with
  `export-classifier` only.
- **Standing constraints:** the savings gate is inconclusive while either
  arm holds an unpriced session (pricing in machine.toml is the person's
  step); a stop row written under label policy 2 owes the
  `label_policy_version` column (`holds-until:` on the constant); relais's
  own workers are never routed (session-router plan §6); a Laya backend
  needs a relais ADR amending SPEC §16; exploration ε stays 0.1 unless a
  ticket here changes it on data; whether automatic activation is on is the
  person's switch, never the agent's.
- **Skills every session reads:** `worktree-task`, `relais`, this
  programme; a plan built for a ticket here goes through plan mode and
  `/plan-review`.

## Decisions so far

(none yet)

## Handoff

2026-11-22, charted (the entry plan's Phase 4). Merged: relais#533 (the
entry plan: label policy 2, honest cost headline, D34–D36); release
v0.77.0 in flight at charting time. Live state: two review-subagent strata
past 20 drawn, `main question d1-2 unknown` at 12 of 20 (projected
2026-12-15). Next, in order: **What each activation gate needs that the
ledger does not yet hold** (research, afk), **How a missed failure is
detected for a drawn task** (research, afk), then the grilling tickets on
the adjustment's shape and the decision record. The research runs were not
started at charting: the host was saturated (two relais admission waits
exhausted that night), so the next session starts them. Dated checks: the
stop-row count on 2026-11-23 and the question stratum's corrected rate on
2026-11-29. The person's step: price the two foreign models in
machine.toml (its own ticket).

## Not yet specified

- How Laya's second job (export of input, drawn tier, outcome, total cost;
  "does this tier succeed") is compared with the strata learner under the
  same gates, and whether it is worth running at all after D35.
- Whether an activated stratum changes the exploration draw inside it
  (explore around the new tier, or freeze it), and whether ε moves.
- How an adjustment on a subagent stratum interacts with the parent's
  `model` preference and the person's pins (session-router plan §6).
- Whether the learner's evaluation runs at session start in `router-state`
  (latency budget) or only on demand and at `relais report`.

## Out of scope

- Replacing the haiku classifier with Laya (D35: no serving path, no cost
  case).
- Pricing `gpt-6.1-sol` or `qwen3.6:35b-a3b` in the binary (prices live in
  machine.toml).
