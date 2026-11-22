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
review-subagent strata (30 and 24 drawn on 2026-11-21) are the expected
first candidates; whether they pass gate 3 is open (the gate-inputs and
pricing tickets). The person can read, in `relais report`, what each
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
- **Builds follow decisions:** `relais router evaluate` (read-only first
  slice over the two ready strata), `router rollback`, the served
  `adjustments` and the plugin's application are plans written after the
  tickets on the adjustment's shape, the activations ledger and the
  decision record close; no ticket here is a deliverable.
- **Skills every session reads:** `worktree-task`, `relais`, this
  programme; a plan built for a ticket here goes through plan mode and
  `/plan-review`.

## Decisions so far

Inherited from `docs/plans/2026-11-21-learning-loop-phase-8-entry.md`
(person, 2026-11-21), recorded in `docs/plans/2026-10-10-learning-loop.md`:

- D34: a question's own `answered` (source `own`, never backfilled) is a
  positive strong outcome; `ROUTER_LABEL_POLICY_VERSION` 2.
- D35: Phase 7 closed, no run 3; Laya replacing haiku dropped as a cost
  project; Laya's second job stays a research comparison.
- D36: Phase 8's start condition met on the review-subagent strata;
  charted as this programme; ε stays 0.1. Watch item: `main question d1-2
  unknown` drawn corrected share (40% of 5 on 11-21).

From this programme's research tickets:

- [t01](t01-gate-inputs.md): neither review stratum can activate. Gates 2
  and 4 fail on both. Gate 1 hangs on what "unknown" means, and gate 3 on
  whether cost is read per session or per task (both go to t03).
- [t02](t02-missed-failure.md): a review's miss is measured on its parent's
  files and end. Today the measure reads 0, but the revert channel and
  cross-session matches are blind. Four plugin fixes come before gate 4
  can mean anything. Whether to count parents goes to t03.

## Handoff

2026-11-22, t02 closed (missed failure). A review's miss is measured on its
parent: in the 24 h after the parent ends, a correction or a revert that
touches the parent's files. The SQL is in the ticket. Result: 0 misses on
both strata once a stop list masks the memory files. Without it the query
finds 45 false misses: absolute paths outside the session cwd collide
across sessions. Gate 4 still fails on sample size: 24 and 14 linked drawn
reviews, from 19 and 11 distinct parents. Reviews cluster on parents, so
the bound should count parents.

What the measure cannot see (blind channels):
- **Reverts:** none of the 93 recorded reverts can match any task's files
  (a literal `$P`, paths resolved against the session cwd instead of the
  `cd` target, `.`).
- **Repository:** hashes carry no repository key.
- **Correction files:** correction rows carry no files.
- **Plan vs hook:** the hook marks B `corrected` where the plan says A's
  miss only.
- **Runs:** `learn::revert` runs have no task join.

Before gate 4 can mean anything, the plugin fixes ship in this order, now
filed as tickets:
1. **Recorded reverts name the files they revert** (t10): resolve revert
   paths.
2. **Task file hashes are scoped to one repository** (t11): drop
   out-of-repository paths and key hashes by repository.
3. **A correction records its files and charges one task** (t12).

Next: t03, which now also takes two questions:
- whether gate 4 counts parents for review strata;
- whether a parent's miss is a review's miss at all.

Earlier entry:

2026-11-22, t01 closed (gate inputs). Neither review stratum can activate:
gates 2 and 4 fail on both (best 5% bound 0.848 on d4-5; 27 and 23
completed drawn, short of 36), gate 1 passes only if "unknown" means the
literal outcome (a review's own `answered` makes 67% and 62% weak), gate 3
is inconclusive under the session rule (one `gpt-6.1-sol` session holds 1
drawn and 50 undrawn strata tasks) though every strata task's own usage is
priced. Findings for the next tickets: every strong outcome on these
strata is inherited from the parent (`outcome_source = parent`), reviews
edit no files (file-overlap misses are vacuous), and `/relais-flag`,
though wired, has never been used: no `flag` reassess event has ever
been written (t02); which "unknown" gate 1 counts and
whether the cost gate reads sessions or tasks (t03). Next: t02 (missed
failure), then t03. Earlier entry:

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
2026-11-29, and **Confirm v0.77.0 shipped and is installed** on
2026-11-23. The person's step: **Price the two foreign models so their
sessions rejoin the headline** (hitl).

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
- How `router rollback` names the activation it undoes, and whether it
  takes `--dry-run` or `--yes` (the activations-ledger ticket reads this).

## Out of scope

- Replacing the haiku classifier with Laya (D35: no serving path, no cost
  case).
- Pricing `gpt-6.1-sol` or `qwen3.6:35b-a3b` in the binary (prices live in
  machine.toml).
