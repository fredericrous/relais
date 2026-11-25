---
status: active
branch: prog/agents-many-nodes/an-t21-fence-decide-and-feedback
repos: [relais, homelab]
adrs: []
plan-id: p-01M87KZHERERHCHR23XA96XQ0W
---
# Fence decide answers through the grant

## Review panel

👉 **Decide:** none — approve if fencing decide's answers and leaving feedback unfenced closes an-t21.
📍 relais, homelab · 2 phases · next: Phase 1. Panel: backend, lang:rust, tui, unix (opus); codex: not run (programme ticket agent).
**Changed by review:** the grant is kept when a stopped-review approve fails after its ledger commit; the refusal goes through `refusal()` with a next step; the grep check fixed.
**Verdicts:** final: backend, lang:rust, tui, unix approve; 4 carried; words 1197 → 1550 (×1.30); remove 0, merge 0.
📄 Full reviews: [2026-11-24-fence-decide-answers-through-grant.reviews.md](2026-11-24-fence-decide-answers-through-grant.reviews.md)

## Picture

```
 relais decide <run> --answer ...
   decide_inner: state = ctx::shared_state()   (built once)
     |
     +-- revise, review-parts
     |     --> node::resolve_decision_with_intent    (fenced already)
     +-- approve, open stopped review
     |     --> node::answer_under_grant(approve_from_snapshot)  (new)
     +-- approve, reject, ...
     |     --> node::resolve_decision                (new fence)
     +-- salvaged
           --> ledger.record_salvage_contained       (follow-up)

 each fence:  state.answer_decision(run)
   HeldElsewhere -> "relais decide: the decision of run R is
                     being answered elsewhere; this node has not read
                     its state: see relais status, or answer again
                     once that answer is in or its claim lapses
                     (10 minutes)", exit 3
   Granted       -> ledger write; not closed or failed -> release
```

Every decide answer that closes a decision asks the shared half first;
`relais feedback` closes none and asks nothing.

## Goal

an-t21 of agents-many-nodes. `relais decide` answers a decision three ways
that close it, and only the revise answer asks the shared half first
(`node::resolve_decision_with_intent`). The approve and reject answers
(`Ledger::resolve_decision` in `cli/decide.rs`) and the approval of a
stopped review (`approval::approve_from_snapshot`, which closes through
`Ledger::resolve_decision_with_receipt`) answer through the ledger alone, so
under a shared half a person's answer is not fenced against a relais settle
or an answer on another node. After this change every `relais decide`
answer except `salvaged` takes the decision grant before its ledger write,
and a grant held elsewhere refuses it with one stated line and exit 3.
Under `LocalState` (which always grants) nothing changes.

## Non-goals

- **`relais feedback` takes no grant.** The ticket assumed it answers a
  decision; it does not. It records an outcome on an accepted run, whose
  decision is already closed (`cli/feedback.rs` calls
  `Ledger::resolve_decision` only in its test module; the an-t16 holds-until
  line misnamed it). Fencing it would also refuse wrongly: a granted answer
  keeps its grant for `DECISION_GRANT_TTL_SECS` (10 minutes) after a
  successful write (`resolve_decision_with_intent` never releases on success,
  and the fake answers a second ask `HeldElsewhere`, as
  `conformance::a_decision_grant_lapses` pins). A person's feedback right
  after their own approve on the same node would then read "answered
  elsewhere". The ticket closes as partial acceptance on that item (the
  an-t18 precedent).
- **`--answer salvaged`** (`Ledger::record_salvage_contained`) also closes an
  open decision row unfenced. The ticket does not name it; it is filed as a
  follow-up in the relais inbox.
- No change to the `SharedState` trait, the error or grant types (an-t19
  settled them), `LocalState`, `FakeState` or the conformance suite (an-t23
  is in it). an-t17 works in `shared_state/node.rs` and the coordinator; this
  plan only appends to `node.rs`.
- The `--criterion` sign-off and its receipt reseal stay before the grant,
  as they stay before the gaps refusal today: a sign-off is the person's
  statement about one criterion and stands whoever answers the decision. A
  refused approve leaves it recorded, as a gaps refusal already does, and
  the refusal line then adds `(the sign-off of criterion <id> is recorded)`.

