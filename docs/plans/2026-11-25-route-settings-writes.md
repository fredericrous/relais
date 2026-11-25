---
status: done
branch: prog/agents-many-nodes/an-t20-route-settings-writes
repos: [relais, homelab]
adrs: []
plan-id: p-01M89SFM54QN5GVXPYGJZX3G6J
---
# Route settings writes through SharedState

## Review panel

👉 **Decide:** none — approve if "operators only write settings, as the whole write on the trait" closes an-t20.
📍 relais, homelab · 3 phases · next: Phase 1. Panel: backend, lang:rust, tui, unix (opus); codex: not run (programme ticket agent).
**Changed by review:** installer's exit stated (warns, exits 0); external clients derive the writer's kind from the credential; verification now names the exit-3 tests.
**Verdicts:** final: unix, tui approve; backend, lang:rust approve-with-changes; 2 carried (timestamp-stripped comparison, credential-bound conformance run); words 751 → 1000 (×1.33, fixes to wrong or unverifiable statements exempt); remove 0, merge 0.
📄 Full reviews: [2026-11-25-route-settings-writes.reviews.md](2026-11-25-route-settings-writes.reviews.md)

## Picture

```
 repo grant -----------\                 SharedState trait
 learning envelope -----+--> ctx::      write_settings(
 routing envelope ------+    write_        operator, edit)
 install (if_absent) --/     settings        |
                                             +-- LocalState: locked,
 only an Operator passes;                    |   atomic file edit
 Node and Task: Refused                      +-- external: the primary
                                                 (an-t13, not built here)
```

The four writers and the installer reach one trait method; only an
operator principal passes, and a refusal changes nothing.

## Goal

