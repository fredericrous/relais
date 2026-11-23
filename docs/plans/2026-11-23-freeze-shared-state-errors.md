---
status: done
branch: prog/agents-many-nodes/an-t19-freeze-shared-state-errors
repos: [relais, homelab]
adrs: []
plan-id: p-01M84XR8YXQC68ES1XNS092MFD
---
# Freeze the SharedState error and grant types

## Review panel

👉 **Decide:** none — approve if a node-only error type, a lapse contract and one rename are all the closed client needs frozen.
📍 relais, homelab · 3 phases · 2 PRs · next: Phase 1. Panel: architect, backend, lang:rust, po, tui, unix (opus); codex: not run (programme ticket agent).
**Changed by review:** `NodeError` transparent `Shared`, `node_error` mapping, lapse check requires `HeldElsewhere`; breaking CHANGELOG; pinned CLI tests.
**Verdicts:** final: backend, rust, unix approve; architect, po, tui approve-with-changes; 2 carried; words 630 → 1000 (×1.6, test rows and wrong-statement fixes); remove 0, merge 0.
📄 Full reviews: [2026-11-23-freeze-shared-state-errors.reviews.md](2026-11-23-freeze-shared-state-errors.reviews.md)

## Picture

```
 SharedState trait (a closed client implements)      node (relais only)
 +-----------------------------------------+     +----------------------+
 | SharedStateError: LeaseLost ClaimLost   |     | NodeError            |
 |  CasConflict Unreachable SliceExhausted |<----|  Shared(SharedState- |
 |  Refused ExternalAbsent Home Read       |     |   Error) Lock Ledger |
 |  Invalid                                |     |  Claim Registry      |
 | DecisionGrant: Granted | HeldElsewhere  |     |  Compensation        |
 |  (a held grant lapses after 600 s)      |     +----------------------+
 +-----------------------------------------+
```

The left box is what a closed client implements; the right is relais's own node half, which wraps it. The CLI's `node_error` (Phase 2) sends `Shared(e)` to `shared_state_error` and the rest to `Operational`; the lapse check (Phase 3) lives in the conformance suite.

## Goal

Before a closed client (an-t13) implements `SharedState`, settle three
points the an-t15 review left: split a node-only error type out of
`SharedStateError`, state in the trait doc and the conformance suite that a
held decision grant lapses, and rename `DecisionGrant::AlreadyAnswered`
(which means "held elsewhere") to `HeldElsewhere`. One relais PR, a breaking change to the public error types (the CHANGELOG entry names it); no
behaviour changes for one node.

## Non-goals

- Changing the trait's methods or `LocalState`'s answers.
- Routing more call sites through the trait (an-t16 to an-t18).
- Moving `Home`, `Read` and `Invalid` out of `SharedStateError`:
  `machine_settings()` is a trait method and says them.

## Code review

- relais: architect, backend, lang:rust (unix, tui left out: no command line changes; one error type split)
- homelab: none — markdown only (the ticket closure)

## Behaviour

- `SharedStateError` keeps what a trait method can return: `LeaseLost`,
  `ClaimLost`, `CasConflict`, `Unreachable`, `SliceExhausted`, `Refused`,
  `ExternalAbsent`, `Home`, `Read`, `Invalid`. It loses `Lock`, `Ledger`,
  `Claim`, `Registry`, `Compensation` and their `From` impls.
- New `NodeError` (public, `shared_state::node::NodeError`, not re-exported at the root), what the `node`
  functions return: `Shared(SharedStateError)`, `Lock { path, cause }`,
  `Ledger`, `Claim`, `Registry`, `Compensation { local: Box<NodeError>,
  release: Box<SharedStateError> }`. `From<SharedStateError>` and the three
  `From` impls make `?` work as before; `node::compensated` and `give_back`
  change types. `Display` of each existing variant is unchanged, and
  `Shared(e)` is transparent (displays as `e`, `source()` is `e.source()`),
  so no CLI line or exit code moves. `NodeError` and `SharedStateError`
  are not `#[non_exhaustive]`: the trait is unstable until a client exists
  and adding a variant should break its `match`.
- The CLI gets a `node_error` function that sends `Shared(e)` to the
  unchanged `shared_state_error(what, e)` (so `ExternalAbsent`, `Home`,
  `Read` and `Invalid` keep their own `CliError` variants and exit classes)
  and the rest to `Operational`; `load_machine` and `shared_state` keep
  calling `shared_state_error`. The callers are `run`, `learn`, `decide`
  and `programme`. `programme claim` matches
  `NodeError::Claim` and `Compensation`; `promote` matches
  `NodeError::Shared(CasConflict)`.
- `DecisionGrant::AlreadyAnswered` becomes `HeldElsewhere`.
- `fake::DECISION_TTL_SECS` (600) is renamed `types::DECISION_GRANT_TTL_SECS`, its uses in `fake.rs` follow, and it becomes
  contract: the trait doc says an implementation that holds a grant lets it
  lapse after that long unless released, so a grant whose answer never
  landed does not block the decision forever. `LocalState` holds none.
- Conformance: `run_all` is unchanged. New `a_decision_grant_lapses` check and
  `run_all_with_clock<S>(make: impl Fn() -> S, advance: impl Fn(&S, i64))`, which runs `run_all` and the new check;
  the check grants, asks again and requires `HeldElsewhere`, advances past the TTL and requires `Granted`; `LocalState` (no clock, holds none) keeps `run_all`. The fake runs it
  with its own clock; the closed client must pass it, with an injectable clock on its test primary (Phase 3 adds this requirement, the 600 s lapse contract, its single-ledger fence limit and the `HeldElsewhere` meaning to `docs/shared-state.md`; they are not there today).

