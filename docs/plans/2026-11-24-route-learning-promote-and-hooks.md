---
status: done
branch: prog/agents-many-nodes/an-t18-route-learning-promote-and-hooks
repos: [relais, homelab]
adrs: []
plan-id: p-01M85CMTB6AMSVGMTEEDM6X9TF
---
# Route the learning cycle and hooks through SharedState

## Review panel

👉 **Decide:** none — approve if a bounded flock plus the shared CAS on every pointer write, and session-scoped claim ops, close an-t18.
📍 relais, homelab · 4 phases · next: Phase 1. Panel: architect, backend, lang:rust, po, tui, unix (opus); codex: not run (programme ticket agent).
**Changed by review:** flock held from the read through the CAS; hook deadline on a detached thread with an owned `Arc`; pinned conflict and hook lines.
**Verdicts:** final: architect, lang:rust, po, tui, unix approve; backend approve-with-changes; 9 carried; words 1550 → 2095 (×1.35, high findings exempt); remove 0, merge 0.
📄 Full reviews: [2026-11-24-route-learning-promote-and-hooks.reviews.md](2026-11-24-route-learning-promote-and-hooks.reviews.md)

## Picture

```
 caller (CLI builds state)   shared_state::node      SharedState trait
 +----------------------+   +------------------+   +------------------+
 | relais promote       |-->| swap_registry_   |-->| registry_pointer |
 | relais rollback      |-->|  pointer         |   | swap_registry_   |
 | cycle auto promote   |-->| roll_back_       |   |  pointer         |
 | cycle auto rollback  |-->|  registry (new)  |   |                  |
 |                      |   | flock both paths |   |                  |
 |                      |   | heal dead promote|   |                  |
 +----------------------+   +------------------+   +------------------+
 | programme next       |-->| reserve_handoff  |-->| reserve_handoff  |
 | hook start (resume)  |-->| refresh_session  |-->| renew_tickets    |
 | hook SessionEnd      |-->| end_session      |-->| release_tickets  |
 +----------------------+   +------------------+   | (three new)      |
 | tuning, hook, price, |------------------------->| machine_settings |
 | router state/observe |                          +------------------+
 +----------------------+
```

Every registry write and every programme-claim write outside a test now
goes through `shared_state::node`, which asks the trait first. Library code
(`learn/cycle.rs`, `programme/claim.rs`, `hook/respond.rs`, `router/`) is
handed the state or the settings by the CLI; it never builds one.

## Goal

an-t18 of agents-many-nodes. The learning cycle's automatic promote and
rollback, `relais rollback`, the handoff reservations of `relais programme
next --claim`, and the session hooks' claim refresh and end still write the
node's files directly, so under a shared primary they would race a
`relais promote` or leave shared holds behind. Route them, and the
remaining direct `machine.toml` readers, through `SharedState`. One relais
PR. Nothing changes for one node: under `LocalState` every new trait
method answers "the node decides" or grants.

## Non-goals

- The daily spend check and the runner's settles (an-t16, in flight), the
  coordinator's seats and the daemon's own `machine.toml` read in
  `cli/coordinator.rs` (an-t17, in flight).
- Writers of `machine.toml`: `relais trust grant` (`trust.rs`,
  `cli/trust.rs`), the learning envelope (`learn/envelope.rs`,
  `cli/learn.rs`) and the routing envelope and off switch
  (`cli/native.rs`). The trait has no settings write, and adding one needs
  a decision on which principal may write (an-t12). Filed as a programme
  ticket; this plan's Resolution says the acceptance holds for the
  registry and claims, not for settings writes.
- `relais doctor`'s read of `machine.toml`: it diagnoses the file itself
  (and already reports a failing `shared_state` finding).
- Readers that only name the path in a message: `runner/attempt.rs`, the
  holdout label, the recipe trust message, the pricing preflight.
- The router's inference read of the active artifact (`registry.active()`):
  the artifacts are node-local files; routing reads the node's pointer.

## Code review

