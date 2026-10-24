---
id: ADR-0006
status: accepted
decisions:
  - key: routing.task-type-research
    choice: a change may run below the implementation tier only through a recipe that names its task types and was promoted on replay evidence (at least 14 of the first 15 qualifying trials accepted without escalation); a seeded 10% draw sends a live run of such a recipe to the implementation tier so a Sonnet arm keeps accruing, and the recipe is rolled back when its live Wilson 95% lower bound of acceptance without escalation falls below 0.50 or its cost per accepted task is at or above the type's Sonnet median; the implementation floor stays for every other change
    first: true
    reason: Cost is why task types exist (docs/plans/2026-10-12-task-types.md): a type whose mechanical work Haiku does as well as Sonnet should not pay for Sonnet, but SPEC section 6 puts a change at the implementation floor because complexity and consequence are separate, so the floor is relaxed only where replays measured the research model on that type, with a kept comparison arm and a rollback that follows live evidence (D3, D4; the person, 2026-10-12)
---
# 0006 — a change runs below the implementation tier only on evidence

## Context

SPEC section 6 starts every `change` at the implementation tier, even a one-line diff. That floor protects against a cheap model on consequential work, and it also pays Sonnet for work a research-tier model (Haiku) does as well. Task types (the plan) give relais a way to measure that per kind of work: a recipe that names the types it covers, replayed under the research model against the Sonnet runs the ledger already holds.

## Decision

1. **Admission.** A `change` routes below the implementation tier only through a recipe that names its task types (`task_type`), at the research tier, and that was promoted on replay evidence: `relais recipe promote` admits it only when at least 14 of its first 15 qualifying trials (same recipe id, type and arm, the research model dispatched first, one per task) were accepted without escalation (Wilson 95% lower bound 0.7017). The authorising trials are recorded in the ledger. A replay may run the research tier to measure a candidate; it never changes what a live run does. Each task's own risk floor still applies.
2. **The Sonnet arm stays.** For a live run a typed research recipe covers, `route::route` draws `rng::exploration_draw("task-type", task, run)`; below 0.1 the run takes the implementation tier instead. The draw is reproducible for a task and run, its value and propensity (0.1) are recorded on the first dispatch intent whether or not the run is drawn, and a replay is never drawn.
3. **Rollback.** For a promoted typed research recipe, over its live runs of a type (an infrastructure stop is unknown and excluded), checked at 20 live runs and then every 10, the learn cycle says once (per recipe revision) that a rollback is due and names the command, `relais recipe rollback <name>`, and never writes relais.toml itself, when the Wilson 95% lower bound of acceptance without escalation is below 0.50, or when its cost per accepted task (research attempts plus any escalation) is at or above the type's Sonnet median: the drawn arm's once it has 5 runs, else the ledger's historical Sonnet runs of the type. The learn cycle evaluates it beside the learned artifact's rollback.
4. **Escalation means the escalation tier.** Accepted without escalation means no attempt ran on the escalation tier. A same-tier repair after a failed check or after review findings is not escalation: a run the research model repaired itself and then had accepted counts as a success, whatever its attempt count (the person's decision, 2026-10-24).
5. **Everything else keeps the floor.** A change not covered by such a recipe, a typeless contract, a replay-only candidate and a task whose scope fires an implementation-tier risk rule route as before.

## Consequences

- Cost per accepted task, not only acceptance, decides whether a cheaper tier stays: a type that escalates to Sonnet often costs more than Sonnet alone.
- The draw spends about a tenth of a promoted type's runs at Sonnet prices; that is the price of a comparison that cannot go stale.
- A hand-written typed research recipe in `relais.toml` routes live like a promoted one; the gate is `recipe promote`, and the rollback rule watches whatever the file holds.
