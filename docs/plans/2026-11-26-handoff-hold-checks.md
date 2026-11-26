---
status: in-progress
branch: prog/agents-many-nodes/an-t27-handoff-hold-checks
repos: [relais, homelab]
adrs: []
plan-id: p-01M8AFC0571BS2Z5SSGAVT73VF
---
# Check a retried handoff's hold end and keep its hold on give-back

## Review panel

👉 **Decide:** none — approve if the expiry case and the kept-hold fix close the two an-t23 gaps.
📍 relais, homelab · 2 phases · next: Phase 1. Panel: architect, backend, lang:rust, po, tui, unix (opus); codex: not run (programme ticket agent).
**Changed by review:** `renew_own_handoff` moves the file's hold end; `run_all` case limited to what `LocalState` shares; `adds=contract`.
**Verdicts:** final: backend, architect, unix approve; lang:rust, po, tui approve-with-changes (low/medium only); 12 carried into implementation (Verification naming of the two `node.rs` unit tests and their mutations, post-release check, LocalState note, error arm at both call sites); words 655 → 817 (×1.25); remove 0, merge 0.
📄 Full reviews: [2026-11-26-handoff-hold-checks.reviews.md](2026-11-26-handoff-hold-checks.reviews.md)

## Picture

```
 first grant (t=0, ends 0+H)      retry by me (t=H/2, ends 1.5H)
      |-------------------------------|-----------------|
                   t=H+1: other -> Held { me }   (past the first end,
                                                  not the retry's)
                                      t=1.5H-1: other -> Held
                                      t=1.5H+1: other -> Granted

 node::reserve_handoff, claim file already holds my handoff:
   shared half re-grants me -> file says "held" -> KEEP the shared lease
   (today: release it, dropping a hold the file still keeps)
```

A retry moves the hold's end, and a kept claim file keeps the shared lease; the two new cases hold every client to both.

## Goal

an-t23 left two gaps. (1) No case advances the primary's clock past
`HANDOFF_HOLD_SECS` from the first grant but not from a retry, so a client
could end the hold at the first grant and pass. (2) `node::reserve_handoff`
gives the shared lease back when the claim file refuses after a `Granted`;
a primary re-grants a holder's own retry, so a caller asking for a ticket
whose claim file already holds that holder's `handoff` would lose a shared
hold the file keeps. Close both: the expiry case on the fake with its clock
(every primary runs it), and the give-back fixed with a case in `run_all`.

## Non-goals

- No change to `LocalState`, the fake's own behaviour (the fake already
  moves the end to `HANDOFF_HOLD_SECS` after a retry), or the callers.
- A claim file that holds the ticket for another session, or as `work`,
  still gives the grant back: only the same session's live `handoff` keeps it.

## Code review

- relais: backend, lang:rust (unix and tui left out: no command-line output or flag changes, a library path and test cases only)
- homelab: none — markdown only

## Behaviour

- `a_primary_ends_a_reservation_hold_from_its_last_grant`, in
  `run_all_with_clock` (primaries only; `H = HANDOFF_HOLD_SECS`): me
  reserves (`Granted`); `advance(H/2)`; me retries (`Granted`);
  `advance(H/2 + 1)`: other is `Held { holder: me }` (past the first grant's
  end, not the retry's); `advance(H/2 - 2)`: still `Held`; `advance(2)`:
  other is not `Held` (the hold ended `H` after the retry).
- `SlugLock::renew_own_handoff(id, session, now)`: when a live `handoff`
  claim of `session` holds `id`, rewrite its `claimed_at` and `refreshed_at`
  to `now` and answer `true`; else `false`. Under the lock, so the file's
  hold ends `HANDOFF_HOLD_SECS` after `now`, where the re-granted lease ends.
- `node::reserve_handoff`, `Granted` then the claim file refuses: when
  `renew_own_handoff` is `true`, keep the shared lease and answer `false`
  (not newly reserved); otherwise give it back as today. An error from
  `renew_own_handoff` gives the lease back through `compensated`, as the
  existing local-error arm does, so the person sees one error.
  `reserve_in_empty_programme` has the same give-back on `Ok(None)` and
  gets the same check for its `first` ticket.
  The `holds-until:` marker goes.
- `a_handoff_whose_claim_file_exists_keeps_its_shared_hold`, in `run_all`
  (so `LocalState` passes it too): me reserves through `node::reserve_handoff`
  (`true`, file written); me asks again at a later `now` (`false`, the
  claim file still names my `handoff`); another holder's
  `state.reserve_handoff` is not `Granted` (a primary: `Held` naming me).
  Before the fix the second call released the lease and other was granted.
  The file's hold end moving to that `now` plus the hold is a primary-only
  effect (`LocalState` answers `NodeDecides` and never renews), so a
  `node.rs` unit test on the fake asserts it, with another for
  `reserve_in_empty_programme` (me reserves, asks again: `None`, other `Held`).
- Trait doc and `docs/shared-state.md` now say a primary moves the hold's
  end `HANDOFF_HOLD_SECS` after the retry, checked by the new case;
  CHANGELOG (Unreleased) names relais-fabric as a client that runs both at
  its next pin bump.

## Phases

- [ ] Phase 1: `crates/relais/src/programme/claim/store.rs`
  (`renew_own_handoff`), `crates/relais/src/shared_state/node.rs` (the fix),
  `crates/relais/src/shared_state/conformance.rs` (two cases, array length 13 to 14,
  `run_all_with_clock`), `crates/relais/src/shared_state/mod.rs` (doc), the `conformance.rs` module doc
  (ticket-hold expiry is now a clocked case),
  `docs/shared-state.md`, `CHANGELOG.md`.
- [ ] Phase 2: homelab: an-t27 Resolution, status closed, and its line in
  `docs/programmes/agents-many-nodes/map.md` (Decisions so far).

## Decision log

- 2026-11-26 — Fix the give-back path rather than prove it unreachable:
  the proof rests on two callers' discipline, and one new caller would
  break it silently. decided-by: agent (ADR-0080).
- 2026-11-26 — Keep the lease only for the same session's live `handoff`;
  any other claim-file hold still gives the grant back, so a shared hold
  never outlives a refusal that is not its own holder's.
  decided-by: agent (ADR-0080).

## Verification

- Phase 1: `cargo test -p relais shared_state` lists and passes
  `a_primary_ends_a_reservation_hold_from_its_last_grant` (fake with clock) and
  `a_handoff_whose_claim_file_exists_keeps_its_shared_hold` (fake and
  LocalState; it asserts not `Granted`, never `Held`, since `run_all` cannot
  tell a primary); then `cargo test -p relais` (full) and
  `cargo clippy -p relais --all-targets -- -D warnings`, also with
  `--features conformance`.
- Phase 1, the cases bite (mutations, each reverted): the fake keeps the
  first grant's end on a retry, so the expiry case fails at the `Held`
  step; restoring the old give-back, so the second case fails.
- Phase 2: `relais programme check docs/programmes/agents-many-nodes` exit 0.

<!-- panel: repos=relais,homelab adds=contract -->