an-t20 of agents-many-nodes. an-t18 routed every reader of the machine
settings and left their writers direct: `relais trust grant`, the learning
envelope, the routing envelope and its off switch (and the installer's two
"record when absent" variants of the last two) write the node's own file,
because the trait has no settings write. Under `external` that file is one
nobody reads. Add the write to `SharedState`, decide who may call it, and
route those writers through it, so the an-t18 Answer ("no direct write to
the single-owner state outside `LocalState`") holds in full. One relais PR;
`LocalState` behaves as before.

## Non-goals

- The daemon's read of its limits in `cli/coordinator.rs` and `relais
  doctor`'s own check of the file: reads, not writes (still narrowed).
- Writes by a remote primary's client: this ticket gives the trait method
  and the contract; an external client implements it (an-t13).
- Per-key authorization finer than the principal's kind (a later ticket if
  an operator ever needs a settings write another operator may not make).

## Code review

- relais: backend, lang:rust, unix, tui
- homelab: none — markdown only (the ticket closure)

## Behaviour

**The decision: only an `Operator` principal may write settings** (an-t12:
pods "never change trust, `machine.toml` authority, the daily cap"; a
node's certificate does not make it the person). `write_settings(writer:
&Principal, edit: &SettingsEdit) -> Result<SettingsWrite, SharedStateError>`
answers `SharedStateError::Refused` for a `Node` or `Task` writer with
nothing changed, on every implementation, `LocalState` included: the
check is part of the contract and a conformance check holds a client to it.
The CLI is the operator's hand: it passes `Principal::operator($USER)`
(`operator` when unset). The id is for audit only; no check reads it. The
declared kind is honoured only by `LocalState` (the node's own file). An
external client must not forward it as proof: the primary derives the
writer's kind from the authenticated credential (a node certificate is
never Operator), so a pod running `relais trust grant` is Refused (an-t12).

**Plain values, no callback.** `SettingsEdit` is an enum of the writes the
file took: `GrantTrust`, `GrantRepository`, `SetAutoPromote { if_absent }`,
`AutoPromoteOff`, `SetRoutingEnvelope { if_absent }`, `RoutingOff`.
`SettingsWrite { written, removed }` says whether the settings changed
(`false`: key already granted, or an `if_absent` write met a grant or a
tombstone) and whether an off edit removed a grant or envelope.

**The write is the whole write, not shared-then-local.** Unlike a claim,
the readers already read the shared settings through the trait
(`machine_settings`), so a node-local half would write a file nobody reads.
`LocalState` applies the edit to the node's file through the existing
locked, atomic writer (comments, mode and the 10 s lock wait unchanged);
an external client applies it on the primary.

**Routed.** `cli/trust.rs` (`trust grant`, both scopes), `cli/learn.rs` and
`cli/native.rs` (the learning envelope and its off, the routing envelope and
its off), and `cli/install.rs` (the two `if_absent` installs) call
`ctx::write_settings`, which builds the one state at the composition root.
Under `external` with no client the four CLI commands now exit 3
(`SharedStateAbsent`, the existing one-line sentence naming `machine.toml`
and `shared_state = "local"`) instead of writing the file, as every routed
operation does. `relais install` is the exception: its two `if_absent`
writes print that the envelope or grant was not recorded and the install
still succeeds (one line per write, as a failed record did before), so
routing stays in shadow and promotion stops at `awaiting_promotion`; a CLI
test pins it. Any other failed write (`SettingsWrite`, or a `Refused` an
external client returns) is one stderr line and `OperationalFailure`, as
before. Messages, exit codes and the ledger rows after a write are
unchanged. The path-based
writers in `trust.rs`, `learn/envelope.rs` and `router/mod.rs` become
`pub(crate)`: only `LocalState` (and their own tests) can call them, so
a direct write from the CLI no longer compiles.

**Contract.** `conformance::run_settings_writes(make)` runs
`only_an_operator_writes_settings` and `a_settings_write_reads_back` on a
state `make(path)` builds with a scratch settings path (`LocalState::at`);
it is separate from `run_all` so a check never writes the real file; an
external client, which ignores the path, runs these checks only against a
scratch primary, never a live one. The
fake keeps settings text in memory and applies the same edits.
`SharedStateError::SettingsWrite` carries the local writer's own message.

## Phases

- [x] Phase 1 — trait, `LocalState`, fake, conformance: `crates/relais/src/shared_state/mod.rs`, `crates/relais/src/shared_state/types.rs`, `crates/relais/src/shared_state/error.rs`, `crates/relais/src/shared_state/local.rs`, `crates/relais/src/shared_state/fake.rs`, `crates/relais/src/shared_state/conformance.rs`, `crates/relais/src/coordinator/seats.rs`
- [x] Phase 2 — route the writers: `crates/relais/src/cli/ctx.rs`, `crates/relais/src/cli/trust.rs`, `crates/relais/src/cli/learn.rs`, `crates/relais/src/cli/native.rs`, `crates/relais/src/cli/install.rs`, `crates/relais/src/cli/decide.rs`, `crates/relais/src/trust.rs`, `crates/relais/src/learn/envelope.rs`, `crates/relais/src/router/mod.rs`, `crates/relais/tests/shared_state_cli.rs`, `crates/relais/tests/it/plugin_install.rs`
- [x] Phase 3 — docs and changelog: `docs/shared-state.md`, `CHANGELOG.md`

### 🧑 Your steps

Your steps: none.

## Verification

- `cargo test -p relais --features conformance --lib shared_state`:
  `only_an_operator_writes_settings` with a Node and then a Task writer is
  `Refused` and the settings are unchanged; `a_settings_write_reads_back`
  reads each edit back through `machine_settings`. Both run on `LocalState`
  (scratch path) and on the fake.
- New tests in `crates/relais/tests/shared_state_cli.rs` (the existing
  exit-3 test covers only `trust show`, a read): with `shared_state =
  "external"`, `trust grant`, the learning envelope and its `--off`, and the
  routing envelope and its `--off` each exit 3 with the one-line sentence on
  stderr and the file byte-identical; `relais install` exits 0, says "not
  recorded", and leaves the file byte-identical. Switching one CLI call back
  to a direct writer must fail them (the `pub(crate)` change cannot stop a
  writer inside the library).
- Same file as before: the envelope commands write timestamps from
  `Utc::now()`, so a byte comparison of two runs cannot pass; the existing
  tests that read the written file back (`an_install_records_the_routing_envelope_once_and_a_preview_does_not`,
  `an_install_never_replaces_an_envelope_the_person_recorded`,
  `an_install_after_routing_was_turned_off_leaves_it_off`, the `trust grant`
  tests, and the writers' own comment-and-mode tests) run unchanged on this
  tree and pass. A by-hand run of the envelope commands against a scratch
  config was not made: a hook reserves those commands for the person.

## Results

Observed on the tree before the review fixes, then again after them.
- Conformance, input a Node then a Task writer of `GrantRepository`,
  expected `Refused` and no repository, actual `Refused` twice and
  `trust_repos` empty; an operator write, expected `written`, actual
  `written`; every edit read back (trust, repository, learning grant and
  off, routing envelope and off, `IfAbsent` leaving a grant or tombstone):
  pass on `LocalState::at(scratch)` and on the fake (35 `shared_state`
  tests).
- `shared_state_cli`: five commands under `external`, expected exit 3, one
  stderr line, empty stdout, file bytes unchanged, actual the same (5 tests
  pass); `relais install` under `external`, expected exit 0 with one
  "not recorded" line per write and the file unchanged, actual the same.
- `cargo test -p relais --features conformance`: lib 2718, bin 72, `it`
  353, 26, 16, 5 and the doc test pass; `cargo fmt --check` and `cargo
  clippy --all-targets --features conformance -D warnings` clean.

<!-- panel: repos=relais,homelab reviewers=backend,lang:rust,tui,unix body-sha=f6c833d71392 -->
