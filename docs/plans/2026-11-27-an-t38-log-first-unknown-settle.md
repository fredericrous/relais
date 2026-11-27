---
status: active
branch: prog/agents-many-nodes/an-t38-log-first-unknown-settle
repos: [relais]
adrs: []
plan-id: p-01M8DVEB09WDA9VAARWZW5MD67
---
# Log a first settle the coordinator answered `Unknown`

## Review panel

none — approve if one decision event plus a tested stderr line is the whole change.
📍 relais · 4 phases · next: Phase 1. Panel: backend, lang:rust, tui, unix (opus); codex: not run (programme ticket agent).
**Changed by review:** the stderr line is a pure tested function, written without `eprintln!`; its wording says what happened to the money; negative tests on the other settle paths.
**Verdicts:** round 2: 4 approve (after 4 approve-with-changes in round 1); 1 carried (the stderr-line Verification row names the command check); words 650 → 749 (×1.15); remove 0, merge 0.

## Goal

an-t36 pinned that a first settle answered `Unknown` stays confirmed and
books nothing, but writes nothing either. This PR adds one decision event and
one stderr line for that case, `gate_settle_unknown_first`, with the dispatch
and the cost. A retry's `Unknown` stays the only `gate_settle_unconfirmed`.

Which settle answers can be `Unknown` on a first try (read, not assumed):

- `RemoteGate::settle` (`coordinator/mod.rs`, `lifecycle_call`): `Unknown`
  is the coordinator's `Response::Unknown`, which `lifecycle()` returns when
  the dispatch is not in its table.
- `LocalGate::settle` (`admission/mod.rs`, `Admission::settle`): `Unknown`
  when `self.dispatches` has no entry for the id.
- The runner reserves before it runs, and binds and releases before it
  settles, so on a normal run the entry exists at the first settle. A first
  `Unknown` therefore means the entry went missing during the run: the
  coordinator restarted, or its sweep forgot a lapsed lease. The event fires
  on those paths only, not on every run.
- `spent_micros = None` (unknown usage) settles the same way; the `None` case
  does not change the answer.
- The `Unavailable` path is a different outcome (`Err`), and a refusal is an
  `Err` too; neither reaches the `Unknown` arm.

## Phases

- [ ] **Phase 1, the line and the event.** `crates/relais/src/runner/launch.rs`:
  `settle_with_retry` returns a new `SettleTaken::UnknownFirst` for an
  `Unknown` on try 1 (the `tries > 1` guard still returns `Unconfirmed` for
  a retry). The caller emits one stderr line
  (its text starts `relais: dispatch <id>: the settle of <cost> found no
  dispatch`, and it names the command `relais coordinator status` in
  backticks, as its two sibling lines do) and
  `Event::Decision { what: "gate_settle_unknown_first", reason: "<id>: <cost>" }`. The line is built by a pure
  `pub(super) fn unknown_first_line(dispatch, cost) -> String` (like
  `unbooked_line`), written with `let _ = writeln!(std::io::stderr().lock(), ...)`
  (as `counter.rs`; `eprintln!` panics on a closed stderr), and must not
  contain `dropped_micros=` (the counter's filter, `counter.rs:210`, counts any
  such unparsable line as unparsed). The `SettleTaken` doc comments and the
  an-t36 test doc are rewritten to say a first `Unknown` is `UnknownFirst`.
- [ ] **Phase 2, tests.** `crates/relais/src/runner/tests/mod.rs`: `a_first_settle_answered_unknown_stays_confirmed`
  asserts exactly one `gate_settle_unknown_first` and still no
  `gate_settle_unconfirmed` or `gate_settle_failed`. The retry test
  (`a_settle_whose_answer_was_lost_is_retried_and_booked_once`) asserts zero
  `gate_settle_unknown_first`, as do the `SettleUnavailable` and
  `SettleRefused` tests. A unit test asserts `unknown_first_line` has no
  newline, names the dispatch, contains the text `relais coordinator status`,
  and does not contain `dropped_micros=`.
- [ ] **Phase 3, docs.** `docs/shared-state.md`, the settle bullet in "What
  still misses the counter": name `gate_settle_unknown_first` beside the
  `gate_settle_unconfirmed` sentence, with its ceiling (the cost minus the
  reservation if the coordinator's lease lapsed, the whole cost if it
  restarted), and one sentence on why the counter does not read it.
- [ ] **Phase 4, CHANGELOG** `CHANGELOG.md` under `## Unreleased`.

## Counter decision

`relais counter` does NOT read the new event. The counter's inputs are
`coordinator.log` and `coordinator.log.1` lines ending `(dropped_micros=<n>)`,
plus the ledger's `ledger_micros`. A run's decision events and stderr do not
land in either. The cost of a first `Unknown` is in the run's own ledger
entry or not, and whether the coordinator books it at the lapsed lease is
not knowable here; reading it in would turn the 5% check into a guess. The
same holds for `gate_settle_unconfirmed`, which the counter also does not
read. The line and event are for the operator: they name the dispatch and
point at `relais coordinator status`.

## Non-goals

- No change to what a first `Unknown` does: it still books nothing and is not
  retried; only its variant and its record change.
- No change to `coordinator.log` lines or to `counter.rs`.
- No release.

## Code review
- relais: backend, lang:rust (tui, unix left out: no output change beyond one stderr line)

## Verification

| Check | Command | Expected | Actual |
| --- | --- | --- | --- |
| an-t36 test extended | `cargo test -p relais a_first_settle_answered_unknown_stays_confirmed` | 1 settle, 1 `gate_settle_unknown_first`, no unconfirmed/failed | pending |
| retry test unchanged in meaning | `cargo test -p relais a_settle_whose_answer_was_lost_is_retried_and_booked_once` | zero `gate_settle_unknown_first`, one `gate_settle_unconfirmed` | pending |
| mutation | remove the emit in `launch.rs`, run `cargo test -p relais a_first_settle_answered_unknown_stays_confirmed` | fails on the new assertion | pending |
| stderr line | `cargo test -p relais unknown_first_line` | one line, names dispatch, no `dropped_micros=` | pending |
| suite | `cargo test -p relais` | exit 0 | pending |

## Decision log

- 2026-11-27: counter does not read the event (see Counter decision).

<!-- panel: repos=relais adds=lang:rust reviewers=backend,lang:rust,tui,unix body-sha=3ee2d10290cf -->
