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
  like today's `Err` branch. `LocalState` never refuses, so its output is unchanged.
- No new output under `LocalState`.

## Phases

- [x] Phase 1 — field, routing and tests: `crates/relais/src/runner/config.rs`,
  `crates/relais/src/runner/review/engine.rs`,
  `crates/relais/src/runner/engine.rs`,
  `crates/relais/src/runner/scheduler.rs`,
  `crates/relais/src/shared_state/node.rs`,
  `crates/relais/src/shared_state/fake.rs` (a `set_spent` test knob),
  `crates/relais/src/cli/run.rs`, `crates/relais/src/cli/replay.rs`; the
  test `RunConfig` literals in `crates/relais/src/runner/tests/mod.rs`,
  `crates/relais/src/runner/tests/exhaustion.rs`,
  `crates/relais/src/runner/tests/verify_kind.rs`,
  `crates/relais/src/runner/tests/budgets.rs` (the compiler finds any
  missed site); new tests below
- [x] Phase 2 — docs: `docs/shared-state.md`, `CHANGELOG.md`

## Verification

Observed 2026-11-23 on the Mac (`cargo test -p relais --features conformance`).

| Test or check | Input | Result |
| --- | --- | --- |
| `the_daily_ceiling_reads_the_shared_counter_before_dispatch` | fake counter 151 over 150, empty ledger | per-day limit, no dispatch: ok |
| `the_review_check_reads_the_shared_counter_too` | counter raised to 200 while the worker runs | `needs_review`, "spend ceiling was reached before the review", no reviewer: ok |
| `daily_spent_tests` | global 0, global 7, ledger 50 | 0, 7, 50 (the larger): ok |
| `spend_read_tests` | ledger failure; shared failure | `RunError::Ledger`; "the day's spend could not be read: ...", no ANSI: ok |
| `a_settle_answered_elsewhere_...` | widen settle, decision already answered | `needs_decision` stays, no intent: ok |
| `the_same_settle_under_a_free_decision_...` | widen settle, free decision | `cancelled`, intent written, grant kept: ok |
| existing suite under `LocalState` | the unchanged runner, CLI and conformance tests | 2640 lib + 63 + 350 of 351 integration passed; the one failure, `bounded_change_end_to_end...`, passed alone in 29 s (the host was at load 90 to 170) |
| timed-out settle refused by the shared half; `Granted` then local `Ok(None)` released once; step line text | not tested: the timed-out path needs the native harness; the release is the an-t15 fake test; `step` events are not captured | gap, in the ticket's fog |
| `relais run` against the `main` binary under `LocalState` | not run: the host load made two release builds impractical; the unchanged suite is the evidence | gap |

## Decision log

- Child runs share the parent's state by reference, not a fresh build:
  one instance per process fences consistently.
- The review and attempt checks both read through `spent_today`; one change.
- holds-until: `cli/decide.rs` and `cli/feedback.rs` answer through
  `Ledger::resolve_decision` without `answer_decision`; until they are routed
  (a follow-up), a person's answer on another node is not fenced against a
  settle.
- Review changed two choices. The refusal step line is the shared error's own
  text ("refused: the decision of run X is being answered elsewhere"), with no
  added sentence: a closed client's other refusals (authorisation) must not
  read as "no action needed", and a typed answered-elsewhere result belongs
  with an-t19's types. The review check prints a shared failure's sentence
  once. `max(global, ledger)` carries `holds-until: an-t17`.
- Spend reads: one shared call per attempt and one per review dispatch. A
  shared outage fails every run closed, by choice: a ceiling that cannot be
  read is not applied. A signal between the shared answer and the local write
  is the lease's TTL to repair, not this plan's.

<!-- panel: repos=relais reviewers=backend,lang:rust,tui,unix body-sha=7b90edc3bb7b -->
