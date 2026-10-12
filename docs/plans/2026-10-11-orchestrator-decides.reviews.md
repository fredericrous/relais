

## Amendment 3: the orchestrator reports a no (2026-10-12)

- **Amendment 3 (orchestrator feedback), backend round 1** (61k, 33 s) approve-with-changes: high, `feedback` has no `--json` (added); high, `source = orchestrator` only with the plugin's nonce, else `cli`, and the Bash reminder covers `relais feedback`; medium, input takes `--task` and `--magnitude`; medium, one label per task; medium, Measure coverage lines and a live check; low, ordering on the in-flight run. 3 patches applied.
- **round 2** (66k, 54 s): high, the "latest outcome" block would stop a detected row whenever the orchestrator reported first (agreement always 0); fixed so only a contradicting outcome blocks; low, how a `cli` reverted row labels (a no, out of agreement).
- **blocker re-check** (72k, 14 s): high, the fix removed the second-build no-op; fixed with a `detected` row per task and undo sha blocking re-insert, plus its fixture.
- **final** (73k, 47 s) approve. Carried into implementation: store the undo sha under a fixed `detail_json` key (`undo_sha`) so the dedupe matches on it; the in-flight revert-detection run was given the pre-amendment blocking rule, so its candidate is adjusted to this one.
- **fresh binding** (57k, 71 s) approve-with-changes, no blocker. Carried into implementation: agreement (tasks with both sources over tasks with either) and coverage (share of detected reverts the orchestrator also reported within 7 days) defined and both tested, the fixture expecting agreement 1 of 2 and coverage 1 of 1; an orchestrator-reported revert outranks containment like a detected one; guard tests for `relais feedback`; `feedback --actor` takes the `Actor` type; the ordering note moves to Delivery and the header's next names the feedback tool.
