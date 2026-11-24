---
status: active
branch: prog/agents-many-nodes/an-t23-conformance-retried-handoff
repos: [relais, homelab]
adrs: []
plan-id: p-01M87KVRXQ6EJ2WAXH68H3AKZB
---
# Conformance case for a retried handoff

## Review panel

👉 **Decide:** none — approve if a holder's own retry re-granted and another holder refused (`Held` from a primary) close an-t23.
📍 relais, homelab · 2 phases · next: Phase 1. Panel: architect, backend, lang:rust, po, tui, unix (opus); codex: not run (programme ticket agent).
**Changed by review:** a primary-only check in `run_all_with_clock`; step messages name check and step; step-5 mutation that actually bites.
**Verdicts:** final: architect, unix approve; backend, lang:rust, po, tui approve-with-changes; 12 carried; words 934 → 1353 (×1.45: round-1 medium edits went past ×1.25 with no person to ask, ADR-0080); remove 0, merge 0.
📄 Full reviews: [2026-11-24-conformance-case-retried-handoff.reviews.md](2026-11-24-conformance-case-retried-handoff.reviews.md)

## Picture

```
 me: next --claim                      other: next --claim
      |                                     |
      v                                     v
 shared half: reserve_handoff(t1)      shared half: reserve_handoff(t1)
      |  granted, then a crash:             |
      |  no claim file                      +--> Held { holder: me }
      v                                     |    (LocalState: the claim
 me retries: granted again,                 |     file refuses)
 higher epoch, claim file written           v
                                       not reserved: only a work claim
                                       or HANDOFF_HOLD_SECS frees t1
```

A reservation retried by its own holder is reserved again, and another
holder's reservation of it is refused; the new conformance case holds
every client to both.

## Goal

an-t18 gave `SharedState` a `reserve_handoff`, and the in-memory fake grants
a reservation again to the holder that made it. Nothing in
`conformance::run_all` states that, so the closed client (an-t13) is not
held to it, and nothing says what another holder gets while the
reservation lasts. This plan adds one conformance case that both
`LocalState` and the fake pass, and writes the rule in the trait doc and
`docs/shared-state.md` (agents-many-nodes an-t23).

## Non-goals

- No change to `FakeState`, `LocalState`, `node::reserve_handoff` or the
  claim files: the fake already behaves as the rule below says, and its
  own unit test (`a_retried_reservation_by_its_own_holder_is_granted_again`)
  stays as it is.
- No fix of the give-back path in `node::reserve_handoff`. When the shared
  half re-grants a holder's own reservation and the claim file then
  refuses, the node gives the shared lease back, which would drop a hold
  the claim file still keeps. Neither caller reaches it today: the
  `next --claim` loop and `reserve_in_empty_programme` ask the shared half
  only for tickets the claim files do not hold, under the same `SlugLock`.
  Phase 2 files the path in the agents-many-nodes inbox, and the ticket's Resolution names that entry; it is not fixed here.
- No expiry check in `run_all`: `HANDOFF_HOLD_SECS` is a primary's
  expiry, tested on the fake, as the module doc already says; a later expiry case belongs in `run_all_with_clock`.

## Code review

- relais: backend, lang:rust (tui and unix left out: no command-line output changes; architect left out: the contract change is one test case, judged at the plan's architect seat; the code adds no module or seam)
- homelab: none — markdown only (the ticket closure)

## Behaviour

The rule, stated once and repeated word for word in spirit in the trait
doc, `docs/shared-state.md`, the Decision log and the homelab Resolution:

- **Its own holder retries** (a `next --claim` whose claim file was never
  written: a crash between the shared grant and the file): the shared
  half answers it again, never `Held`. A primary grants it at a higher
  epoch and the hold's end moves to `HANDOFF_HOLD_SECS` after the retry;
  `LocalState` answers `NodeDecides`. The node then writes the missing
  claim file, so the retry is reserved. The node completes a retry only
  when its claim file is missing; with the file present the ticket is
  already held and `next --claim` skips it.
- **Another holder asks** while the reservation lasts: a primary answers
  `Held { holder: <the reserver> }`; `LocalState` answers `NodeDecides`
  and the claim file refuses. `run_all` holds a client to the end result
  (step 4: not reserved), not to `Held` over `NodeDecides`; a primary is
  held to `Held` in `run_all_with_clock`. Either way the reservation is not made. The
  ticket frees only when the hold ends (`HANDOFF_HOLD_SECS`) or when any
  session's `work` claim takes the reservation over, as today.

The case, `a_handoff_retried_by_its_holder_is_reserved_again`, in
`run_all` (12 checks, from 11), on one ticket `t1` with the fixture's
claim key (`repo_key` = the fixture name, slug `programme`, as `ticket()`
builds it):

1. `state.reserve_handoff(me)`: not `Held` (the shared grant whose claim
   file was never written).
2. `state.reserve_handoff(me)` again: not `Held`; when both answers are
   `Granted`, the retry's epoch is higher.
3. `node::reserve_handoff(state, lock, key, "t1", me, …)` under a
   `SlugLock` of the fixture's store (`store.lock(key, LOCK_WAIT, now)`,
   one `now = now_unix()` for steps 3 and 4): `true`, and
   `fixture.store.holds(key, now)` maps `t1` to
   `Holder::Session { id: me.id, kind: Handoff }`.
