---
status: active
plan-id: p-01M8AV1Z1EY98E92MXR92HTCYR
branch: prog/agents-many-nodes/an-t30-give-back-error-arm
repos: [relais, homelab]
adrs: []
---
# Test the give-back error arm

## Review panel

👉 **Decide:** none — approve if pinning the already-chosen give-back with three failure-injecting tests closes the ticket.
📍 relais, homelab · 2 phases · next: Phase 1. Panel: backend, lang:rust, tui, unix (opus); codex: not run (programme ticket agent).
**Changed by review:** the Decision names what a failed renewal leaves behind; tests assert file hold end, root skip and a Drop guard; the unreachable CLI assertion became a pointer to the existing test.
**Verdicts:** round 1: 4 approve-with-changes; final: 4 approve; words 497 → 717 (×1.44, fixes of wrong statements); remove 0, merge 0; `plan-review record` found no transcript (subagent session).

## Picture

```
 shared: Granted(lease)   file: refuses, holds my live handoff
         \                  /
          keep_or_give_back: renew_own_handoff
            log line fails   -> release lease, Err(Claim)
            write fails      -> release lease, Err(Claim)
            both ok          -> keep the lease
 after an Err:  shared hold: released   file: old end, unchanged
```

After a failed renewal the shared hold is gone and the file keeps its old end.

## Goal

an-t27's `keep_or_give_back` keeps the shared lease when the claim file holds
the holder's own live handoff and gives the grant back otherwise; the arm
where the renewal itself fails has no test. Decide what a failed renewal
leaves behind and pin it with tests that inject each failure.

Decision: a failed renewal gives the grant back and is the answer (what the
code does through `compensated`, as every local-error arm does). Left behind:
the shared hold is released, so another node's holder can be granted at once;
the claim file is unchanged and keeps its old end (at most `HANDOFF_HOLD_SECS`
from the first grant), so this node's own sessions are still refused until
then; with a failed write the claims log holds a `refresh` line for a
renewal that did not happen (an attempt, not a renewal). The halves disagree
until the file's end, on the side of the shared hold being shorter; that is
accepted, as the failure is a local disk fault and the next retry renews
both. The caller gets `NodeError::Claim` (or `Compensation` when the
give-back fails too); the CLI turns it into an operational failure whose text
names the path that failed.

## Non-goals

- No behaviour change to `keep_or_give_back` or `renew_own_handoff`; its doc
  comment is corrected to say the above (it claims the halves never disagree).
- No new conformance case: a failing local disk is not a client's concern.

## Code review

- relais: backend, lang:rust (unix and tui left out: no flag or output change, tests and a doc line only)
- homelab: none — markdown only

## Behaviour

- Unit tests in `shared_state/fake.rs` (`FakeState` plays the primary, a real
  claim `Store` plays the file), through `node::reserve_handoff`. Times are
  fixed and distinct: first reserve at `1_000`, retry at `1_000 + HOLD/2`
  after `state.advance(HOLD/2)`, so an unchanged `claimed_at` means something.
  Each asserts: `Err(NodeError::Claim(_))`; its printed text names the path
  that failed (the CLI mapping of `NodeError::Claim` to an operational failure
  is already pinned by the binary crate's `cli/ctx.rs` table test, which a lib
  test cannot reach); the file still holds `t1` at `1_000 + HOLD` and is
  free at `1_000 + HOLD + 1` (the retry's end was not written); another
  holder is `Granted` (the lease was given back).
  - `a_failed_renewal_log_gives_the_shared_grant_back`: the store-wide
    claims log (`<state>/claims/log.jsonl`, not inside the programme's
    directory) is replaced by a directory, so the append fails; the text
    names the log path.
  - `a_failed_renewal_write_gives_the_shared_grant_back` (unix): the
    programme's slug directory (`claims_dir_for_test`, holding the claims and
    `.lock`) is made read-only (0o555), so the log line lands and the atomic
    write fails; the log's last line is a `refresh` for `t1`; the text names
    the claim path. A Drop guard restores the mode (a failed assert must not
    leave an undeletable directory). A probe write that succeeds (root)
    prints `skipped` on stderr and returns, as `prune.rs` does.
  - `an_empty_programmes_failed_renewal_gives_the_shared_grant_back`: the log
    failure through `reserve_in_empty_programme` (`Err`, other holder
    `Granted`).
- The doc comment of `keep_or_give_back` and `docs/shared-state.md` state the
  decision (including that a `refresh` line records an attempt); CHANGELOG
  (Unreleased) one line.

## Phases

- [ ] Phase 1: `crates/relais/src/shared_state/fake.rs` (three tests),
  `crates/relais/src/shared_state/node.rs` (doc comment),
  `docs/shared-state.md`, `CHANGELOG.md`.
- [ ] Phase 2: homelab: an-t30 Resolution, status closed, its line in the
  map's Decisions so far (`docs/programmes/agents-many-nodes/an-t30-give-back-error-arm.md`,
  `docs/programmes/agents-many-nodes/map.md`).

## Decision log

- 2026-11-26 — A failed renewal gives the grant back and returns the error,
  rather than keeping the lease: the file did not renew, so a kept lease
  would outlive its file hold. decided-by: agent (ADR-0080).

## Verification

- Phase 1: `cargo test -p relais --lib shared_state::fake` -> the three
  tests pass; with the `Err` arm of `keep_or_give_back` changed to
  `Err(local) => Err(local.into())` (no release), each fails on the "another
  holder is Granted" assertion -> result pending.
- Phase 2: the ticket file reads `status = "closed"` with a Resolution, and
  the map's Decisions so far has its line -> result pending.

<!-- panel: repos=relais adds=lang:rust reviewers=backend,lang:rust,tui,unix body-sha=10fcdba3c5f4 -->
