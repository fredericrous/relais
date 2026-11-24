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

- [x] Phase 5a — record: ADR-0015 replaces `execution.resource-budgets`
  (ADR-0014): a moving host-slot line outlives `admission_wait_seconds`, the
  overrun charged to the total only: `.adr/records/0015-a-moving-host-slot-line-outlives-the-admission-budget.md`,
  `.adr/records/HEADS.md`
- [x] Phase 5b — code: `greenharness-slots = "5"`; `take()` keeps the crate's
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
  run is not blocked. Observed: `host_slots::a_moving_line_outlives_the_admission_budget_and_the_grant_is_not_blocked`
  (5 s budget, 1 s poll, the holder lets go at poll 20): admitted, `overrun()`
  at least 10 s, `waited(Admission)` under 5 s, `exhausted()` is `None`.
  `budgets::a_moving_host_slot_wait_outlives_the_admission_budget_without_exhausting_it`:
  80 s of a 50 s budget, `waited + overrun` is 80 s, the total shrinks by 80 s.
- A stalled line blocks the run `admission_wait_exhausted`, and the row reads
  `"end":"stalled"`. Observed with a 1 s window, not 300 s:
  `host_slots::a_stalled_line_ends_the_wait_and_the_row_says_so` gets
  `LeaseStop::SlotLine(Stalled { window_s: 1, .. })`, a `deferred` row with
  `"end":"stalled"`, and the event `the host slot line made no progress for 1 s
  at position`. The 300 s default is `greenharness_slots::STALL`, read through
  `Stall::window_from` at the engine edge.
- Coordinator, then host slot (overrun), then coordinator: the coordinator
  still exhausts. Observed:
  `host_slots::the_coordinator_still_exhausts_after_a_host_slot_overrun` drives
  `Queue::wait` through `charging` (10 s Budget, 60 s Stall, Budget): overrun
  at least 15 s, then the second coordinator poll ends `Exhausted::AdmissionWait`.
- A line that cannot be read three polls in a row ends the wait with its own
  cause. Observed:
  `host_slots::an_unreadable_line_after_joining_ends_the_wait_with_its_own_cause`
  (the slot directory removed after the first poll): `SlotLine(Unreadable {
  failures: 3 })` and the event `admission_queue_unreadable`.
- `cargo clippy -p relais --all-targets -D warnings` clean; `cargo test -p relais
  --lib -- policy::budgets verify::heavy runner::` 618 passed, 1 failed (the
  coordinator test's off-by-one, fixed), then `host_slots` 12 passed.

## Implementation review

- implementation-review: approve-with-changes, then the delta; the changes were these plan edits (ticks, observed results). Candidates: names.reveal-intent fine (`f` in `fmt`), tests.no-real-waits fine (deliberate, `holds-until:` named).
- backend: approve after the delta. Round 1 high (stall at any point of the budget): deliberate, ADR-0015 point 1 says so; moving-test finding: deliberate, `Stall`'s movement is tested in the crate; new low (the `admission_overrun` text repeats the run's cumulative overrun): deliberate.
- lang:rust: approve-with-changes, delta accepted. `Bound` beside the wait kind: deliberate (a Disk wait ignores it by construction); process-wide `Once`: deliberate. Fixed: typed `SlotLine`, `pub(crate)`, heartbeat.
- architect: approve after the delta. Fixed: ADR-0015 points 6 and 7 with the rollback; the clock seam is a follow-up filed in amont's inbox (`holds-until` in the test).
