# Full reviews: relais learns from what it routes (2026-10-11)

Sidecar of `2026-10-11-learning-faucet.md`.


### backend, round 1 — approve-with-changes (52k tokens, 76 s)

1. [high, accepted] The three known replays predate ledger v17 (empty `arm_run_id`), so Phase 2's rule excludes them; Verification and Measure now expect 3 restored sources and 3 replays excluded by name, with a test.
2. [medium, accepted] A drawn spawn could land on an excluded model: the draw is skipped, no propensity recorded, with a test.
3. [medium, accepted] The strata join must carry no `agent_id` condition on either side (an unkeyed spawn keeps `''`); applied by hand.
4. [medium, accepted] The live check starts at the plugin reinstall, counts draw-eligible spawns N and explored E over 24 h, expects E ≈ 0.1·N, extends to 72 h below N = 30.

### lang:rust, round 1 — approve-with-changes (46k tokens, 45 s)

1. [high, rejected: duplicate of backend 1].
2. [medium, accepted] A source and its replay share features, so `measure` would count a replayed task twice in abstention; count tasks, not records, with a test.
3. [low, accepted] Build the floor grid from integer hundredths, deduplicate on rounded hundredths, sort with `f64::total_cmp`.

### tui, round 1 — approve-with-changes (35k tokens, 19 s)

1. [medium, accepted] Sweep row width tested with worst-case values; the configured floor named in a `configured` note, not by `*` alone.
2. [medium, accepted] A one-line legend `sub = subagent spawn` under the strata table.
3. [medium, accepted] The gap line split in two, the command on its own line, `cost unknown` never `$0`.
4. [medium, accepted] An artifact without the fields prints `floor sweep: not in this artifact; run relais learn cycle`; `--json` emits `null`, never `[]`.
5. [low, accepted] `no projection: N of 20 drawn tasks` per stratum; patch written in the session.
6. [low, rejected: duplicate of unix 4] Plain ASCII, no colour.

### unix, round 1 — approve-with-changes (35k tokens, 22 s)

1. [medium, accepted] Dataset and artifact writes are temp-file-and-rename; an unreadable artifact prints `floor sweep: unavailable (reason)`, exit 0, `null` in `--json`.
2. [medium, accepted] What the installed 0.15.0 does with a version-6 dataset is checked and stated in the CHANGELOG.
3. [low, accepted] New `--json` keys are additive, listed in the CHANGELOG, `null` when unknown.
4. [low, accepted] Report lines on stdout, no colour when piped or under `NO_COLOR`, diagnostics on stderr; applied by hand (overlapped tui 4).

### backend, round 2 (delta) — approve-with-changes (47k tokens, 39 s)

Round-1 findings 1–4: resolved.
1. [high, accepted, applied] Counting tasks changes the gate's unit (`test_records`, `supported_test_records`, `abstention_rate`); say so, numerically identical without replay examples, and correct the "changes nothing the gate measures" sentence.
2. [medium, carried into implementation] One text for a missing or unreadable artifact (`unavailable (reason)`), another for an artifact written before the fields (`not in this artifact`); the tests must not expect both for one case.
3. [medium, carried into implementation] The Measure query must use `propensity > 0 AND propensity < 1`, as Verification does, or research spawns (propensity 1) inflate N.
4. [low, carried into implementation] The temp-file-and-rename sentence belongs with `dataset build` and the registry (Phase 2), not in the `learning_report` loader sentence.
5. [low, carried into implementation] Read 0.15.0's `dataset.rs` and write down its single expected behaviour on a version-6 dataset instead of "skips or fails".

### backend, binding run on the final body — approve (34k tokens, 11 s)

Round-2 finding 1: resolved.
1. [low, carried into implementation] The release-commit CHANGELOG list must also name the gate's unit change.