## Phases

- [x] Phase 1 — types and errors (committed with Phase 2: Phase 1 alone does not build): `crates/relais/src/shared_state/error.rs`,
  `crates/relais/src/shared_state/types.rs`, `crates/relais/src/shared_state/mod.rs`,
  `crates/relais/src/shared_state/node.rs`, `crates/relais/src/shared_state/local.rs`,
  `crates/relais/src/shared_state/fake.rs`
- [x] Phase 2 (same commit as Phase 1) — CLI call sites: `crates/relais/src/cli/ctx.rs`,
  `crates/relais/src/cli/programme.rs`, `crates/relais/src/cli/learn.rs`, `crates/relais/src/cli/run.rs`, `crates/relais/src/cli/decide.rs`
- [x] Phase 3 — conformance and docs (CHANGELOG entry marked breaking, with an old-to-new mapping for the moved variants and the rename; confirm an-t13 has no merged code against the trait before merge): `crates/relais/src/shared_state/conformance.rs`,
  `docs/shared-state.md`, `CHANGELOG.md` (a breaking entry: the removed `SharedStateError` variants and `AlreadyAnswered` to `HeldElsewhere`; minor bump, an-t13's pin follows)

### 🧑 Your steps
none

## Decision log

- The an-t14/15 plan named the three points; `Home`, `Read` and `Invalid`
  stay in `SharedStateError` because `LocalState::machine_settings` returns
  them through the trait.
- The 600 s TTL is far longer than the grant-to-release window (one local SQLite write, then `release_decision`; no measurement is claimed). A lapsed holder's late write is fenced by the node's ledger, not by a token: `resolve_decision_with_intent` writes only while the decision is open, so a second answer finds it resolved and returns `None`. A grant token in the trait is left to an-t13 (a follow-up, not this ticket). The ledger fence holds only within one ledger, so `docs/shared-state.md` states that a client with several answerers adds its own fence, which an-t13 owns.
- Conformance takes the clock as a closure rather than a trait method:
  the trait has no time, and a client with no clock keeps `run_all`.

## Verification

| Check | Input | Expected |
|---|---|---|
| Suite | `cargo test -p relais` shared_state and cli tests | the existing tests pass; the diff of existing test files touches only renamed identifiers and error paths, and the PR body lists those hunks |
| Mapping | a new table test in `cli/ctx.rs`: each `NodeError` variant (`Shared` of `ExternalAbsent`, `Home`, `Read`, `Invalid`, `CasConflict`; `Lock`, `Ledger`, `Claim`, `Registry`, `Compensation`) through `node_error` | `Display` byte-identical to the old text, the same `CliError` variant and exit code; `Shared(Read)` is `CliError::Read` |
| Negative | a wrapper whose grant never lapses, through `a_decision_grant_lapses` | the check panics (`should_panic`) |
| Conformance | `run_all_with_clock` on `FakeState`; `run_all` on `LocalState` | both pass; the fake held the grant |
| Negative (never holds) | a wrapper that answers `Granted` to every ask, through `a_decision_grant_lapses` | the check panics (`should_panic`) |
| Lapse | the fake, grant, ask, advance 601 s, ask | `HeldElsewhere` then `Granted` |
| CLI lines | new tests pinning `programme claim` on a Compensation (both stderr lines) and `promote` on `CasConflict` (stderr line, `Blocked`) | byte-equal lines and exit codes to origin/main 58bf0988 |
| Lint | `make lint` through the gates | clean |

## Implementation review

- implementation-review: round 1 approve-with-changes (test rows, `contains`, phases, doc drift) all fixed or `deliberate`; delta approve. 14 candidates ruled fine.
- architect: moved `NodeError` into `node.rs` so `error.rs` no longer imports the node's stores; delta approve.
- backend: lapse check now bounds the TTL on both sides with an early-lapse negative test; promote and Compensation arms `deliberate: unreachable under LocalState`; delta approve.
- lang:rust: lapse check above the tests module, `node_error` exhaustive, merged imports; delta approve.

## Outcome

Built as planned in the relais PR. Observed: `cargo test -p relais --features conformance --lib --bins` lapse, conformance and `node_error` tests pass, including three `should_panic` negatives (never holds, never lapses, lapses early); `local_state_conforms` and `the_fake_conforms` unchanged; the pre-commit gates passed on the plan and code commits.

Deviations, all recorded: `Compensation.release` is `Box<NodeError>` (the intent path's local release fails with a ledger error, so `compensated` and `give_back` take any release error that converts to `NodeError`); `NodeError` lives in `node.rs` at `shared_state::node::NodeError`; the "CLI lines" row is `deliberate: unreachable under LocalState` (a release never fails there, the CAS needs a race), the arms are renamed one for one and the `node_error` table test pins every variant's line and exit class; the `run`, `learn` and `decide` call sites needed no edit beyond `learn.rs`'s `CasConflict` arm.

Carried: a client with several answerers adds its own late-answer fence (an-t13); nothing pins `promote`'s `CasConflict` arm under a primary.

<!-- panel: repos=relais,homelab adds=contract reviewers=architect,backend,lang:rust,po,tui,unix body-sha=318dca23cc47 -->
