---
status: done
branch: prog/agents-many-nodes/an-t36-pin-settle-retry-guard
repos: [relais, homelab]
adrs: []
plan-id: p-01M8BFTN1NZ2ZT3C5BNG2FEQP7
---
# Pin the settle retry's first-Unknown guard

## Review panel

👉 **Decide:** none — approve if one fault variant and one test pin the first-`Unknown` guard.
📍 relais, homelab · 2 phases · next: Phase 1. Panel: backend, lang:rust, tui, unix (opus); codex: not run (programme ticket agent).
**Changed by review:** the fault answers without calling `inner.settle` and the test asserts nothing booked; the Verification table gains an actual column; the mutation must compile.
**Verdicts:** round 2: 3 approve, 1 approve-with-changes (backend, then approve); 0 carried; words 326 → 383 (×1.17); remove 0, merge 0.
📄 Full reviews: [2026-11-26-pin-settle-retry-guard.reviews.md](2026-11-26-pin-settle-retry-guard.reviews.md)

## Picture

```
 first settle answer   tries  result today       pinned by
 -------------------   -----  -----------------  ----------------
 Applied               1      Confirmed          existing tests
 Unknown               1      Confirmed (guard)  NEW: no event
 Unavailable, then
   Unknown             2      Unconfirmed        an-t28 test
```

One row had no test: a coordinator that answers the first settle `Unknown`
must stay confirmed; only a retry's `Unknown` is unconfirmed.

## Goal

`settle_with_retry` in `runner/launch.rs` returns `Unconfirmed` for an
`Unknown` only when `tries > 1`. No test pins the first-try case, so a later
change could make every first `Unknown` write `gate_settle_unconfirmed`. Add
the test (an-t36).

## Non-goals

- No change to `launch.rs` or any behaviour.
- No change to the retry count, pause or messages.

## Code review

- relais: backend, lang:rust (unix and tui left out: test only, no output change)
- homelab: none — markdown only

## Behaviour

Phase 1 (relais): in `crates/relais/src/runner/tests/mod.rs` add a `Fault`
variant `SettleUnknownFirst` to `FaultyGate`: the first worker settle is
answered `Ok(LifecycleOutcome::Unknown)` (the coordinator answered; it holds
no such entry) without calling `inner.settle`, later ones pass through. The
variant joins the pass-through arms of `heartbeat` and `release_write`. A test
`a_first_settle_answered_unknown_stays_confirmed` runs it through
`run_through_settle_fault` and asserts: one settle only (no retry), no
`gate_settle_unconfirmed`, no `gate_settle_failed`, nothing booked on the
inner gate (`settled_micros` sum is 0). Proof it bites: remove the
`if tries > 1` guard locally, run the test, see it fail on the `gate_settle_unconfirmed` assertion (it must
compile), restore; the failing output goes in the PR body. CHANGELOG
`## Unreleased` gets a tests-only sentence.

Phase 2 (homelab): the ticket's Resolution (`status = "closed"`) and one line
under Decisions so far in the map.

## Phases

- [x] Phase 1 — the pinning test (relais): `crates/relais/src/runner/tests/mod.rs`, `CHANGELOG.md`
- [x] Phase 2 — the Resolution (homelab): `docs/programmes/agents-many-nodes/an-t36-pin-settle-retry-guard.md`, `docs/programmes/agents-many-nodes/map.md`

## Verification

| check | input | expected | actual |
|---|---|---|---|
| new test | `cargo test -p relais a_first_settle_answered_unknown_stays_confirmed` | 1 settle, no `gate_settle_unconfirmed`, none booked | ok: `--lib settle` exit 0; the test asserts 1 settle, booked 0, neither decision |
| mutation | guard `tries > 1` removed, same command | compiles; fails on the `gate_settle_unconfirmed` assertion | ok: compiled; failed at `mod.rs:9026` "a first `Unknown` is confirmed: [baseline_deferred, gate_settle_unconfirmed, accept]"; guard restored, `launch.rs` diff empty |
| suite | `cargo test -p relais runner::` | 0 failed; the new test listed ok | ok: `--lib runner::` 564 tests, exit 0 (the new one among them) |

## Decision log

- 2026-11-26: a programme ticket agent writes and reviews this plan (ADR-0080).

## Implementation review

- implementation-review: approve-with-changes; tick Phase 1 and record the actual column, both fixed here; the optional positive-decision assertion is `deliberate: the mutation run is the proof it can fail`; opus.
- code-review backend: approve, no findings; follow-up (log line on a first `Unknown`) filed to the inbox; opus.
- delta after the rebase onto main (tree 63d5c4c9): implementation-review, backend and lang:rust all approve, no new findings; opus.
- code-review lang:rust: approve; one low (bind `gate.settles()` once), `deliberate: the message reads the same Vec, cost nil`; opus.

<!-- panel: repos=relais,homelab adds= reviewers=backend,lang:rust,tui,unix body-sha=6e674ffe4d7a -->
