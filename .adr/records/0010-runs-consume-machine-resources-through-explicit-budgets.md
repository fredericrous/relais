---
id: ADR-0010
status: accepted
decisions:
  - key: execution.resource-budgets
    choice: a run spends time through four explicit budgets, each with its own named end - `work_seconds` (attempts, candidate verification and review after the baseline; the repository's `max_wall_seconds`, narrowed by `limits.wall_seconds`; `budget_exhausted (wall_clock)`), `admission_wait_seconds` (cumulative time queued for a lease, baseline included; machine setting, default 1800; `blocked (admission_wait_exhausted)`), `disk_wait_seconds` (cumulative time waiting for the disk floor; machine setting, default 600; `blocked (disk_low)`) and `total_seconds` (end to end from the start of the run; `limits.total_seconds`, default the baseline's command timeouts plus work plus both waits; `budget_exhausted (total)`); a wait consumes its own budget and the total, never work; every command starts with the smaller of its own timeout and what is left of each budget that applies to it; resource exhaustion is evidence for a decision, never a repair and never a red base; the defaults are a disk floor of 8 GiB, a disk wait of 600 s, an admission wait of 1800 s and heavy slots of cores/4 (at least 1)
    first: true
    reason: five runs of one task ended on the machine, not the code - a full disk judged a behavioural failure and repaired into the wall clock, and a worker re-running the suite for 45 minutes under a load average of 300 - because one deadline taken after the baseline counted queue time as work time and left candidate verification unbounded; the person approved the split into work, admission wait, disk wait and total (2026-10-14)
---
# 0010 — runs consume machine resources through explicit budgets

## Context

A run's only clock was `Instant::now() + max_wall_seconds`, taken after the baseline. A queue wait for a lease spent it, so a busy machine looked like a slow worker. Candidate verification was bounded only by each command's own `timeout_seconds`, and the baseline by nothing but those. Under load or on a full disk the run ended as `budget_exhausted` or `interrupted` with a cause that the outcome did not name, and a full disk was judged as a failure of the code and repaired.

## Decision

1. **Four budgets**, after Temporal's schedule-to-start, start-to-close and schedule-to-close split:

   | Budget | Covers | Source | On exhaustion |
   |---|---|---|---|
   | `work_seconds` | attempts, candidate verification and review after the baseline | repository `max_wall_seconds`; `limits.wall_seconds` caps this one | `budget_exhausted (wall_clock)` |
   | `admission_wait_seconds` | cumulative time queued for any lease, baseline included | machine setting `[resources]`, 1800 | `blocked (admission_wait_exhausted)` |
   | `disk_wait_seconds` | cumulative time waiting for the disk floor | machine setting `[resources]`, 600 | `blocked (disk_low)` |
   | `total_seconds` | end to end from the start of the run | `limits.total_seconds`; default the baseline's command timeouts plus work plus both waits | `budget_exhausted (total)` |

2. **Waits never consume work.** A wait consumes its own budget and the total.
3. **Every command is clipped.** A command starts with the smaller of its own `timeout_seconds` and what is left of each budget that applies: baseline commands and base-tree setup draw on the total; task-tree setup, candidate commands and review draw on work and total. A baseline that times out stays `BaselineUnrunnable`: blocked, not red.
4. **One clock.** The budgets read the ledger's injectable clock, so a test that scripts the ledger scripts them.
5. **Exhaustion is evidence.** A disk or memory shortage is reported with a decision, never repaired blindly and never read as a red base. Waiting out a disk shortage is bounded by `disk_wait_seconds` alone, not also by the admission wait.
6. **Defaults.** Disk floor 8 GiB, disk wait 600 s, admission wait 1800 s, heavy slots cores/4 (at least 1), a 64 MiB ballast file, a disk estimate of 2 GiB per check until measured.
7. **Deferred.** Scaling the work deadline by machine load waits until this accounting has run in practice: it cannot rescue a baseline that already timed out, and admission addresses the cause.

## Consequences

- The deadline `Instant` taken after the baseline becomes a `Budgets` value passed to verification and to the native adapter.
- The queue wait for a model dispatch now ends `blocked (admission_wait_exhausted)` (exit 3), where it ended `budget_exhausted` on the wall clock.
- machine.toml gains `[resources]`, the contract gains `limits.total_seconds`; both are optional and a file written before them parses and writes back unchanged.
- The heavy-lease classes, the disk floor and the exhaustion classification are separate steps of the same plan; this record fixes the budgets they draw on.
