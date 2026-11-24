---
status: done
branch: prog/agents-many-nodes/an-t22-one-shared-state-in-run
repos: [relais, homelab]
adrs: []
plan-id: p-01M87DKARBH0PBHNWKEKWRGJJE
---
# One SharedState per relais run

## Review panel

👉 **Decide:** none — approve if handing the run's one state to every step, with no fallback, closes an-t22.
📍 relais, homelab · 2 phases · next: Phase 1. Panel: backend, lang:rust, tui, unix (opus); codex: not run (programme ticket agent).
**Changed by review:** greps that can fail (fixed strings, main vs branch counts); `shared` built where `load_machine()` sits; pilot diffs main and branch in two directories.
**Verdicts:** final: backend, lang:rust, unix approve; tui approve-with-changes; 2 carried; words 725 → 836 (×1.15); remove 0, merge 0.
📄 Full reviews: [2026-11-24-one-shared-state-per-run.reviews.md](2026-11-24-one-shared-state-per-run.reviews.md)

## Picture

```
 run_command (composition root)
   shared = ctx::shared_state()      <- the one construction
     |
     +--> ctx::machine_of(&*shared)        (was load_machine: 2nd build)
     +--> node::claim_intent(&*shared, ..)
     +--> ctx::price_table_of(&*shared)    (was load_price_table: 3rd build)
     +--> release_intent(&*shared, ..)     (was a build per refusal)
     +--> release_intent_without_run(&*shared, ..)
     +--> execute(RunConfig { shared_state: &*shared, .. })
```

`run_command` builds the state once and hands that one instance to every
step that reads or writes through it; the helpers on the left build none.

## Goal

an-t22 of agents-many-nodes. `relais run` builds its `SharedState` once
(an-t16), but four more are built on the way: `load_machine()` before it,
`load_price_table()` after the claim, and `release_intent` (through
`release_intent_without_run` too) on every refusal path. A closed client
that keeps per-instance state (a connection, a fence token, a lease cache)
would then claim an intent on one instance and release it on another,
which breaks the an-t16 decision "one instance per process fences
consistently". After this change `ctx::shared_state()` is called once per
`relais run` and every step of the run uses that instance. Nothing changes
for one node: `LocalState` holds no state.

## Non-goals

- The other commands' own constructions (`decide`, `programme`, `learn`,
  `replay`, `recipe`, `report`, `native`). `relais replay` has the same
  pattern (`load_machine`, `shared_state()`, `load_price_table`): filed as
  a follow-up, not done here.
