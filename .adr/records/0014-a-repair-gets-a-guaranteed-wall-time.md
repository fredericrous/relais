---
id: ADR-0014
status: accepted
decisions:
  - key: execution.resource-budgets
    choice: a run spends time through four explicit budgets, each with its own named end - `work_seconds` (attempts, candidate verification and review after the baseline; the repository's `max_wall_seconds`, narrowed by `limits.wall_seconds`; `budget_exhausted (wall_clock)`), `admission_wait_seconds` (cumulative time queued for a lease, baseline included; machine setting, default 1800; `blocked (admission_wait_exhausted)`), `disk_wait_seconds` (cumulative time waiting for the disk floor; machine setting, default 600; `blocked (disk_low)`) and `total_seconds` (end to end from the start of the run; `limits.total_seconds`, default the baseline's command timeouts plus work plus both waits; `budget_exhausted (total)`); a wait consumes its own budget and the total, never work; every command starts with the smaller of its own timeout and what is left of each budget that applies to it, except that a repair or an escalation attempt starts with at least its floor (`[execution] min_repair_seconds`, default 600, and `min_escalation_seconds`, default 900, each capped at the effective `max_wall_seconds`, 0 turning it off) - the work budget is extended by the shortfall when the total covers it and the extension is recorded as `work_extended_s` on `worker_dispatched`, else the attempt is not launched and the run ends `budget_exhausted` with `limit - total` and `cause - repair_floor_exceeds_total`; resource exhaustion is evidence for a decision, never a repair and never a red base; the defaults are a disk floor of 8 GiB, a disk wait of 600 s, an admission wait of 1800 s and heavy slots of cores/4 (at least 1)
    replaces: [ADR-0010]
    reason: in the 30 days to 2026-11-09, 22 of the 56 runs that ended at the worker's wall time were repair or escalation attempts launched with only what the earlier attempts had left of the work budget (cut after 78 to 1934 s, 9 of them Opus escalations); the machine decides a repair from counters alone, so relais paid for attempts that could not finish; successful repairs ran p75 436 s / p90 907 s and escalations p75 909 s / p90 1035 s, and an extension of at most the whole floor bounds the extra spend at about $65 per 30 days
---
# 0014 — a repair gets a guaranteed wall time

## Context

ADR-0010 made the work budget the run's: every attempt starts with what is left of it. A repair or an escalation comes after an attempt that used most of that budget, and the machine decides on it from its counters, never from time. In the 30 days to 2026-11-09, 22 such attempts were launched with 78 to 1934 s left and cut at their wall time, 9 of them Opus escalations. ADR-0012 now verifies a timed-out tree that changed, but a repair that never had time to make its change leaves nothing worth verifying.

## Decision

1. **A floor per attempt kind.** `[execution] min_repair_seconds` (default 600) and `min_escalation_seconds` (default 900), from the p75 to p90 of successful attempts of each kind. Each is capped at the effective `max_wall_seconds`, after `limits.wall_seconds` narrows it; `0` turns the floor off. An initial attempt has no floor.
2. **Extended from the total, and recorded.** When the work budget has less left than the floor and the total covers it, the work budget is extended by the shortfall just before `worker_dispatched`, which records `work_extended_s` (rounded up) and a sentence naming the key. The worker is launched with at least the floor, within the total.
3. **Refused when the total cannot cover it.** The attempt is not launched and writes no attempt or dispatch row; the run ends `budget_exhausted` under `limit_reached`, with `limit: total` and `cause: repair_floor_exceeds_total`, and a sentence naming the key and `limits.total_seconds`. The wait budgets still stop the attempt as before.
4. **No new reason.** The extension and the refusal are detail fields of existing transitions, so a ledger stays readable by an older binary.

## Consequences

- One run's work can exceed `max_wall_seconds` by up to `max_attempts − 1` floors, always within the total; a contract with a 60 s wall gets 60 s floors, so its work can reach about 60 s × `max_attempts`.
- The verification of an extended repair still clips to what the budgets have left.
- A policy with no `[execution]` table keeps the default floors and its authority hash; both keys are left out of the declaration at their defaults, and a tuned recipe may not change them.
- Check 14 days after the release: repair and escalation attempts that end at their wall time per week (from about 5.1) and the outcomes of extended attempts. Roll back by setting both keys to `0` if over at least 6 extended attempts more than half still time out or fail.
