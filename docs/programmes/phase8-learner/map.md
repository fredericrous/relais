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
- D37 (person, 2026-11-23): an agent whose definition declares
  `preferred-model:` is pinned by relais (`router/pins.rs`), so its spawns
  are never routed and add no draws: the first-party reviewers leave the
  review-subagent strata (other review-kind subagents still feed them).
  Their usage is booked to the parent task, which is marked `maybeWrote`,
  so a parent with reviews can label `unknown`. Plan
  `docs/plans/2026-11-23-router-keeps-preferred-model.md`.

From this programme's research tickets:

- [t01](t01-gate-inputs.md): neither review stratum can activate. Gates 2
  and 4 fail on both. Gate 1 hangs on what "unknown" means, and gate 3 on
  whether cost is read per session or per task (both go to t03).
- [t02](t02-missed-failure.md): a review's miss is measured on its parent's
  files and end. Today the measure reads 0, but the revert channel and
  cross-session matches are blind. Four plugin fixes come before gate 4
  can mean anything. Whether to count parents goes to t03.
- [t06](t06-stop-rows-after-release.md): no stop row under v0.77.0.
  The pooled gap is −1.0 point against 15, so the label-policy
  `holds-until:` has not fired and the `label_policy_version` column is
  not owed yet.
- [t09](t09-release-check.md): v0.77.0 shipped (relais#537), and the
  installed binary reads 0.77.0. The plugin is still at 0.72.0 (the
  person's reinstall).
- [t04](t04-activations-ledger.md): `router_activations` is one row per
  activation, rollback columns included, with one active row per stratum
  enforced by an index. `router_decisions` gains `activation_id`, which the
  plugin must echo back. Dedup is by evidence fingerprint plus 10 new drawn
  tasks after a rollback. The command is `router rollback <stratum>`, which
  prints by default and acts on `--write`. The stop-row column is added in
  the same ledger step (it releases the `holds-until:` early).
- [t10](t10-revert-paths-resolved.md): a revert's files are now resolved
  against the command's own directory and hashed as edits are. `$` words
  and unknown directories are skipped. `.` expands to the task's files.
  `git revert` reads its files from `git show`. Only a revert the call's
  success proves is recorded. The live count is t13, after a release and
  the plugin reinstall.
- [t11](t11-file-hash-repo-scope.md): a file hash is sha256(git common dir,
  NUL, path from the work tree root), so a worktree and its checkout agree
  and two repositories never collide; paths in no repository are not
  recorded. Older rows keep the cwd-relative hash (the t02 stop list stays
  for them).
- [t12](t12-correction-attribution.md): a correction row carries the files
  edited after it, re-sent at the task's end and merged by union. The
  charge is decided at read time: an earlier completed main task within
  24 h sharing a file is charged, and a B whose every correction is
  charged (and never flagged) counts with its `outcome_uncorrected`.
- [The correction charge read rule's fallbacks are tested and visible](t14-charge-read-fallbacks.md):
  a correction row reads as one `ChargeRead`: `Charged`, `Legacy` (files
  that parse but are empty) or `Unparsed`. Candidates skipped for damage
  are counted in `CorrectionCharges::skipped_candidates`.

## Handoff

2026-11-23, t14 closed (the charge read rule's fallbacks are tested and
visible). The read-path change is in the PR that closes this ticket, with
its plan and panel; t14's Resolution has the rules and the panel's notes
for t05. It needs no release of its own: nothing outside tests reads
`CorrectionCharge` until t05, so it ships with the next release. The live
state is unchanged from the t12 entry below: v0.80.0 installed, ledger
v50, and the plugin still 0.79.0 until the person reinstalls it. Next: t03.
Earlier entry:

2026-11-23, t12 closed (a correction records its files and charges one
task). Plugin and ledger changes are on
`prog/phase8-learner/t12-correction-attribution` with plan and panel, in the
PR that closes this ticket (relais#550, merged); t12's Resolution has the
rules. t10 and t11 shipped in v0.78.0 and t12 in v0.80.0 (its ledger step is
v50: #546 took v49). The person reinstalls the plugin (`relais install --claude --user
--write`, then `relais coordinator stop`; check `relais doctor`'s `plugin`
finding names the release). [t13](t13-revert-overlap-live-count.md) (due
2026-12-01) counts reverts, the t02 miss query, and now the correction
rows with files and their charges. The host was jammed again: both relais
runs lost their verify to the 1800 s admission wait, and their candidates
were applied and checked in-session; `greengate run --raw` exited 0
without running the plugin suite, so run `claude plugin test` from
`claude-plugin/` to check it. v0.80.0 is released and verified: the Release
run concluded `success` with 7 assets, the macOS archive reports `relais
0.80.0`, the installed binary is 0.80.0, and the live ledger reads v50. The
plugin is still 0.79.0 until the person reinstalls it. The t12 review's
fallback items are filed as [t14](t14-charge-read-fallbacks.md), to be
settled before t05 reads `CorrectionCharge`. Next: t03. Earlier entry:

2026-11-23, t11 closed (file hashes scoped to a repository). The plugin
fix is on `prog/phase8-learner/t11-file-hash-repo-scope` with its plan and
panel, in the PR that closes this ticket; t11's Resolution has the hash
bytes and the cache rules. It is not released: t10 and t11 wait for t12
(a correction records its files and charges one task), which should hash
its files with `hashFiles` from `claude-plugin/hooks/routing.ts`. Then
release all three; the person reinstalls the plugin (the commands are in
the t11 plan's Your steps). [t13](t13-revert-overlap-live-count.md) (due
2026-12-01) counts the overlap after that and runs the t02 miss query on
the new rows (expected 0 false misses from out-of-repository paths). The
host was jammed today: two relais verify runs waited out their 1800 s
admission budget. Next: t12, then t03. Earlier entry:

2026-11-23, t10 closed (revert paths). The plugin fix is committed on
`fix/revert-paths` with its plan and panel; it is in the PR that closes this
ticket. See t10's Resolution for the rules. It is not released: the hooks
run from the installed plugin (0.72.0), so live revert rows still come from
the old code. Release it with t11 and t12, the two other plugin fixes from
t02, then the person reinstalls the plugin. [t13](t13-revert-overlap-live-count.md)
(due 2026-12-01) counts the overlap after that. Next: t11, then t12, then
t03. Earlier entry:

2026-11-23, t04 closed (activations ledger). The answer is in the ticket:
- the `router_activations` table, with its unique index on the active row;
- `router_decisions.activation_id`, which `router-state` serves and the
  plugin echoes;
- the accessor signatures;
- the fingerprint-plus-floor dedup;
- `router rollback <stratum> [--write]`;
- the stop-row `label_policy_version`, added in the same step.

The live ledger has never held a stop row, so nothing needs a backfill. Two
points go to t03's ADR: the echo of the id, and the rule that `adjustments`
are withheld from a plugin that cannot echo it. Next: t03, which takes t02's
two questions plus the wire points above; t05 waits on it. Then t07
(2026-11-29). Earlier entry:

2026-11-23, t09 closed (release check): v0.77.0 shipped. relais#537 merged
as `50736d58`, and the Release run concluded `success` with 7 assets. The
installed `relais` reads 0.77.0. The plugin is still at 0.72.0 (the
person's reinstall), and its hooks already use the 0.77.0 binary on
`PATH`. Next is still t03, with t02's two questions, then t07
(2026-11-29). Earlier entry:

2026-11-23, t06 closed (stop rows after release): `router_exploration_stops`
holds 0 rows under v0.77.0. Exploration is on (probe ε 0.1). The pooled
gap is −1.0 point against the 15-point margin (drawn 125 at 6.4%, undrawn
1,197 at 7.4%), so the `holds-until:` on the label-policy version has not
fired. Next is still t03, with t02's two questions. Then t09 (release
check, due today) and t07 (2026-11-29). Earlier entry:

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
  `model` preference and the person's pins (session-router plan §6):
  answered for agents declaring `preferred-model` by D37 (pinned, never
  adjusted); open for a parent's `model` on other agents.
- Whether the learner's evaluation runs at session start in `router-state`
  (latency budget) or only on demand and at `relais report`.

## Out of scope

- Replacing the haiku classifier with Laya (D35: no serving path, no cost
  case).
- Pricing `gpt-6.1-sol` or `qwen3.6:35b-a3b` in the binary (prices live in
  machine.toml).