- No fallback construction inside `release_intent`. Every call to it sits
  after the intent claim, which already used the run's state, so no caller
  runs before the state exists (the ticket's "keep a fallback only for a
  caller that runs before the state exists" has no such caller). The two
  callers outside `run_command` are its two tests.
- No `CHANGELOG.md` entry: nothing changes for an upgrader or for one node.
- No change to the `SharedState` trait, `shared_state::node`, the runner
  or the coordinator (an-t17 is in `node.rs` and the coordinator).

## Code review

- relais: backend, lang:rust (tui and unix left out: no output, flag or exit code changes)
- homelab: none — markdown only

## Behaviour

- `release_intent(shared: &dyn SharedState, ledger, source, run_id,
  refusal)` and `release_intent_without_run` with the same new first
  parameter. Their bodies build nothing; a failed release still prints
  `relais run: intent <id> could not be released: <e>` and the run's
  outcome is unchanged.
- `ctx::machine_of(&dyn SharedState) -> Result<MachineSettings, CliError>`
  and `ctx::price_table_of(&dyn SharedState) -> Result<PriceTable,
  CliError>` read through a given state, with the operation label
  `"the machine settings"` unchanged (`cli/protocol.rs` maps that exact
  string to `invalid_machine_settings`). `load_machine()` and
  `load_price_table()` stay as thin wrappers for the other commands, and
  `load_machine_pricing()` wraps a `machine_pricing_of(&dyn SharedState)`
  that `price_table_of` also uses, so the label is mapped in one place.
- In `run_command`, `shared` is built where `load_machine()` is called today (after
  `origin_refusal`, `resolve_session` and `load_repo_policy`), then
  `machine = machine_of(&*shared)`. The failure order is the one today's
  `load_machine()` gives: the selection (an absent external client, exit 3)
  is refused before `machine.toml` is parsed. Prices are re-read through
  the same instance where they are read today, so the "prices unavailable"
  release path stays.
- `relais run` stdout, stderr and exit codes are unchanged on every path.

## Phases

- [x] Phase 1 — one state per run: `crates/relais/src/cli/run.rs`,
  `crates/relais/src/cli/ctx.rs`, `docs/shared-state.md` (one bullet beside
  "fences within one instance only": a command builds one state and hands
  it to every step).
- [ ] Phase 2 — close the ticket in homelab:
  `docs/programmes/agents-many-nodes/an-t22-one-shared-state-in-run.md`
  (Resolution, `status = "closed"`) and one Decisions-so-far line in
  `docs/programmes/agents-many-nodes/map.md`.

### 🧑 Your steps

## Verification

| Check | Input | Expected | Actual |
| --- | --- | --- | --- |
| one construction | `grep -cF 'shared_state()' crates/relais/src/cli/run.rs` (whole file: the tests build `LocalState::new()`), on main and on the branch | main: `2` (lines 181, 223); branch: `1`, in `run_command` || main `2`, branch `1` (run.rs:224, in `run_command`) |
| no hidden construction | `grep -nF -e 'load_machine(' -e 'load_price_table(' -e 'load_machine_pricing(' -e 'tuning_bounds(' -e 'machine_for_router(' -e 'shared_state_for_hook(' crates/relais/src/cli/run.rs`, on main and on the branch | main: lines 220 and 365 (the pattern bites); branch: no match || main: 220, 365; branch: no match |
| release tests | `cargo test -p relais --bin relais cli::run` | the two release tests pass, now handed `&LocalState::new()`: they no longer read the developer's own `machine.toml` through `ctx::shared_state()` || 7 passed, both release tests among them |
| suite | `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings` | green || `cargo test --workspace` 0 failed (lib 2671 passed, bin 351, ...); clippy clean, also with `--features conformance` |
| pilot | `relais run` from the main and the branch binaries with `shared_state = "external"` in a sandboxed `machine.toml` (`HOME` in a temp dir), once in a repository with a `relais.toml` and once in a directory with none; stdout, stderr and exit code captured | each pair: the same exit code (3 with a policy), `diff` of stdout and of stderr empty || with a policy: both exit 3, `shared_state_absent`, stdout and stderr `diff` empty; no policy: both exit 2, `not_in_repository`, both diffs empty |

A behaviour test that a passed-in state receives the release needs
`shared_state::fake::FakeState`, which the binary's tests cannot see
(`conformance` is off under plain `cargo test`), and a 25-method recording
wrapper is out of proportion. The new signature makes "the run's state is
the one released" a type fact; the grep proves nothing else is built.

## Decision log

- No fallback construction in `release_intent`: every call sits after the
  intent claim, which used the run's state; the ticket's "caller that runs
  before the state exists" has none.
- No `FakeState` test: the binary's tests cannot see it without the
  `conformance` feature; the signature and the one-construction grep carry
  the guarantee (accepted by every reviewer).
- Phase 2 is the homelab PR on the same branch name, opened after this land.

## Implementation review

- implementation-review: approve-with-changes; empty Actual cells and the unticked Phase 1 fixed in this commit; the release tests' limit kept as stated; candidate `e` binding ruled fine (44k, 17 s).
- backend: approve; the `relais replay` follow-up: deliberate: filed in the relais inbox and reported to the driver, since this ticket writes only its own homelab files (54k, 43 s).
- lang:rust: approve, no findings (48k, 35 s).

## Outcome

`relais run` builds one `SharedState` (run.rs:224) and every step reads and
writes through it: machine settings, prices, the intent claim and release,
the runner. Nothing changes for one node; main and branch print the same
bytes on the refusal paths piloted.

<!-- panel: repos=relais,homelab reviewers=backend,lang:rust,tui,unix body-sha=aadeefd778d6 -->
