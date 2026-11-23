---
status: done
branch: feat/shared-state-owned-values
repos: [relais, homelab]
adrs: []
plan-id: p-01M83SRMV6DHHV8M74KW4ZE5HV
---
# Owned values in the SharedState trait

## Review panel

👉 **Decide:** none — approve if a trait of plain values, with relais composing the shared half and the local write, is the shape to freeze.
📍 relais, homelab · 3 phases · next: Phase 1. Panel: architect, backend, lang:rust, po, tui, unix (opus); codex: not run (programme ticket agent).
**Changed by review:** ticket order kept (`NodeDecides` + `grant_ticket`); intent takeover only from a local claim by `old`; compensation on every shared grant, promote included.
**Verdicts:** 6 approve-with-changes (round 2 and backend's final pass); 6 carried; words 1149 → 1529 (×1.33, high and wrong-statement fixes); remove 0, merge 0.
📄 Full reviews: [2026-11-23-owned-values-in-shared-state.reviews.md](2026-11-23-owned-values-in-shared-state.reviews.md)

## Picture

```
 caller (cli/run, cli/decide, cli/learn, cli/programme)
   |  holds the node handles: &Ledger, &Store, &Registry, pid probe
   v
 shared_state::node::<op>(state, handles, ...)   public free functions
   |  1. shared half -------------> state.<op>(plain values)
   |       LocalState: grants, or "the node decides"
   |       FakeState, closed client: compare-and-set on the primary
   |  2. local write on the handles (today's code)
   |  3. local refusal after a shared grant: release the shared hold
   v
 the answer the caller matched before (Claim, TicketGrant, ...)
```

The trait sees only plain values; the handles never leave the caller, and
the node functions compose the shared half with today's local write.

## Goal

an-t14 left node-local handles in `SharedState`'s signatures: `&Ledger`
(a SQLite connection with its clock), the claim `&Store`, `&Registry`,
`&EvaluatedArtifact`, the `&dyn Fn(u32) -> SuccessorProcess` pid probe, and
request structs that borrow (`TicketClaim<'a>`). A primary on another node
cannot receive any of them, so a closed client cannot map the trait onto a
call to it. After this change the trait carries only the shared half, in
plain values; the node-local half moves to public free functions that take
the handles and compose the two halves. Nothing changes for a single-node
user.

## Non-goals

- Routing anything new through the seam: the spend check and settles
  (an-t16), the seats (an-t17), the learning cycle and hooks (an-t18).
- Replacing relais value types the trait returns or takes (`MachineSettings`,
  `MicroUsd`, `RunId`, `Contest`): they are values, not handles; the client
  still pins one exact relais version (an-t13).
- Serialization derives or a wire format: the primary's protocol is the
  closed layer's.

## Code review

- relais: backend, architect, lang:rust (unix, tui left out: no new command line or output; the CLI lines stay byte for byte)
- homelab: none — markdown only (the ticket closure)

## Behaviour

**Why the handles stay with the callers.** `decide_in` takes the ledger
path from its test (`StateLocations`), and `Ledger` carries a connection and
an injected clock; an implementation that opened the node's ledger itself
would write another ledger than the caller's, or lose its clock. So the
implementation never sees a handle.

**The trait** (`crates/relais/src/shared_state/mod.rs`), the changed methods:

| Method | Takes | Answers |
|---|---|---|
| `daily_spent` | `day_start: &str` | `Option<MicroUsd>`: `None` = no shared counter, the node's ledger is the count |
| `claim_ticket` | `&TicketRequest { repo_key, slug, id, holder: Principal, contest }` | `TicketAnswer::NodeDecides`, `Granted(LeaseToken)` or `Held { holder: Principal, age_secs }` |
| `grant_ticket` (new) | `&TicketRequest` | `LeaseToken`: only after a local `Claimed` under `NodeDecides` |
| `release_ticket` (new) | `&LeaseToken` | `()`; fenced |
| `claim_intent` | `intent_id: &str, run: &RunId, replacing: Option<&RunId>` | `IntentAnswer::Granted` or `HeldBy { run }`; `replacing` is a compare-and-set on the held run |
| `release_intent` (was `release_claim`) | `intent_id: &str, run: &RunId` | `()` |
| `answer_decision` (was `resolve_decision_with_intent`) | `run: &RunId` (the decision's id) | `DecisionGrant::Granted` or `AlreadyAnswered`: compare-and-set, fails closed offline |
| `release_decision` (new) | `run: &RunId` | `()` |
| `registry_pointer` | nothing | `Pointer::NodeDecides` or `Shared(Option<String>)` |
| `swap_registry_pointer` | `expected: Option<&str>, to: Option<&str>` | `Swap::NodeDecides` or `Swapped`; `CasConflict` as before |

Unchanged: `machine_settings`, `slice`, the spend and seat leases,
`announce_schema`, `publish`, `queued`, `mode`, `selection`. Borrowed
parameters are plain data (`&str`, `&RunId`, `&LeaseToken`); no parameter
or request struct borrows a handle or has a lifetime.

**The node half** (`crates/relais/src/shared_state/node.rs`, `pub` because
the CLI is a separate binary target; documented as relais's own, outside
the contract a client implements), one function per changed operation,
shared half first, then the local write, then one release when the local
step refuses after a shared grant:

- `daily_spent(state, ledger, day_start)`: the shared count, else
  `ledger.spend_since`.
- `claim_ticket(state, store, &TicketClaim)`: `NodeDecides` runs today's
  lock, sweep and `claim_work`, and calls `grant_ticket` only on `Claimed`
  (today's order); `Held` returns `Taken::Refused` (a `Holder::Session` of
  kind work) without touching the store; `Granted` runs the same store
  steps, and a refusal there calls `release_ticket`. `TicketClaim` and
  `TicketGrant` stay as they are (this function's input and answer).
- `claim_intent(state, ledger, intent_id, run, alive)`: `Granted` runs
  `ledger.claim_intent`, and anything but `Claimed` releases the shared
  claim. `HeldBy { old }` reads the local intent first: only when its row
  is `Claimed` by `old` does it run `ledger.claim_intent` (the pid probe and
  grace judge, as today); a pending or absent local row, or one held by
  another run, answers `AlreadyClaimed { run: old }` with no local write (a
  run on another node is never judged by this node's probe). A local
  `Claimed` that moved the row from `old` asks the shared half again with
  `replacing: Some(old)`, and a refusal there gives the local claim back
  (`ledger.release_claim`) and answers `AlreadyClaimed`; any other local
  answer is returned.
- `release_claim(state, ledger, intent_id, run, refusal)`: the shared
  release, then `ledger.release_claim`, which runs even when the shared
  release failed; that failure is then the error returned.
- `resolve_decision_with_intent(state, ledger, run, answer, intent)`:
  `AlreadyAnswered` answers `None` (no open decision); `Granted` runs the
  one ledger transaction (answer and intent stay atomic, locally), and a
  local `None` calls `release_decision`.
- `registry_pointer(state, registry)` and `swap_registry_pointer(state,
  registry, expected, evaluated)`: under `NodeDecides`, today's flock,
  compare and promote, moved out of `LocalState` with the `.pointer.lock`
  comment and its never-delete rule verbatim, and the same error variants
  (lock timeout `Refused`, conflict `CasConflict`); under `Swapped` the
  local promote follows without a second compare, and a failed promote
  swaps the shared pointer back (`expected = to`, `to = expected`) and
  returns the promote's error. `&EvaluatedArtifact` stays here, so the
  evidence proof stays in the type.
- **A failed release never replaces the local answer.** A refusal value is
  returned as it is; a local error is returned with the release failure
  joined in one `SharedStateError::Compensation { local, release }`. An
  unreleased shared hold ends by its TTL (tickets) or a `replacing` claim
  (intents).

**LocalState has no shared half**: `daily_spent` is `None`, the pointer is
`NodeDecides`, a ticket request is `NodeDecides` and `grant_ticket` is
today's epoch grant, intents and decisions always grant, and the releases
do nothing. Every node function therefore runs the code the trait method
ran before, in the same order, so a refused ticket never moves the
holder's epoch; the refusal for `external` without a client still comes
from `ctx::shared_state()?` before any handle is touched.

**The fake** keeps the shared half in memory, as a primary would: the
pointer and ticket rows (unchanged), an intent table (one run per intent,
compare-and-set, `replacing` moves it) and a set of answered decisions.
Claims, answers and releases fail closed when degraded, as they did.

**Conformance**: the nine properties keep their names and run through the
node functions with the `Fixture`'s handles (a fixture is test code, not a
trait signature); `a_ticket_has_one_holder` also asserts that the first
holder's lease still releases after the second holder's refusal.
`concurrent_local_swaps_land_exactly_once` goes through
`node::swap_registry_pointer`. New fake tests: a local refusal after a
shared grant leaves the intent free; a claim held by a gone run moves to
the new run; a shared `HeldBy { old }` with the local row pending answers
`AlreadyClaimed` and leaves `old` the shared holder; a failed local promote puts the shared pointer back; a
degraded release after a local refusal returns the refusal.

## Phases

- [x] Phase 1 — the trait in owned values, the node half, LocalState, the
  fake, conformance and callers: `crates/relais/src/shared_state/mod.rs`,
  `crates/relais/src/shared_state/node.rs`,
  `crates/relais/src/shared_state/local.rs`,
  `crates/relais/src/shared_state/fake.rs`,
  `crates/relais/src/shared_state/conformance.rs`,
  `crates/relais/src/cli/run.rs`, `crates/relais/src/cli/decide.rs`,
  `crates/relais/src/cli/learn.rs`, `crates/relais/src/cli/programme.rs`
- [x] Phase 2 — docs: `docs/shared-state.md` (the "For a closed client"
  section: the trait takes plain values, the node half is relais's; replace
  the bullet that lists `Ledger`, `Store`, `Registry`, `EvaluatedArtifact`
  as parameters),
  `CHANGELOG.md`
- [ ] Phase 3 — homelab: the an-t15 Resolution and its Decisions-so-far
  line: `docs/programmes/agents-many-nodes/an-t15-owned-values-in-shared-state.md`,
  `docs/programmes/agents-many-nodes/map.md`

### 🧑 Your steps

## Decision log

- 2026-11-23: the handles stay with the callers and the trait carries the
  shared half only (not an implementation bound to node paths): `decide_in`
  injects its ledger path and `Ledger` carries a clock.
- 2026-11-23: shared first, then local, with one release on a local refusal;
  the same order for every operation, so an external client never sees a
  local write it did not grant.
- 2026-11-23 (implementation review): `answer_decision`'s `AlreadyAnswered`
  is a `Refused` error ("being answered elsewhere"), not `None`, and a
  primary's decision grant lapses (the fake: 10 minutes), so a node that died
  between the grant and its ledger write does not hold the decision for ever.
- 2026-11-23: deliberate: `SharedStateError` keeps its `Ledger`, `Claim`
  and `Registry` pass-through variants, produced only by `node`; splitting a
  `NodeError` out changes every CLI error mapping the routed commands use,
  which an-t16 to an-t18 rework. Filed as a programme follow-up, to decide
  before the trait is frozen.
- 2026-11-23: deliberate: a give-back that fails after a local refusal is
  one `relais: the shared hold on … could not be given back` line printed by
  the library: the answers it would ride on are the ledger's `Claim` and the
  store's `Taken`, shared by every caller, and the line only appears under
  an external client.
- 2026-11-23: the fleet pack refresh (`.adr/packs/decisions.pack`) rides
  this branch, as worktree-task's Start step does.

## Verification

| Check | Input | Expected | Actual |
|---|---|---|---|
| no handle in the trait | `awk '/^pub trait SharedState/,/^}/' crates/relais/src/shared_state/mod.rs` piped to `grep -nE` on `Ledger`, `Store`, `Registry`, `EvaluatedArtifact`, `dyn Fn`, `<'` | no line | no line (grep exit 1) |
| conformance | `cargo test -p relais --lib shared_state::` | the 9 properties for LocalState and the fake; the race and the new fake tests | 16 `shared_state::` tests pass inside the workspace run on the final tree |
| existing suite unchanged | `git diff origin/main -- ':!crates/relais/src/shared_state'` | no added or removed `#[test]`, `assert` or `mod tests` line; `cargo test --workspace` passes | no such line; workspace: lib 2619 passed, 0 failed, 5 ignored (main's lib had 7 fewer `shared_state::` tests), other binaries 63, 351, 16, 16, 3, 1 passed; commit gate 30 checks passed |
| piloted CLI | `main` and branch binaries on one scratch `RELAIS_STATE_DIR`, recreated per case: `programme claim` fresh and held by another session, `promote` of an unknown artifact, `run --intent` | identical stdout, stderr and `$?` | identical: claim fresh rc 0, held rc 3 (retry lines), promote rc 3, run rc 2. Not run: promote success, CAS conflict and held lock (an evaluated artifact needs a training run to seed) and a claimed intent (`run --intent` refuses before the claim outside a plugin session); those paths are covered by the conformance properties, the LocalState race test and the fake tests |
| feature build and lint | `cargo build -p relais --features conformance`; `cargo clippy -p relais --all-targets --features conformance -- -D warnings` | builds, no warning | both clean |

## Implementation review

- implementation-review: approve (Delta; round 1 approve-with-changes, 2 blocking fixed: fake race, a placeholder name); 88k+34k, 280+25 s.
- backend: approve-with-changes; decision-grant lapse fixed in node and fake; deliberate: the lapse goes in the trait doc and conformance with the closed client (follow-up); 69k+38k.
- architect: approve-with-changes; TicketClaim moved; deliberate: NodeError split, library stderr line, `AlreadyAnswered` rename and lapse doc (follow-ups); 59k+37k.
- lang:rust: approve (Delta); race fixed, closure renamed; deliberate: the stderr line, an `i64` add on the fake's own clock; 76k+29k.

## Outcome

The trait carries plain values only; relais's `shared_state::node` composes
the shared half and the local write; one node sees no change (pilot
identical, workspace suite green). Follow-ups for the programme, before the
trait is frozen: split a `NodeError` out of `SharedStateError`; state the
decision-grant lapse in the trait doc and the conformance suite, and rename
`DecisionGrant::AlreadyAnswered` to what it means (held elsewhere).

<!-- panel: repos=relais,homelab adds=contract reviewers=architect,backend,lang:rust,po,tui,unix body-sha=4e1c775f4278 -->