4. `node::reserve_handoff(…, other, …)`: `false`.
5. `state.reserve_handoff(other)`: not `Granted`; when `Held`, its
   holder is `me`.

`run_all_with_clock` (primaries only) adds
`a_primary_refuses_another_holders_reservation`: the same steps 1, 2 and 5
with step 1 `Granted` and step 5 `Held { holder: me }`, because a primary's
refusal is the only one another node sees.

The case never retries `me` at the node once the claim file exists (the
give-back path above). Every node call in it takes one fixed `now`.

Each step asserts with a message that states the rule, names the step and
prints the answer, in the form of the existing checks (`"<rule>, got
{answer:?}"`), e.g. `"step 2: a holder's own retry is never Held, got
{retry:?}"`. `run_all` does not print the check's name when a check panics
(`conformance.rs:96-100`), so the message alone tells a client's CI log
which check and step failed.

## Phases

- [ ] Phase 1 — the case and the rule:
  `crates/relais/src/shared_state/conformance.rs` (the case, `run_all` and its `[(&str, Check); 11]` array length, now 12),
  `crates/relais/src/shared_state/mod.rs` (the `reserve_handoff` doc),
  `docs/shared-state.md` (the suite list and the programme-claims bullet),
  `CHANGELOG.md` (Unreleased: a client must now pass this case; name relais-fabric as the consumer to re-run at its next relais pin bump; no release is cut here).
- [ ] Phase 2 — the ticket closure in homelab:
  `docs/programmes/agents-many-nodes/an-t23-conformance-retried-handoff.md`
  (Resolution, status closed, the give-back path filed to the inbox) and this ticket's line in
  `docs/programmes/agents-many-nodes/map.md` (Decisions so far).

## Decision log

- 2026-11-24 — Another holder's reservation of a reserved ticket is
  refused (`Held`, naming the reserver, from a primary), because a
  reservation names the next session of one holder; letting a second
  reservation replace it would hand the ticket to a session the first
  holder never relayed to. Only a `work` claim takes a reservation over,
  as an-t18 decided.
- 2026-11-24 — A holder's own retry is granted again at a higher epoch
  and moves the hold's end to `HANDOFF_HOLD_SECS` after the retry (the
  shared hold's age still counts from the first grant), because the retry writes the missing claim file with a
  fresh `claimed_at`; the shared hold then ends with it.

## Verification

- Phase 1: `cargo test -p relais --lib shared_state::conformance` → the
  three conformance tests pass with the new case on `LocalState`, the
  fake and the fake with its clock.
- Phase 1, the case can fail: remove the fake's own-reservation exemption
  in `FakeState::reserve_handoff` (no commit) → `the_fake_conforms`
  fails on the new case at step 2; revert → passes.
- Phase 1, the case bites on `LocalState` too: `LocalState` refuses
  another holder only through the claim file step 3 writes. Mutation:
  skip step 3 (no commit) → `local_state_conforms` fails at
  step 4 (the node reserves for `other`); restore → passes.
- Phase 1, step 5 bites: in `FakeState::reserve_handoff`, clear
  `row.holder` when `row.handoff` is set, before `try_acquire` (no commit;
  dropping only the early `Held` return is not enough, since `try_acquire`
  still refuses `other`, fake.rs:213-215) → `the_fake_conforms` panics with
  a message starting `step 5:` (`other` is granted); revert → passes.
- Phase 1: `cargo clippy -p relais --features conformance --all-targets`
  and `cargo build -p relais --features conformance` → clean, so the
  case compiles outside `cfg(test)`, as relais-fabric builds it.
- Phase 1: relais-fabric (an-t13) is not on this machine, so its run of the
  case is owed at its next relais pin bump (see Outcome).
- Phase 1, primaries: `run_all_with_clock` also runs
  `a_primary_refuses_another_holders_reservation` (step 1 `Granted`, step 5
  `Held { holder: me }`); the `LocalState` answer `NodeDecides` is outside it
  → passes on the fake with its clock; with the step-5 mutation above
  (holder cleared on a handoff row) it fails with a `step 5:` message.
- Phase 2: in the homelab task worktree, `relais programme check docs/programmes/agents-many-nodes`, run on its own and not piped → exit 0.

## Outcome

- After the next relais release: relais-fabric (an-t13) runs `run_all_with_clock` with `--features conformance` → every check passes, `a_handoff_retried_by_its_holder_is_reserved_again` and `a_primary_refuses_another_holders_reservation` included (step 5 answers `Held { holder: me }`). If it fails, the closed client breaks the rule: file it against an-t13. Do not weaken the case; roll the case back only if the fake or `LocalState` is found to break the rule.


<!-- panel: repos=relais,homelab adds=contract reviewers=architect,backend,lang:rust,po,tui,unix body-sha=c36623a9e794 -->
