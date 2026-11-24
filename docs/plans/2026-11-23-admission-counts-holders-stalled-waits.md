---
status: active
branch: fix/hsa-t11
repos: [relais]
adrs: [ADR-0015]
canonical: amont:docs/plans/2026-11-23-admission-counts-holders-stalled-waits.md
phases: [5]
---
# Admission counts holders; stalled waits defer (relais slice)

Pointer plan. The decision, the Behaviour (section "relais") and the Phase 5
checklist live in the canonical plan in amont; this file holds only what
relais does for Phase 5.

## Code review

- relais: backend, lang:rust, architect (unix, tui left out: no shell or screen change)

## Phase 5 here

- [ ] Phase 5a — record: ADR-0015 replaces `execution.resource-budgets`
  (ADR-0014): a moving host-slot line outlives `admission_wait_seconds`, the
  overrun charged to the total only: `.adr/records/0015-a-moving-host-slot-line-outlives-the-admission-budget.md`,
  `.adr/records/HEADS.md`
- [ ] Phase 5b — code: `greenharness-slots = "5"`; `take()` keeps the crate's
  `Stall`; `admission_overrun` in `Spent`, charged by a host-slot wait past
  its budget and never to `admission_waited`: `crates/relais/Cargo.toml`,
  `crates/relais/src/verify/heavy.rs`, `crates/relais/src/policy/budgets.rs`,
  `crates/relais/src/verify/heavy/tests.rs`
- [ ] Phase 5c — release and install: `CHANGELOG.md`

## Decision log

- 2026-11-24 — The canonical plan's Decision log says every caller uses the
  crate's `wait_in_line`. relais cannot: its `take()` loop heartbeats the
  coordinator lease, charges a budget, honours a cancel flag and sleeps through
  an injected `Sleeper` on every poll, and `wait_in_line` takes an `on_wait`
  that returns nothing, sleeps with `std::thread::sleep` and keeps its test
  seam private. `take()` keeps its loop and drives `Stall::observe`, which is
  public in 5.0.0; the window comes from `Stall::window_from` over
  `GREENHARNESS_SLOT_STALL_S`, read at relais's edge.

## Verification

- A command in a moving line past `admission_wait_seconds` is granted, and the
  run is not blocked: `exhausted()` is `None` after the grant with a non-zero
  overrun.
- A stalled line (300 s with no admission and no change of place) blocks the
  run `admission_wait_exhausted`, the journal row reads `"end":"stalled"`.
- Coordinator, then host slot (overrun), then coordinator: the coordinator
  still exhausts.
