---
status: active
branch: fix/queue-wastes-less
repos: [relais]
adrs: [execution.resource-budgets]
---
# Plan: the admission queue wastes less

## Review panel

👉 **Decide:** none — approve if the waste was relais never saying how to re-verify a kept candidate without a worker, not the queue itself.
📍 relais · plan approved by the panel · next: worktree, then Phase 1. Panel: backend (opus-5-5), lang:rust (opus-5-5), codex (gpt).
**Changed by review:** dropped the slot reorder (it broke fairness); dropped requeue and `needs_decision` (ADR-0010, and it touched too many paths); added reviewer admission (`launch.rs:174`).
📄 Full reviews: [2026-11-01-admission-queue-wastes-less.reviews.md](2026-11-01-admission-queue-wastes-less.reviews.md)
**Verdicts:** round 1: 2 rework; round 2: 2 approve-with-changes; Codex: rework, superseded by the shrink; final backend: approve. 1 carried into implementation: "two cases" now lists three.

## Context

On 2026-10-31, three relais runs each waited their whole 3 h `admission_wait` for a heavy slot and ended `blocked (admission_wait_exhausted)`. Two of them already held a worker's candidate (one about $2.16 of model work). The person: "the whole point of relais is to spend less. help with the fixes now". They have since set `admission_wait_seconds = 1800`.

What the code does today, read on 2026-11-01:
- **The candidate survives.** Its sha is written to the attempt row before verification starts (`attempt.rs:635`, state `verifying`). `candidate::resolve` accepts any run that has ended, blocked included (`candidate.rs:64-118`). In the live ledger, all 10 most recent runs blocked on the admission wait still list their candidates.
- **A no-worker recovery already works.** A new `kind: verify` run naming `candidate {run}` re-verifies the candidate with no worker. Store-plan step A1 is doing exactly this for `run-65f285d2aff88-fe37`.
- **Nothing says so.** The blocked outcome names only the budget (`engine.rs:873-896`). An orchestrator reading it starts a `change` run or a `revise`, and so buys the worker again. That is the waste.
- **The attempt is left open.** On a lease stop during verification (`attempt.rs:904`, `:933`), the attempt row stays `verifying` after the run has ended.

## Goal

A run blocked by the queue with a candidate tells its reader how to verify that candidate again without a worker, and its ledger rows are closed.

## Non-goals

- **The order of the heavy-slot and host-slot waits stays as it is.** Every relais heavy check needs both, so a run holding a coordinator token while it waits for a host slot does not lower relais's throughput. Changing the order would lose the coordinator's round-robin and aging, and the run's amont queue position (round 1).
- **Nothing is requeued automatically, and the end state does not change.** ADR-0010 says exhaustion is "evidence for a decision, never a repair". The run stays `blocked (admission_wait_exhausted)`: no new state, no exit-code change, no ADR amendment (rounds 1–2, Codex).
- An expiry with no candidate under verification (setup, a `change` run's baseline, a worker dispatch's queue, integration) is unchanged.

## Behaviour

**1. The outcome names the recovery.** When `admission_wait` expires while a candidate is verified, the run still ends `blocked (admission_wait_exhausted)`, and its detail and text now add the recovery:
- "the candidate is kept: verify it again without a worker with a `kind: verify` run, `candidate {run: <id>, index: <n>}`";
- the detail JSON carries `candidate: {run, index, sha}`.

This covers two cases:
- an attempt's verification (`attempt.rs:904`, `:933`) names this run and that attempt;
- a `kind: verify` run, at any expiry (its baseline, `attempt.rs:904/933` reached through `verify_applied`, its reviewer's admission), names its resolved candidate's `source_run` and index, never its own id;
- a reviewer's admission expiry (`launch.rs:174`) after the candidate passed its checks names that candidate.

The candidate is passed explicitly into `end_on_lease_stop` / `end_on_exhausted` as an `Option`, never looked up in the ledger: a candidate that already failed its checks must not be offered. Every other caller passes `None` and is unchanged.

**2. The attempt is closed.** At those two attempt sites, the attempt is finished `Blocked`, its sha kept, before the run ends.

**3. The skill says it.** The relais plugin skill (`claude-plugin/skills/relais/SKILL.md`, "An existing candidate") adds one line: a run blocked on a budget or a queue with a candidate is recovered with `kind: verify`, never with a `change` run or `revise`. `relais status` shows the outcome's recovery line.

## Phases

- [ ] Phase 1 — relais:
  - `runner/engine.rs`, `runner/attempt.rs`, the verify run's expiry callers (`baseline.rs:105,149,193`, `engine.rs:590`);
  - `runner/launch.rs:174` (admission for model work, which the reviewer shares through `managed_launch`): the `Option` is carried through `managed_launch` / `launch_admitted`. A worker dispatch passes `None`; the reviewer passes the candidate under review, or a verify run's resolved source;
  - the plugin skill line;
  - SPEC §10 and §11, wherever `blocked (admission_wait_exhausted)` is described.

  Release.
- [ ] Phase 2 — observe: for 2 weeks, check that each admission expiry with a candidate was recovered by a `kind: verify` run, with 0 workers bought again. Record the count here.

## Decision log

- 2026-11-01 — Round 1 (backend, lang:rust) asked for rework. Taking the host slot first breaks fairness, and automatic requeue contradicts ADR-0010: both dropped.
- 2026-11-01 — Round 2 proposed ending `needs_decision`. Codex then found that this needs a new candidate form for verify runs and touches more paths (setup, review admission). A read of `candidate::resolve` and the ledger showed the recovery already works, so the state change was dropped. What is left is the text, the skill line, and closing the attempt.

## Verification

- Phase 1, each as input → expected:
  - an admission expiry during an attempt's verification (fake sleeper, `Sizing::Fixed`, budget clock) → `blocked (admission_wait_exhausted)`, with detail `candidate {run, index, sha}`, and the attempt row `blocked` with its sha;
  - a `kind: verify` run built from that detail → resolves the same sha and launches no worker (no worker dispatch row);
  - an expiry in a `kind: verify` run's baseline → the detail names the source run and index, not the verify run;
  - an expiry in a `change` run's baseline or in a worker dispatch's queue → no `candidate` in the detail, unchanged;
  - a reviewer's admission expiry (fake gate stays `Queued`), in a `change` run after its checks pass and in a `kind: verify` run → detail `candidate` names the checked candidate, or for the verify run the source run;
  - `relais status` on the first → the recovery line;
  - the existing `BlockedBy{AdmissionWaitExhausted}` asserts (`tests/budgets.rs`, `tests/mod.rs:4642`) → still pass;
  - `make check` → exit 0.
- Phase 2: the count above, from the ledger (blocked runs whose detail carries a candidate, and the verify runs naming them).

## Outcome

(Filled when the plan closes.)

<!-- panel: repos=relais reviewers=backend,lang:rust,codex body-sha=e3fa9f30e63d -->