- relais: architect, backend, lang:rust, unix (flock order, the hook process and its deadlines; tui left out: the only new output is one stderr line on `relais rollback` and one on the hook, in existing wording)
- homelab: none — markdown only (the ticket closure)

## Behaviour

**Registry.**
- `node::swap_registry_pointer` takes the `.pointer.lock` flock on both
  paths (`NodeDecides` and `Swapped`), so two local pointer writes never
  interleave in one node. The flock is taken before the local pointers are
  read and held across the shared CAS and the local write, never across an
  evaluation or training; its wait stays bounded (`REGISTRY_LOCK_WAIT`,
  2 s): a timeout is `Refused`, exit `Blocked` from the CLI, a note in the
  cycle.
- New `node::roll_back_registry(state, registry, expected_active)`: compare
  and roll back as one step. Under `NodeDecides`: under the flock, the
  local active must equal `expected_active` and `previous.json` must name
  a different artifact, else `CasConflict` when active differs from `expected_active`, `Ok(None)` when there is no previous or it equals the active (today's `NothingToRollBack`, unchanged); then `Registry::rollback`. Under `Swapped` (the shared CAS
  from `expected_active` to the previous id won): the local rollback under
  the flock, taken before the previous id is read and held across the shared CAS, so `previous.json` cannot change between the read and the write; a failed local write puts the shared pointer back
  (`NodeError::Compensation` when that also fails), as the promote does. `relais promote` and `relais rollback` print a dedicated message for `Compensation` naming both pointers and the recovery command, exit `OperationalFailure`.
- `expected` is what the caller judged, never re-read just before the
  swap: the cycle's rollback compares against the artifact
  `active_artifact_check` judged (`ActiveCheck` carries its id); the
  cycle's promote compares against the active the anti-flap rule read in
  `promote_candidate`; `relais rollback` against the active it read. A
  person's promote during a cycle therefore makes the cycle's swap lose
  with `CasConflict`, and a cycle's rollback makes a person's promote lose.
