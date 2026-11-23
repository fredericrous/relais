---
status: active
branch: feat/router-preferred-model-pin
repos: [relais]
adrs: []
plan-id: p-01M84EV32AMRF0Z095HHZVJ5BK
---
# The router keeps a reviewer's preferred model

## Review panel
📄 Full reviews: [reference](2026-11-23-router-keeps-preferred-model.reviews.md).
👉 **Decide:** none — approve if reviewer spawns booked to their parent (parents with reviews may label `unknown`; phase 8 loses its first strata) and ×1.58 growth are fine.
📍 relais · 1 phase · 1 release · next: Phase 1. Panel: backend, rust, tui, unix (opus), codex.
**Changed by review:** the row records the preference, not the model run (documented, plugin follow-up); parent bookkeeping named; both post-install checks rewritten to read the ledger and transcripts directly.
**Verdicts:** 4 approve after the delta; 3 carried (80% match floor on the 7-day check, `/clear` also reloads pins, CHANGELOG caveat); words 728 → 1154 (×1.58, person asked for the fixes); remove 0, merge 0.

## Picture

```
 today (routing on)                   after this plan
 ------------------                   ---------------
 spawn relais-review                  spawn relais-review
   model: "opus"                        model: "opus"
     |                                    |
     v                                    v
 router-state pins: none             router-state pins: relais-review=opus
     |                                    |
     v                                    v
 table/explore picks a tier          decision: pin, override=false
     |                                    |
     v                                    v
 runs on Haiku (seen 11-23)          call untouched -> runs on Opus
```

A reviewer spawn used to have its explicit `model: "opus"` replaced by the
router's table choice; after this plan a definition's `preferred-model` is a
pin, so the router leaves the call as the parent made it.

## Context
Reviewer and planner agents say `model: inherit` + `preferred-model: opus` (skills 0.22.0, relais 0.79.0), and greenbelay 6.9.0 holds a launch to `model: "opus"`. But with session routing on (mode `on` on this Mac), the relais plugin's `routeSpawn` replaces any unpinned spawn's `model` with the router's choice (`claude-plugin/hooks/routing.ts:404-406`, `decideSpawn` in `router.ts:1387`: "a model named inside the spawn's own prompt was written by the parent agent: a preference relais overrides, never a pin"). Observed 2026-11-23: a `relais-review` spawn launched with `model: "opus"` ran on Haiku (`router_decisions`: reason `table`, applied 1). Earlier spawns that session got Opus only through a person-named subagent model for that task.

The person decided (2026-11-23): `preferred-model` is a pin. A launch marked `model-fallback:` still falls back to the session model, and agents that declare a preference leave phase 8's candidate strata.

## Approach
One change in Rust; the plugin needs none.

