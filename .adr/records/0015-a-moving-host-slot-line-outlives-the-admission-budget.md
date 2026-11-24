---
id: ADR-0015
status: accepted
decisions:
  - key: execution.resource-budgets
    choice: a run spends time through four explicit budgets, each with its own named end - `work_seconds` (attempts, candidate verification and review after the baseline; the repository's `max_wall_seconds`, narrowed by `limits.wall_seconds`; `budget_exhausted (wall_clock)`), `admission_wait_seconds` (cumulative time queued for a lease, baseline included; machine setting, default 1800; `blocked (admission_wait_exhausted)`; a host-slot wait whose line is still moving outlives it, the time past it charged to the total only, and it ends blocked after 300 s with no admission and no change of place), `disk_wait_seconds` (cumulative time waiting for the disk floor; machine setting, default 600; `blocked (disk_low)`) and `total_seconds` (end to end from the start of the run; `limits.total_seconds`, default the baseline's command timeouts plus work plus both waits; `budget_exhausted (total)`); a wait consumes its own budget and the total, never work; every command starts with the smaller of its own timeout and what is left of each budget that applies to it, except that a repair or an escalation attempt starts with at least its floor (`[execution] min_repair_seconds`, default 600, and `min_escalation_seconds`, default 900, each capped at the effective `max_wall_seconds`, 0 turning it off) - the work budget is extended by the shortfall when the total covers it and the extension is recorded as `work_extended_s` on `worker_dispatched`, else the attempt is not launched and the run ends `budget_exhausted` with `limit - total` and `cause - repair_floor_exceeds_total`; resource exhaustion is evidence for a decision, never a repair and never a red base; the defaults are a disk floor of 8 GiB, a disk wait of 600 s, an admission wait of 1800 s and heavy slots of cores/4 (at least 1)
    replaces: [ADR-0014]
    reason: relais's host-slot wait ended a run at its admission budget even while the line moved (21 relais waits deferred and 38 hook timeouts in the journal from 2026-11-21 18:00, 14 of the timeouts in a moving line); ADR-0022 in amont makes a wait end on a stalled line instead, and relais follows it for its host-slot wait, where it takes the slots through the same crate
---
# 0015 — a moving host-slot line outlives the admission budget

## Context

ADR-0014 gave `admission_wait_seconds` as the cumulative time a command is queued, and a run ends `blocked (admission_wait_exhausted)` when it is spent. relais's heavy commands queue for host slots through the `greenharness-slots` crate, and amont's ADR-0022 changed what ends a wait there: a line that has stopped moving, not the clock alone. A relais run whose slot line was still advancing was blocked all the same, after a grant was already near.

## Decision

1. **The host-slot wait is bounded by stall.** `take()` keeps the crate's `Stall` (300 s, `GREENHARNESS_SLOT_STALL_S`). While the line moved inside that window the wait goes on after `admission_wait_seconds` is spent.
2. **The overrun is the total's.** Time past the budget is charged to `admission_overrun`, never to `admission_waited`, so `exhausted()` does not end the run after the grant. The total still bounds it.
3. **A stall ends the wait as the spent budget did.** A `deferred` row, then `blocked (admission_wait_exhausted)`, with the reason `the host slot line made no progress for 300 s at position N`; the journal row carries `"end":"stalled"`.
4. **The coordinator's model-work queue keeps its exhaustion.** It charges the same budget and does not take the rule; whether it should is amont's t16.
5. **The disk wait is unchanged.**

## Consequences

- A run can now spend more than `admission_wait_seconds` queued, up to the total.
- `admission_overrun` is in memory only: no ledger migration, and an older binary reads the same rows.
- Roll back by pinning `greenharness-slots = "4.13"` and removing the overrun, in one patch.