- A lost CAS or a lock-wait `Refused` in the cycle: one plain-text note,
  pinned by a test (`rollback of A wanted (<reason>) but X is active now;
  nothing changed, the next cycle judges X`, or `promotion of B withheld:
  X is active now, not A; nothing changed`), no `record_learning_rollback` row, action not
  `RolledBack`/`Promoted` (`None`, or `AwaitingPromotion` for the
  promote). `relais rollback`: exit `Blocked` with one stderr line naming
  the active artifact, in its own wording (not promote's, which tells the person to run `relais promote`): `relais rollback: another promotion made X active while this ran; nothing changed — run `relais rollback` again if X should go`. A test pins the text.
- Heal (resolves the `holds-until: an-t18` marker in `node.rs`): before a
  swap or rollback under a shared pointer (`Pointer::Shared(Some(p))`) that
  differs from the local active, the node completes the dead promote
  locally under the flock, when `p` exists locally and passes
  `Registry::evaluated`; otherwise it fails closed with `CasConflict`
  naming `p` and the command that resolves it; a cycle blocked this way
  notes it on every cycle. A heal writes one stderr note naming the artifact and both pointers (never silent). `NodeDecides` never heals, so `LocalState` behaviour is
  unchanged. The unnumbered marker in `swap_locally` (cycle writes without
  the lock) is removed with the routing.
- `CycleInputs` gains `shared_state: &dyn SharedState`; `roll_back_active`
  gains `state` and `expected`. `cli/learn.rs` passes
  `ctx::shared_state()?` for `learn cycle` and `relais rollback`.

**Programme claims.**
- New trait methods, with `LocalState`, `FakeState` and conformance cases:
  - `reserve_handoff(&TicketRequest) -> TicketAnswer`: the shared half of a
    `handoff` reservation. `LocalState`: `NodeDecides`. The fake holds a
    handoff row for `HANDOFF_HOLD_SECS`.
  - `renew_tickets(&Principal)`: extend every live `work` hold of that
    holder. `LocalState`: `Ok(())`.
  - `release_tickets(&Principal)`: give back every `work` hold of that
    holder; `handoff` holds stay, as `end_session_once` keeps them.
    `LocalState`: forgets its own epoch rows for that holder's tickets.
  Only existing `SharedStateError` variants (an-t19 froze them). The
  shared `work` TTL is the client's (an-t11: 30 min for a pod claim; an
  operator's follows the local 8 h); renewals come on the events that
  refresh the local claim (a claim, a resumed session). A conformance case
  checks that a hold not renewed lapses (fake: after `TICKET_TTL_SECS`).
- `node::reserve_handoff` and `node::reserve_in_empty_programme`: shared
  first; `Held` skips the ticket (the next candidate is tried, as a lost
  local race does today); `Granted` then the claim file, released once
  when the claim file refuses; a crash between the two leaves a shared handoff hold that ends at `HANDOFF_HOLD_SECS` (accepted, bounded). `claim::next` takes `&dyn SharedState` and
  calls them; its error type becomes `NodeError`, and `cli/programme.rs`
  maps it as the claim command already does.
- `node::refresh_session` and `node::end_session`: the shared renew or
  release, then the local pass (`refresh_session_once`,
  `end_session_once`), both always run; the result carries both. The holder
  is `Principal::operator(session)`, as `relais programme claim` names it.
  `relais programme claim`'s own best-effort refresh uses it too.
- The hooks: `hook::respond::handle` and `handle_with` take
  `Arc<dyn SharedState>` (owned, so a shared call can run on a thread the
  hook abandons at its deadline, as the gatherer does); `cli/doctor.rs::hook_respond` gets it once from a new `ctx::shared_state_for_hook()` (state plus optional warning), so only `cli/ctx.rs` builds a state. Under
  `shared_state = "external"` with no client the hook says one stderr
  line only on the two claim events (continued SessionStart, SessionEnd):
  `relais: shared claims skipped, the local claims were updated:
  machine.toml sets shared_state = "external" and this build has no
  client; set shared_state = "local" or install the build that has one`.
  It runs the local pass with a `LocalState` (chosen in `cli/ctx.rs`, not
  in `hook::respond`), stdout stays byte-identical to the no-warning case
  and the exit stays 0. Shared holds a skipped release leaves end by
  their TTL. A future remote client's calls run inside the hook's
  existing `GATHER_DEADLINE`/`ONCE_GRACE` bounds, and the hook enforces them itself: each shared renew or release runs on a detached `std::thread::spawn` that owns an `Arc` clone and answers over an `mpsc` channel read with `recv_timeout` (the hook's remaining deadline); on a timeout the thread is abandoned, the local pass still runs, and the hold expires by TTL. A test with a `SharedState` whose `release_tickets` sleeps past the deadline checks that the hook answers in time, exits 0 and removes the local work claim.

**Readers.** Through `state.machine_settings()`, each keeping its own
fallback: `tuning_bounds` and `hook_admission_settings` (any error: the
stated defaults, as today for an unreadable file), `load_machine_pricing`
(errors as today: absent is `None` pricing, an error is `CliError`),
`router_state` and `capture_policy` (library: they take the settings
result as a parameter from `cli/native.rs`; an error is the existing
`StateError::Machine` / capture `Off` with its warning). `LocalState`
treats an absent file as defaults, as `router::read_machine` does.

## Phases

- [x] Phase 1 (the headline race fix; independently mergeable, and Phases 2-3 may split to a follow-up PR if review stalls) — registry: `crates/relais/src/shared_state/node.rs`,
  `crates/relais/src/learn/cycle.rs`, `crates/relais/src/cli/learn.rs`,
  `crates/relais/src/shared_state/fake.rs`,
  `crates/relais/src/shared_state/conformance.rs`; one commit
- [x] Phase 2 — programme claims and hooks:
  `crates/relais/src/shared_state/mod.rs`,
  `crates/relais/src/shared_state/types.rs`,
  `crates/relais/src/shared_state/local.rs`,
  `crates/relais/src/shared_state/fake.rs`,
  `crates/relais/src/shared_state/conformance.rs`,
  `crates/relais/src/shared_state/node.rs`,
  `crates/relais/src/programme/claim.rs`,
  `crates/relais/src/hook/respond.rs`, `crates/relais/src/cli/doctor.rs`,
  `crates/relais/src/cli/programme.rs`,
  `crates/relais/tests/shared_state_cli.rs`; one commit
- [x] Phase 3 — readers and docs: `crates/relais/src/cli/ctx.rs`,
  `crates/relais/src/router/mod.rs`, `crates/relais/src/cli/native.rs`,
  `docs/shared-state.md`, `CHANGELOG.md` (the trait gains three methods: a
  breaking change for a client, minor bump); one commit. Before the PR,
  the branch rebases on whichever of an-t16 and an-t17 merged (both edit
  `node.rs`, `fake.rs`, `docs/shared-state.md`)
- [x] Phase 4 — homelab ticket closure:
  `docs/programmes/agents-many-nodes/an-t18-route-learning-promote-and-hooks.md`,
  its line in `docs/programmes/agents-many-nodes/map.md` (Decisions so
  far only), and the follow-up ticket for settings writes, whose id the
  Resolution names: an-t18 closes with settings writes narrowed, not done

## Decision log

- 2026-11-24 — Heal a dead promote rather than reorder the halves, because
  the shared CAS must decide first (an-t15) and the shared pointer is the
  truth once it moved; the heal needs the artifact locally, else it fails
  closed, since artifacts are node-local files.
- 2026-11-24 — Writers of `machine.toml` are narrowed, because a settings
  write through the trait needs an authorisation decision (an-t12's
  principals) that no ticket has made.
- 2026-11-24 — The hook falls back to a `LocalState` under an absent
  external client (with one stderr line), because a hook may never fail a
  session on bookkeeping and the local pass is what ran before.

## Decision log (implementation)

- 2026-11-24 — The module-cycle gate refused `learn -> shared_state` (the
  node half already imports `learn::registry`) and would refuse
  `programme -> shared_state` the same way. Inverted: `learn::cycle` owns a
  `PointerWrites` trait (`PointerRefusal`, finally `Superseded`, `Refused`, `Compensated`, `Failed` (below)), and
  `programme::claim` owns `Reservations` (with `EmptyProgramme`);
  `shared_state::node` implements them as `NodePointers(Arc<dyn
  SharedState>)` and `NodeReservations(&dyn SharedState)`, and the CLI
  builds both. So `CycleInputs` gains `pointers`, not `shared_state`, and
  `roll_back_active` takes `&dyn PointerWrites`.
- 2026-11-24 — The cycle's lost promote is `AwaitingPromotion` with the
  `relais promote <id>` hint (one of the two actions the plan allowed): the
  person decides whether the new artifact should replace the one that won.
  A refused one (a held lock, or a shared pointer this node cannot follow)
  is action `None` with no hint, since the hinted command would be refused
  the same way (implementation review).
- 2026-11-24 — A heal that cannot complete is a `Refused` naming the
  artifact and why, not the `CasConflict` the Behaviour section says: a
  conflict read as a race a retry wins, and the cycle and `relais promote`
  printed "run it again". A failure to read the node's own artifact files
  (`ArtifactError::Io`) is that error. `PointerRefusal` is `Superseded`,
  `Refused`, `Compensated` and `Failed`, so the CLI matches variants and
  never downcasts (lang:rust, architect, backend).
- 2026-11-24 — One bounded-call helper, `shared_state::node::within`
  (a named `std::thread::Builder` thread, `recv_timeout`, `Unanswered` says
  deadline, died or no thread), serves the hook's claim passes and its
  admission settings; the settings read runs on it for every
  implementation, `LocalState` included (architect, lang:rust).
- 2026-11-24 — The "Cycle loses" row runs with the fake's pointer moved
  before the cycle (another node's promote that won), not the first-swap
  wrapper the row names, and only on the fake: under `LocalState` the
  person's promote between check and swap is the `roll_back_active` test
  with a judged artifact no longer active. The cycle then goes on to train
  (action `None`, no rows for lack of data).
- 2026-11-24 — The hook's shared deadline is `SHARED_CLAIM_DEADLINE`, 1 s,
  inside `GATHER_DEADLINE`; the shared call runs first and the local pass
  after it (the architect's carried note: the local pass keeps its budget,
  since `ONCE_GRACE` bounds it separately).
- 2026-11-24 — The work-hold lapse is checked on the fake
  (`a_work_hold_renewed_stays_and_one_left_alone_lapses`), not in
  `conformance`: the trait exposes no TTL, so a generic check cannot know
  when a client's hold lapses.
- 2026-11-24 — The hook's absent-client line ends with the existing
  `ExternalAbsent` sentence (it names the machine.toml path), so the plan's
  wording is its prefix; the integration test pins the prefix and the fix.
- 2026-11-24 — The follow-up for settings writes went to the homelab inbox
  for the driver to file: a ticket agent writes only its own ticket file
  and its Decisions line.

## Verification

| Check | Input | Expected | Observed |
|---|---|---|---|
| Race (both orders) | `LocalState` and `FakeState`; two registries over one dir; promote B judged against A and rollback judged against A, run promote-first then rollback-first | exactly one returns `Ok`, the other `CasConflict`; the pointer files name the winner | promote-first: art-b active, rollback `CasConflict`; rollback-first: art-z active, promote `CasConflict`; on `LocalState` and the fake (`a_promote_and_a_rollback_judged_against_one_artifact_cannot_both_win`) |
| Cycle loses | cycle with an artifact due for rollback; a person's promote lands between the check and the swap (test seam: a wrapper `SharedState` that, on its first `swap_registry_pointer`, moves the fake's pointer or promotes C locally under `LocalState`, then delegates) | action `None`, the note names the other artifact, no rollback row | action `None`; note `rollback of art-b wanted (1 run(s) it routed were reverted or had a regression confirmed) but art-c is active now; nothing changed, the next cycle judges art-c`; no rollback row (fake seam, see the Decision log) |
| Heal | fake's shared pointer moved to an evaluated B via a direct trait call; then `node::roll_back_registry` / `swap_registry_pointer` | local active becomes B first, then the operation runs; with B missing locally: `CasConflict` naming B | shared art-b ahead: art-b made active, then rolled back to art-a on both pointers; art-elsewhere not held: `Refused` naming it with `no artifact art-elsewhere in this registry` (Decision log) |
| Flock on both paths | `.pointer.lock` held through `LockFile::try_acquire`; `swap_registry_pointer` and `roll_back_registry` on `FakeState` (`Swapped`) and `LocalState` | `Refused` after `REGISTRY_LOCK_WAIT`; the shared pointer unmoved | `Refused` on both paths and both implementations with a zero wait; shared pointer and local active still art-a |
| No heal locally | `LocalState`; `relais promote B` while no shared pointer exists | only the requested pointer files change; no `Registry::evaluated` heal call, no heal note | `LocalState` answers `NodeDecides`, `heal` returns at once; existing promote tests and the pilot unchanged |
| Grep | `Registry::promote` / `.rollback(` call sites outside `shared_state/node.rs` and the named in-file test modules (`learn/cycle.rs` tests, `learn/registry.rs` tests); `reserve_handoff`, `refresh_session_once`, `end_session_once` outside `programme/claim/` and `shared_state/node.rs` | none | only `shared_state/node.rs`, `programme/claim/store.rs` and test modules |
| Hooks | conformance cases for the three new methods on both impls; fake: handoff held, session end gives back work holds and keeps handoff | pass | `a_session_gives_back_its_work_and_keeps_its_handoff` passes on both; fake: work given back, handoff held, retry by its own holder granted |
| Hook under absent external | `tests/shared_state_cli.rs`: `shared_state = "external"`, a SessionEnd payload with a claim on disk | exit 0, one stderr line, stdout still a valid hook answer, the work claim file removed | exit 0, one line starting `relais: shared claims skipped, the local claims were updated: machine.toml `, stdout identical to the local run, claim removed; a 10 s slow fake: SessionEnd answers inside 2 s, local pass ran |
| Piloted | built binary in a scratch HOME: `relais promote`, `relais rollback`, `relais programme next --claim` on a scratch programme | same stdout, stderr, exit code and files as `origin/main`'s binary, run non-TTY with `NO_COLOR=1` | branch binary vs installed 0.80.0 (no origin/main binary built on a jammed host): rollback, rollback --json, promote of a missing artifact, programme next, programme next --claim on a scratch programme, a SessionEnd hook: identical on all four streams |
| Suite | `cargo test -p relais --features conformance` | passes | full `cargo test` on Phase 3: lib 2651, bin 64, it 351, programme_claims 16, programme_session 16, shared_state_cli 4, all pass; after the review fixes: lib subset (shared_state, hook, programme, learn, router, cli) 588, `it` (learning, rollback, promote, hook) 45, shared_state_cli 4, all pass |
| Suite after rebase onto an-t16 (#565), 2026-11-24 | `cargo test -p relais --features conformance`; the conflicts were only in CHANGELOG.md and docs/shared-state.md, both resolved by keeping both sides | passes | lib 2668, bin 64, it 351, 23, programme_claims 16, programme_session 16, shared_state_cli 4: all pass. The first lib run (21 min on a loaded host) failed `runner::tests::budgets::a_base_setup_step_cut_off_by_the_total_ends_on_the_total`, which is outside this diff; the budgets module (29) and the whole lib then passed on a re-run. clippy `-D warnings` with `conformance` is clean |
| Lint | the pre-commit gates | clean | fmt, clippy with and without `conformance` (`-D warnings`), module and child cycle checks: clean, before and after the review fixes |

## Implementation review

- implementation-review: round 1 approve-with-changes (banners, discards, real waits, names, heal note) fixed; delta approve-with-changes (test names, lost-promote note) fixed; final tree approve; 127k + 74k + 79k tokens.
- architect: round 1 two lows fixed (`Refused`, one `within` helper); delta approve; `deliberate: within stays in node and PointerRefusal keeps its name (optional moves)`.
- backend: round 1 one medium and three lows fixed (heal note, `Io` kept, `Died`, retried reservation); delta approve; `deliberate: the admission read falls back silently, as an unreadable file always has`.
- lang:rust: round 1 two mediums, four lows fixed, fake's `handoff: bool` `deliberate: a test fake's row`; final tree approve; `deliberate: no Compensated test (needs a primary failing between two calls; node-level test covers compensated())`, rollback's shared errors stay operational, as before.
- unix: round 1 three lows fixed (stderr under --json, one-line warning); deltas approve-with-changes: whitespace collapsed, --json stderr in the CHANGELOG; `deliberate: the withheld-note wording is new on this unreleased branch`.

## Outcome

Built as planned, in one relais PR of five commits after the plan: every
registry pointer write and every programme-claim write goes through the
node half of `SharedState`, and the settings readers go through
`machine_settings()`. A promote and a rollback judged against the same
artifact cannot both land (tested in both orders on both implementations);
the `holds-until: an-t18` marker is resolved by the heal. Not done here, by
decision: settings writes (the trait has none; follow-up in the homelab
inbox), the spend check and settles (an-t16), the coordinator's seats
(an-t17). Surprises: the module-cycle gate forced two small traits owned by
`learn` and `programme`; the host's load reached 300, so the commit gates
deferred clippy and the suite, which were run by hand on every tree.

<!-- panel: repos=relais,homelab adds=contract reviewers=architect,backend,lang:rust,po,tui,unix body-sha=aa0703c7c3ee -->