- **`router/pins.rs` `parse_definition`:** also read `preferred-model:`. A definition whose `model` is absent or `inherit` and that declares `preferred-model: X` yields the pin `(name, X)`. A definition with its own non-inherit `model` keeps that as the pin (today's behaviour). Doc comment updated.
- **Why the plugin needs no change:** `decideSpawn` returns a pinned type as `reason: 'pin'`, `override: false`, `isRouted: false` (`router.ts:1396-1403`); `routeSpawn` then passes the call as given (`routing.ts:405-406`) and never classifies it (`:394`). So:
  - `model: "opus"` on the call is kept (Opus runs);
  - a marked fallback launch with no `model` is left untouched and inherits the session model, as `agent-model-fallback` intends;
  - a pinned spawn is never routed, so it adds no exploration draws: reviewers leave the phase 8 strata, which were charted as the learner's first candidates (`sub review d3 module`, 30 drawn; `sub review d4-5 cross-cutting`, 24 drawn). Other review-kind subagents (a general-purpose agent asked to review) still feed them.
  - **Bookkeeping moves to the parent.** A pinned spawn gets no router task of its own: its usage is booked to the parent task, and the parent gets `maybeWrote: true` (`routing.ts:437`), so an otherwise `answered` parent can label `unknown`. Today this touches a few `Explore` spawns; after this plan it touches every reviewer launch (about 520 a fortnight), so main-task labels and `relais report` denominators shift. Accepted by the person (2026-11-23) and written into the phase 8 decision line.
- **The decision row records the preference, not the model run:** `decideSpawn` writes the pin into `decision.model` (`router.ts:1399`) while the call passes unchanged (`routing.ts:406`), so for a `preferred-model` pin the `router_decisions` row and the `router-state` pins say the preferred model even when a marked fallback (no `model`) or an explicit call `model` runs on something else. Stated in SPEC next to the pin sentence; making the row record the call's model is a plugin follow-up, filed to the relais follow-ups. The learner never reads pinned rows, since they are not routed.
- **Pins outrank a person-named subagent model** (`router.ts:1396` before `:1407`), as definition pins already do. Stated in SPEC.

## Phases
### Phase 1: the preference is a pin
- `crates/relais/src/router/pins.rs` (parse + unit tests), `crates/relais/src/router/mod.rs` (a `definition_pins` test with a preference-only definition: the existing test only covers `model: opus`, so this test is required).
- `crates/relais/src/install/mod.rs` tests: `parse_definition("relais-review", &agent_review())` is `Ok(Some(("relais-review", "opus")))`, so the shipped template and the parser cannot drift apart.
- `docs/SPEC.md` §30 pin sentence (line 41) and the pins paragraph: an agent's `preferred-model` is a pin.
- `docs/programmes/phase8-learner/map.md`: a decision line (person, 2026-11-23) that agents declaring `preferred-model` are pinned and leave the strata, and the "Not yet specified" bullet on the parent's `model` preference answered by it.
- `CHANGELOG.md` Unreleased.

## Delivery
Board checked: phase8-learner's t12 runs in `relais-wt-t12-correction` (correction attribution, different files); the map edit is a decision line only. One worktree + PR from `origin/main`, merge on green, release v0.80.0, `brew upgrade relais`. `router-state` is the binary's (`relais native router-state`), so no plugin reinstall is needed; pins are read when a session's router-state is served, so sessions already open keep routing reviewers until they restart.

## Code review
- relais: backend, lang:rust (unix, tui, architect left out: one parser function, its tests and docs)

## Verification
- `cargo test -p relais --lib install::` passes the template-pin test.
- `cargo test -p relais --lib router::`: new tests: inherit + `preferred-model: opus` → pin `opus`; no `model` + preference → pin; `model: sonnet` + preference → `sonnet`; `inherit` alone → no pin; quoted value; `preferred-model:` before `model: inherit` → pin; `preferred-model` before `model: sonnet` → `sonnet` (line order does not matter); `preferred-model: inherit` or empty → no pin. The `Pin` type doc (pins.rs:6-8) and the `parse_definition` doc are both updated. Existing pin tests still pass.
- After install: `relais native router-state --session <id>` lists `relais-review`, `implementation-review` and the `plan-review-*` / `code-review-*` agents under `pins` with `opus`, and its stderr is empty (no new `agent definition … skipped` warning).
- Live, in a session with routing on: launch `relais-review` with `model: "opus"` → `router_decisions` row reason `pin`, applied 0, model `opus` (not `table`), and the subagent's transcript shows Opus; a launch with the `model-fallback:` first line and no `model` → reason `pin`, applied 0, the call untouched; compare that row's `model` with the subagent's step usage model and write the result in the PR (for the fallback launch they may differ, since the row shows the preference and the run uses the session model; for the explicit `model: "opus"` launch they must match, and a mismatch there is a defect).
- After the live launch: read `router_decisions` directly (reason `pin`, model `opus` as the alias the definition wrote; `router.ts:1399` stores it unresolved) and check `relais report`'s pricing against the models in `router_usage` for those agent ids; the report's routing section reads usage models, not decision reasons, so it cannot show pin rows.
- After 7 days: build the reviewer cohort from transcripts (Agent/Task `tool_use` whose `subagent_type` declares `preferred-model`, sessions started after install, the returned `agentId`), correlate by session and agent id with `router_decisions` subagent rows, and report cohort size, matched `pin`, matched `table` and unmatched rows apart. Zero matched `table` is the pass; unmatched rows are not proof.

### Observed (Phase 1)
- `cargo test -p relais --lib -- router:: install::`: 224 passed, incl. the 6 new tests (preference pins an inheriting definition, in any line order and quoted; own `model` outranks it; `inherit`/empty preference is no pin; `definition_pins` serves a preference-only file; the shipped `agent_review()` parses to `relais-review = opus`).
- Pending after release: router-state pins and stderr, the live ledger rows, the 7-day cohort (pass needs ≥80% of the cohort matched, else inconclusive).

## Implementation review
- implementation-review: approve, no findings; 47k, 45 s.
- backend: approve; 1 low to the plugin follow-up (an unchecked pin value only reaches telemetry); 66k, 78 s.
- lang:rust: approve; 2 low left (pin source could be an enum for the follow-up; one `inherit` filter closure); 46k, 36 s.

- After the rebase on 4e43fbb6 (CHANGELOG conflict, both kept): implementation-review approve, 43k, 24 s; lang:rust approve, 40k, 19 s; backend approve-with-changes, 59k, 38 s: deliberate: an unknown `preferred-model` value is not validated, as `model:` pins never were (same follow-up); deliberate: a parent-written model on a pinned call is the documented row-records-the-preference caveat; deliberate: SPEC line 40 "beside `model: inherit`" is short of "or no `model:`", which §30 states in full.

Next: release v0.81.0 (v0.80.0 was cut by another session), `brew upgrade relais`, then the post-install checks.

## Non-goals
No plugin change in this plan (recording the call's model on a pinned row is a filed follow-up); no change to `decideSpawn`, the capability table, ε, or how a person-named subagent model works; no change to relais's own run agents; Codex unaffected.

<!-- panel: repos=relais adds= reviewers=backend,language,tui,unix,codex body-sha=4a6fa7e1ed13 -->