## Code review

- relais: backend, lang:rust, unix, tui
- homelab: none — markdown only

## Behaviour

- `node::resolve_decision(state, ledger, run, answer) -> Result<bool,
  NodeError>`: `answer_decision` first. `HeldElsewhere` returns
  `SharedStateError::Refused { reason: "the decision of run <run> is being
  answered elsewhere" }`, the same text the revise path and the runner's
  settles use (one private helper builds it for both), and the ledger is not
  touched. `Granted`, then `Ledger::resolve_decision`: `Ok(true)` keeps the
  grant; `Ok(false)` gives it back (`give_back`); `Err` gives it back and
  returns `compensated(...)`, exactly as `resolve_decision_with_intent`.
- `node::answer_under_grant(state, run, write, closed) -> Result<Result<T,
  E>, NodeError>` for a write whose error is not a `NodeError` (the stopped
  review's `ApprovalError`): the same grant, refusal and release. The grant
  is kept when `closed(&result)` says the decision closed, on `Ok` or `Err`:
  `approve_from_snapshot` can fail writing `receipt.json` after its ledger
  commit (`approval.rs:275-288`), so on `Err` the closure re-reads
  `decision_of_run` and the grant is given back only when the decision is
  still open. A failed release is said on stderr (`give_back`), since the
  caller's error type has no `Compensation` arm. The grant is asked before
  `approve_from_snapshot` runs, so a refused approve writes no artifact.
- `relais decide --answer approve|reject|...` (every answer that reached
  `ledger.resolve_decision`) calls `node::resolve_decision`.
- `relais decide --answer approve` on a stopped review: an OPEN decision is
  approved through `answer_under_grant` (closed = any `Ok`, a sealed receipt or the same one from a racing answer;
  on `Err`, a re-read `decision_of_run` showing it resolved; if that re-read
  fails, the grant is kept, the read error is said on stderr, and it lapses
  within the TTL); the
  repeat on an answered decision (resolved with a receipt row) stays the
  local idempotent exit 0 that heals `receipt.json`, with no grant asked, as
  the revise path answers its repeat from `intent_of_decision` before asking.
- A shared `Refused` on any of the three decide paths (revise included) is
  returned as `refusal(CliOutcome::Blocked, <reason + advice>)`, so
  `decide_command` (decide.rs:61-73) prints one stderr line, `relais decide:
  the decision of run <run> is being answered elsewhere; this node has not read its state:
  see relais status, or answer again once that answer is in or
  its claim lapses (10 minutes)`, with an empty stdout, or under `--json` only
  `{"error": "...", "exit": 3}` on stdout (the plugin's decide tool parses
  that object), and exits 3. The advice is added in decide only; the shared
  reason text is unchanged, so the runner's settle lines do not move. It
  follows `relais promote` in mapping `Refused` to `Blocked`, not in printing
  it itself. Today the revise path prints it as an operational failure,
  exit 1; that changes (a CHANGELOG line says so). Every other shared or node
  error goes through `ctx::node_error("decide", e)`.
- `decide_in` takes a state builder beside `locate`, `ctx::shared_state`;
  `record_answer` calls it at each grant point, so a decide that asks no
  grant builds none.
- No `Ledger::resolve_decision` call is left outside `shared_state/node.rs`
  and test modules (`ledger/` itself defines it).

## Phases

- [ ] Phase 1 — route the three decide answers:
  `crates/relais/src/shared_state/node.rs` (the two functions and their
  `FakeState` tests in `crates/relais/src/shared_state/fake.rs`'s test
  module), `crates/relais/src/cli/decide.rs` (the routing, the state
  parameter, the `Blocked` mapping, and a test-only `SharedState` that
  delegates every trait method (the trait has no defaults) to `LocalState`
  except `answer_decision`, which answers `HeldElsewhere`, since the binary's
  tests cannot see `FakeState` without the `conformance` feature),
  `docs/shared-state.md` (the table row and the fence bullet),
  `CHANGELOG.md` (the exit code line), and the `Blocked` row of the exit-code table in `crates/relais/src/cli/outcome.rs` (its meaning gains "or another answerer holds the shared decision grant; retry once it lapses").
- [ ] Phase 2 — close the ticket in homelab:
  `docs/programmes/agents-many-nodes/an-t21-fence-decide-and-feedback.md`
  (Resolution, `status = "closed"`) and one Decisions-so-far line in
  `docs/programmes/agents-many-nodes/map.md`.

## Verification

| Check | Input | Expected | Actual |
| --- | --- | --- | --- |
| no unfenced resolve | `grep -rnF -e 'resolve_decision(' -e 'resolve_decision_with_receipt(' -e 'approve_from_snapshot(' crates/relais/src --include='*.rs'`, read per hit | every non-test `resolve_decision(` hit is in `shared_state/node.rs` or `ledger/`, or is a `node::resolve_decision(` call in `cli/decide.rs` (the routed answers), and no `ledger.resolve_decision(` call remains in `cli/decide.rs`; `resolve_decision_with_receipt(` is called only from `runner/approval.rs`; the one non-test `approve_from_snapshot(` call is inside the `answer_under_grant` closure in `cli/decide.rs` | |
| node fence (lib) | `cargo test -p relais --lib shared_state::` with new `FakeState` tests: a held grant refuses `node::resolve_decision` and leaves the decision open; a closed-already decision (`Ok(false)`) releases the grant; a written answer keeps it; `answer_under_grant` releases on `closed == false` and on an `Err` that left the decision open, and keeps the grant on an `Err` after the decision closed | pass | |
| decide refusal (bin) | `cargo test -p relais --bin relais cli::decide` with new tests: under the held-elsewhere state, `--answer approve` and `--answer reject` on a waiting run, and approve on an open stopped review, each give `Blocked` (exit 3) with the stated line on stderr and an empty stdout (with `--json`: exactly one stdout object `{"error", "exit": 3}`), and the ledger still shows the decision open (for the stopped review, `<run>/receipts/` holds no artifact); the revise answer gives the same; an approve with `--criterion` adds the sign-off note | pass | |
| LocalState unchanged | the existing `cli::decide` tests, now handed `LocalState::new()`; `runner::tests::review_stop` untouched (`approve_from_snapshot` keeps its signature) | pass unchanged | |
| suite | `cargo test --workspace`; `cargo clippy --workspace --all-targets -- -D warnings`, also with `--features conformance` | green | |
| pilot | the branch binary, `HOME` in a temp dir: a run left waiting on a decision (seeded ledger), `relais decide <run> --answer reject --actor orchestrator` | exit 0, `recorded ...`, as main | |

## Decision log

- 2026-11-24 (implementation) — `closed` on `Ok` is every `Ok`, not
  `sealed.written`: an `Ok` with no write is the same receipt a racing
  answer sealed, so the decision is closed and the grant stays. Found in
  implementation; the reviewed text said `sealed.written`.
- 2026-11-24 (implementation) — the shared state is built lazily, at the
  three points that ask for a grant (not in `decide_inner`): an answer that
  reaches none of them (`salvaged`, `abandon`, argument refusals) is unchanged
  under `shared_state = "external"` with no client. The answers that do ask
  (approve, reject, the other plain answers, revise, the stopped-review
  approve) refuse there with exit 3 through `shared_state_error`, as
  `relais run` does (an-t06, refuse rather than split). `salvaged` stays
  unfenced.

- 2026-11-24 — feedback is not fenced (see Non-goals), because it answers no
  decision and a fence would refuse a person's feedback for 10 minutes after
  their own approve.
- 2026-11-24 — `Refused` maps to `Blocked` (exit 3) on every decide path,
  revise included, following `relais promote`: the person retries later; it
  is not relais's machinery failing.
- 2026-11-24 — the stopped-review repeat asks no grant: it writes no
  decision, and a grant asked there would refuse it within the TTL.
- 2026-11-24 — the plain path asks no extra "already closed" read before
  the grant: a repeat on an answered decision never reaches it, since the
  run is then terminal and `awaits_a_person` refuses it first (decide.rs:502).
- 2026-11-24 — a delegating test state in the binary rather than a
  `conformance` dev-dependency on itself: plain `cargo test` must run the
  refusal tests.

<!-- panel: repos=relais,homelab reviewers=backend,lang:rust,tui,unix body-sha=671f658ee3ae -->
