---
status: active
branch: feat/spend-through-shared-state
repos: [relais]
adrs: []
plan-id: p-01M84XQ30ZP3VWGQR6K9YGB8W7
---
# Spend check through shared state

## Review panel

👉 **Decide:** none — approve if the cap reads the larger of the shared and local counts and settles go through the shared half.
📍 relais · 2 phases · 1 PR · next: Phase 1. Panel: backend, lang:rust, tui, unix (opus); codex: not run (programme ticket agent).
**Changed by review:** `max(global, ledger)` for the cap; ledger errors stay `RunError::Ledger`; phases merged, test matrix named.
**Verdicts:** final: unix approve; backend, rust, tui approve-with-changes; 9 carried (to the implementation: tui retry hint and EPIPE row, rust doc comments and row wording, backend test files, unix timeout line); words 518 → 774 (x1.49, wrong-statement fixes); remove 0, merge 1.
📄 Full reviews: not kept (the ledger record found no transcript); each verdict is above.

## Picture

```
 cli/run, cli/replay
   |  ctx::shared_state()
   v
 RunConfig.shared_state
   |
   +-- spent_today ---------> node::daily_spent
   |                            -> SharedState::daily_spent
   +-- settle_scope_overflow
   +-- settle_timed_out ----> node::resolve_decision_with_intent
                                -> SharedState::answer_decision
 LocalState: None / Granted, ledger counts and writes as before
```

The run config now carries the trait; the spend check and both settles
reach the shared half before the ledger.

## Goal

Programme agents-many-nodes an-t16. `RunConfig` carries the `SharedState`
trait. The runner's daily spend check and its two automatic settles
(scope widening, timed-out verify) go through it, so a closed client can
count the global daily cap and answer the decision once across nodes.
`LocalState` behaves as before.

## Non-goals

- Seats and the coordinator (an-t17), the learning cycle (an-t18), and the
  error and grant types (an-t19, a sibling PR; this plan does not touch
  `error.rs` or `types.rs`).
- Reserving or settling spend through `reserve_spend` and `settle_spend`:
  the check only reads the day's count (`daily_spent`), as today.
- Any change to what a run prints or records under `LocalState`.

## Code review

- relais: backend, lang:rust (unix, tui left out: no command line surface or process change)

## Behaviour

- `RunConfig` gains `shared_state: &'a dyn SharedState`. The CLI builds it
  once at its composition root (`ctx::shared_state()`, an `Arc` kept alive
  across `execute` and passed as `&*state`), for `relais run` and
  `relais dataset replay`, before any ledger write, so a `CliError` such as
  `ExternalAbsent` changes nothing else. A child run (package) inherits its
  parent's reference. Tests pass `&LocalState::new()`.
- `spent_today` (the attempt loop and the review spend check) calls
  `node::daily_spent(state, ledger, day_start)`. `LocalState` returns
  `None`, so the figure is `ledger.spend_since` as now. A `Some(global)` is
  taken as `max(global, ledger.spend_since)`: nothing feeds the global
  counter until `settle_spend` is wired (an-t17), and local spend is a lower
  bound of global spend, so the cap never counts less than today.
- A shared read error maps by variant: `SharedStateError::Ledger(e)` to
  `RunError::Ledger(e)` (the "ledger:" prefix and `is_storage_full` stay as
  today under `LocalState`), every other variant to `RunError::Other("the
  day's spend could not be read: {e}")` at both call sites (no `From` impl:
  `error.rs` is an-t19's). The run stops before dispatch (fails closed),
  through the existing `RunError` exit path, no new code.
- `settle_scope_overflow` and `settle_timed_out` write through
  `node::resolve_decision_with_intent`. The shared half answers the decision
  once; the local transaction follows; one release on a local refusal. A
  refusal ("being answered elsewhere") leaves the decision to that
  answerer, any error leaves it open for a person; both with a `step` line
  like today's `Err` branch, and a refusal's line adds that the other
  node's answer stands, so no action is needed here. `LocalState` never
  refuses, so its output is unchanged.
- No new output under `LocalState`.

## Phases

- [ ] Phase 1 — field, routing and tests: `crates/relais/src/runner/config.rs`,
  `crates/relais/src/runner/review/engine.rs`,
  `crates/relais/src/runner/engine.rs`,
  `crates/relais/src/runner/scheduler.rs`,
  `crates/relais/src/shared_state/node.rs`,
  `crates/relais/src/cli/run.rs`, `crates/relais/src/cli/replay.rs`; the
  test `RunConfig` literals in `crates/relais/src/runner/tests/mod.rs`,
  `crates/relais/src/runner/tests/exhaustion.rs`,
  `crates/relais/src/runner/tests/verify_kind.rs`,
  `crates/relais/src/runner/tests/budgets.rs` (the compiler finds any
  missed site); new tests below
- [ ] Phase 2 — docs: `docs/shared-state.md`, `CHANGELOG.md`

## Verification

| Test or check | Input | Expected |
| --- | --- | --- |
| fake counter `per_day + 1`, empty ledger | one attempt | per-day limit, none dispatched (attempt loop and review check, one test each) |
| fake counter 0, ledger above `per_day` | one attempt | per-day limit |
| ledger failure under `LocalState` | spend read fails | `RunError::Ledger` as before |
| fake `answer_decision` answered elsewhere | scope-exceeded run, timed-out run | no intent written, `step` names the other node |
| fake `Granted`, local `Ok(None)` | settle | released once |
| fake read error, fake refusal | the step and error lines | text as specified, no ANSI |
| `relais run` against the `main` binary under `LocalState` | per-day limit hit, review read failure, widen settle, timed-out settle, stdout piped to `head -1` | same stdout, stderr and exit |
| `cargo test -p relais`, `--features conformance` | the suite | pass |

## Decision log

- Child runs share the parent's state by reference, not a fresh build:
  one instance per process fences consistently.
- The review and attempt checks both read through `spent_today`; one change.
- holds-until: `cli/decide.rs` and `cli/feedback.rs` answer through
  `Ledger::resolve_decision` without `answer_decision`; until they are routed
  (a follow-up), a person's answer on another node is not fenced against a
  settle.
- Spend reads: one shared call per attempt and one per review dispatch. A
  shared outage fails every run closed, by choice: a ceiling that cannot be
  read is not applied. A signal between the shared answer and the local write
  is the lease's TTL to repair, not this plan's.

<!-- panel: repos=relais reviewers=backend,lang:rust,tui,unix body-sha=7b90edc3bb7b -->
