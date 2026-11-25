---
status: active
branch: prog/agents-many-nodes/an-t25-replay-one-shared-state
repos: [relais, homelab]
adrs: []
plan-id: p-01M8A6003RG3Z5K7NXKB6K2P4B
---
# One SharedState per relais replay

## Review panel

👉 **Decide:** none — approve if building the state once at the top of the replay (bounds, settings, prices, run) closes an-t25.
📍 relais, homelab · 2 phases · next: Phase 1. Panel: backend, lang:rust, tui, unix (opus); codex: not run (programme ticket agent).
**Changed by review:** a fourth build found (`tuning_bounds`, now `tuning_bounds_of`); verification greps that tell main from branch; reasons for omitted profiles.
**Verdicts:** final: backend, lang:rust approve-with-changes (medium/low only); tui, unix approve; 4 carried (pilot runs both modes and proves the build was reached; picture lists `tuning_bounds_of`); words 267 -> 391 (x1.46, fixes a wrong statement); remove 0, merge 0.
📄 Full reviews: [2026-11-25-replay-one-shared-state.reviews.md](2026-11-25-replay-one-shared-state.reviews.md)

## Picture

```
 dataset_replay_command
   shared = ctx::shared_state()      <- the one construction, first
     |
     +--> ctx::tuning_bounds_of(shared.ok())  (was tuning_bounds: 1st build)
     +--> ctx::machine_of(&*shared)       (was load_machine: 2nd build)
     +--> (the old shared_state() here, 3rd build: now the one above)
     +--> ctx::price_table_of(&*shared)   (was load_price_table: 4th build)
     +--> RunConfig { shared_state: &*shared, .. }
```

`dataset_replay_command` builds the state once and hands that one instance to every step that reads through it.

## Goal

an-t25 of agents-many-nodes. `relais dataset replay` builds four
`SharedState` instances: `tuning_bounds()` (replay.rs:93), `load_machine()`,
`shared_state()` and `load_price_table()`. After this change `ctx::shared_state()` is called once
per replay and the machine settings, the prices and the `RunConfig` all use
that instance, as `relais run` does since an-t22. `LocalState` holds no
state, so nothing changes for one node.

## Phases

- [x] Phase 1 (relais): `crates/relais/src/cli/replay.rs`, `crates/relais/src/cli/ctx.rs`, `CHANGELOG.md`. Build the state once at replay.rs:93, before `tuning_bounds`, and keep the `Result`; add `ctx::tuning_bounds_of(Option<&dyn SharedState>, &RepoPolicy)` (the existing `tuning_bounds` stays as a wrapper for `recipe.rs`) and pass it the state when it built, so the bounds still get no error, as today; apply `?` where line 304 is now, so the failure order and the dry-run return are unchanged; replace `load_machine()` with `ctx::machine_of(&*shared)` and `load_price_table()` with `ctx::price_table_of(&*shared)` (both exist from an-t22); drop the two unused imports. Error labels are unchanged: the helpers carry them. One CHANGELOG sentence.
- [ ] Phase 2 (homelab): `docs/programmes/agents-many-nodes/an-t25-replay-one-shared-state.md`, `docs/programmes/agents-many-nodes/map.md`. Resolution, status closed, one Decisions-so-far line.

## Verification

- `grep -nF -e 'load_machine(' -e 'load_price_table(' -e 'tuning_bounds(' crates/relais/src/cli/replay.rs`: main matches lines 93, 302 and 385; the branch matches nothing.
- `grep -cF 'shared_state()' crates/relais/src/cli/replay.rs`: the branch prints 1 (observed on `80cc613c7`); the call is at replay.rs:94, before `tuning_bounds_of` at :95.
- Integration test `a_replay_under_an_absent_external_layer_is_refused_at_its_one_state` (`crates/relais/tests/it/release_scenarios.rs`): under `shared_state = "external"` with no client, `dataset replay --dry-run` still exits 0, and a spending replay exits 3 with the one sentence, no spawn and no worker launch.
- Observed on the merged tree `e4f51dc3` (on v0.87.0 and origin/main 258a5cea3, an-t24): `cargo clippy -p relais --all-targets -- -D warnings` exit 0; `cargo test -p relais` exit 0 (2741, 72, 354, 26, 16, 6, 1 passed, 0 failed), including `a_replay_under_an_absent_external_layer_is_refused_at_its_one_state`. No test tells one build from three (`LocalState` holds no state): the greps are the proof, and `grep -cF 'shared_state()'` prints 1 on the same tree.

## Implementation review

- Round 1 (implementation-review, opus): approve-with-changes; low: Phase 1 box and expectation-only verification, both fixed here; the test passes on main too (it pins the failure order, not the single build), its doc comment now says so.
- code-review-backend (opus): round 1 approve; Delta approve-with-changes, one low: the test's `worktrees/replay` assertion could never fail (replays use `worktrees/<run_id>/task`); removed, the spawn and worker-launch assertions stay.
- code-review-language lang:rust (opus): approve-with-changes, then Delta approve; two low (comment placement, `use` for `shared_state`); both fixed in `80cc613c7`.
- Delta (implementation-review, opus, tree `a441f373`): approve-with-changes; the one finding (a line number and the grep wording) is fixed here. Round-1 findings resolved.

## Non-goals

- Other commands' constructions (`decide`, `programme`, `learn`, `recipe`,
  `report`, `native`).
- No CHANGELOG entry beyond one sentence: nothing changes for an upgrader.
- No trait, `node.rs`, runner or coordinator change.

## Code review

- relais: backend, lang:rust (tui: no pane or terminal output changes; unix: no process, signal or file-mode change)
- homelab: none — markdown only

## Decision log

- 2026-11-25 — the manual pilot (main vs branch binary, sandboxed `machine.toml`) is replaced by integration test `a_replay_under_an_absent_external_layer_is_refused_at_its_one_state`; reason: the trust-grant guard refuses agent writes to `machine.toml`. decided-by: agent (ADR-0080), on the driver's instruction. The dry run exits 0 (it never reaches the build the plan moved), so only the spending replay is pinned at exit 3. The Picture now lists all four builds.
- 2026-11-25 — `docs/shared-state.md` gets one sentence for replay, beside run's.

<!-- panel: repos=relais,homelab reviewers=backend,lang:rust,tui,unix body-sha=32a28ca1a797 -->
