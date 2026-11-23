---
status: done
branch: feat/shared-state-trait
repos: [relais, homelab]
adrs: []
plan-id: p-01M834ECAC6WNXWTKYSRMJA2W5
---
# The SharedState trait and LocalState

## Review panel

👉 **Decide:** none — approve if a trait with node-local handles as parameters is the seam an-t13 can build on.
📍 relais, homelab · 8 phases · next: Phase 1. Panel: architect, backend, lang:rust, po, tui, unix (opus); codex: not run (programme ticket agent).
**Changed by review:** refusal scoped to routed reads (exit 3, hook never refused, doctor finding); registry flock CAS; conformance moved to Phase 2.
**Verdicts:** final: architect, backend approve; rust, po, tui, unix approve-with-changes; 9 carried; words 1195 → 1636 (×1.37, wrong-statement fixes); remove 0, merge 0.
📄 Full reviews: [2026-11-23-shared-state-trait.reviews.md](2026-11-23-shared-state-trait.reviews.md)

## Picture

```
 call sites (cli/ctx, runner/review, cli/programme, cli/run,
             cli/decide, runner/engine, cli/learn)
                         |
                         v
            shared_state::current()  <-- machine.toml shared_state
               |                    \
        "local" (default)         "external"
               |                       \
               v                        v
     +-------------------+    registered factory? --no--> ExternalAbsent
     |    LocalState     |          | yes                 (refuse to start)
     |  (public, MIT)    |          v
     +-------------------+     closed client (relais-fabric,
       |    |    |    |         private build, an-t13)
       v    v    v    v
  machine  Ledger  claim   Registry        conformance::run_all
  .toml    (per    store   (active.json)   runs against LocalState,
           node)   files                   the in-memory fake, and
                                           (privately) the closed client
```

Every routed call site asks `current()` for the implementation; the
default is `LocalState` over today's files, and `external` without a
registered closed client refuses to start.

## Goal

Programme agents-many-nodes, ticket an-t14 (map and ticket in the homelab
repository, `docs/programmes/agents-many-nodes/`). Build the public seam
an-t06 decided: a `SharedState` trait in the relais crate over an-t01's
single-owner operations, shaped by an-t10 (slice, degraded mode, publish
queue), an-t11 (an `epoch` in every lease token, `lease_lost` on fenced
calls) and an-t12 (a `holder` principal). Public relais ships `LocalState`,
today's behaviour behind the trait; the production call sites go through
it; a conformance suite runs against `LocalState` and an in-memory fake.
Nothing changes for a single-node user: the routed commands print the same
bytes and exit codes as before (see Verification). Who gains: the closed
`relais-fabric` build (an-t13) gets one seam to implement and one suite to
pass.

## Non-goals

- No closed code, no wire protocol, no primary service (an-t02, an-t13).
- No change to any existing test, to the admission `Gate` trait, or to the
  signatures of `Ledger::claim_intent`, `Ledger::resolve_decision_with_intent`,
  `Ledger::spend_since`, `SlugLock::claim_work` or `Registry::promote`:
  `LocalState` wraps them.
- No offline cache of `machine.toml`, no slice persistence: those belong to
  the external implementation (an-t10); `LocalState` is always online.
- The ledger stays per node (an-t01); nothing here merges ledgers.

## Code review

- relais: architect, backend, lang:rust (unix, tui left out: one new refusal line under a key nobody sets yet)
- homelab: none — markdown only (the ticket closure)

## Behaviour

**Module.** `crates/relais/src/shared_state/` exported as
`relais::shared_state`:

- `types.rs`: `Principal { kind: PrincipalKind (Operator | Node | Task), id }`
  (an-t12; distinct from `programme::claim::Holder`); `ResourceId`
  (`seat:<node>/<dispatch>`, `ticket:<repo-key>/<slug>/<id>`,
  `intent:<id>`, `decision:<run>`, `registry`); `LeaseToken { resource,
  holder, epoch: u64, expires_at: Option<i64> }` (an-t11); `Reservation {
  id, seat epoch, amount }`; `Slice { Unbounded | Granted { remaining,
  valid_until } }`; `Mode { Online | Degraded { reason, slice } }` (an-t10);
  `PublishRow { node, seq, table, payload }` and `PublishReceipt`.
- `error.rs`: `SharedStateError` with `LeaseLost { resource, epoch }`,
  `ClaimLost`, `CasConflict { expected, found }`, `Unreachable`,
  `SliceExhausted`, `Refused { reason }`, `ExternalAbsent` (the named start
  refusal of an-t06), and pass-through variants carrying today's local
  errors (`Ledger`, `Claim`, `Registry`, `Config`) so call sites keep their
  messages.
- `mod.rs`: the object-safe trait `SharedState: Send + Sync`, grouped:
  - config: `machine_settings()`;
  - spend: `daily_spent(&Ledger, day_start)`, `slice()`,
    `reserve_spend(&LeaseToken, amount)`, `settle_spend(&Reservation,
    actual)` (never refused for a stale epoch: a true-up, an-t11);
  - seats: `acquire_seat`, `renew`, `release_seat`;
  - claims: `claim_ticket`, `claim_intent(&Ledger, …)`,
    `release_claim(&Ledger, …)` (the intent release; conformance covers it),
    `resolve_decision_with_intent(&Ledger, …)` (one op: the answer and the
    intent stay one local transaction);
  - registry: `registry_pointer(&Registry)`, `swap_registry_pointer(&Registry,
    expected, &EvaluatedArtifact)` (compare-and-set on the active id,
    under a lock file in the registry directory that `LocalState` holds
    across the compare and `Registry::promote`, taken with `procs::LockFile`
    (`flock`, released by the kernel when the process dies; never deleted,
    so a killed promote leaves no stale lock));
  - schema and publish: `announce_schema(version)`, `publish(rows)`,
    `queued()`, `mode()`.
  Node-local handles (the ledger, the registry directory) are parameters:
  they stay per node; an external implementation adds the shared half.
- Selection: `machine.toml` gains `shared_state = "local" | "external"`
  (absent = local; not serialised when local, so the writers that rewrite
  the file keep it only when set). `shared_state::current()` reads only that
  key through a raw local parse (an unreadable or invalid file selects
  local and leaves the error to the read that follows, as today);
  `machine_settings()` runs after selection and never calls `current()`.
  For external it calls the factory held in a `OnceLock` that
  `shared_state::register_external(fn)` sets; no pre-main hook, no new
  dependency. `cli` is private to the binary, so how the private build's
  `main` reaches that call is an-t13's build plan (named as fog). With no
  factory: `SharedStateError::ExternalAbsent`.
- The refusal: `current()` is called only where a routed operation runs
  (inside `load_machine` and the other routed sites, inside `dispatch`, so
  `--protocol` still announces it). A command that never read machine.toml
  is never refused, and `relais hook` reads none of the routed paths, so it
  keeps exiting 0. New `CliError::SharedStateAbsent` maps to
  `CliOutcome::Blocked` (exit 3): one stderr line, `relais: machine.toml
  <path> sets shared_state = "external", but this relais build has no
  shared-state client; set shared_state = "local" or install the build that
  has one`, ASCII, no colour, nothing on stdout. `relais doctor` adds one
  failing finding with that text only when the key says external, so its
  output is unchanged otherwise.
- `local.rs`: `LocalState` resolves paths from the environment at each call
  exactly as today, delegates to the existing functions, keeps a
  process-wide epoch table (a static `OnceLock<Mutex<…>>`, since instances
  are built per call; in-process only, so cross-process fencing is the
  external implementation's) so `lease_lost` is reachable (only through a
  stale token, which no existing call site holds), grants seats, reports
  `Slice::Unbounded`, `Mode::Online`, a no-op publish with an increasing
  sequence and an empty queue.
- `fake.rs` and `conformance.rs` behind `cfg(any(test, feature =
  "conformance"))`: an in-memory fake with the external semantics (an
  injected clock, expiry, a bounded slice, a degraded flag, a queue,
  compare-and-set claims and pointer); `conformance::run_all(make)` checks
  the properties both must hold: epoch monotonic on acquire; a stale epoch
  on a fenced write gives `lease_lost`; release then re-acquire bumps the
  epoch; a late settle is never refused; a reserve beyond a granted slice is
  refused; a registry swap with a stale expectation is `CasConflict`; a
  second ticket claim by another holder is refused, by the same holder is a
  refresh; publish replay is idempotent by node and sequence. The closed
  repository runs the same suite through the `conformance` feature.

**Routed call sites (production only):**

- config: `cli/ctx.rs::load_machine` (its callers follow; the direct
  readers in `cli/ctx.rs` (`tuning_bounds` and two more),
  `cli/coordinator.rs`, `doctor/mod.rs`, `router/mod.rs`, `holdout.rs`,
  `runner/attempt.rs`, `cli/learn.rs`, `cli/native.rs` are narrowed: they
  still read the local file);
- spend: `runner/review/engine.rs::spent_today` (both ceiling checks);
- claims: `cli/programme.rs::claim_command` (programme ticket), the
  `claim_intent` and `release_claim` sites in `cli/run.rs`, the `resolve_decision_with_intent`
  sites in `runner/engine.rs`, `cli/run.rs`, `cli/decide.rs`;
- registry: `cli/learn.rs::promote_command`.

**Narrowed (named, not routed here)**, each kept on the trait, implemented
by `LocalState`, and covered by conformance: the coordinator's seats (its
grant and release live inside `AdmissionState` behind the `Gate` trait and
its tests; routing them needs a coordinator-held `SharedState`, a follow-up
in the primary build plan); the learning cycle's automatic promote and
rollback (`learn/cycle.rs` takes a `&Registry` its tests build); handoff
reservations and session refresh/end on programme claims (the hook path);
schema announce and publish (no caller exists today: nothing publishes).

## Phases

- [x] Phase 1 — types, errors, trait, selection and refusal:
  `crates/relais/src/shared_state/mod.rs`, `crates/relais/src/shared_state/types.rs`,
  `crates/relais/src/shared_state/error.rs`, `crates/relais/src/shared_state/local.rs`,
  `crates/relais/src/lib.rs`, `crates/relais/src/policy/mod.rs`,
  `crates/relais/src/cli/outcome.rs`, `crates/relais/src/doctor/mod.rs`,
  `crates/relais/Cargo.toml`
- [x] Phase 2 — fake and conformance suite: `crates/relais/src/shared_state/fake.rs`,
  `crates/relais/src/shared_state/conformance.rs`
- [x] Phase 3 — config reads routed: `crates/relais/src/cli/ctx.rs`
- [x] Phase 4 — narrowed, not routed (Decision log): spend routed: `crates/relais/src/runner/review/engine.rs`
- [x] Phase 5 — (the `runner/engine.rs` settles narrowed, Decision log) claims routed: `crates/relais/src/cli/programme.rs`,
  `crates/relais/src/cli/run.rs`, `crates/relais/src/cli/decide.rs`,
  `crates/relais/src/runner/engine.rs`
- [x] Phase 6 — registry routed: `crates/relais/src/cli/learn.rs`
- [x] Phase 7 — docs and changelog: `CHANGELOG.md`, `docs/shared-state.md`
- [x] Phase 8 — ticket closure in homelab (Resolution, `status = "closed"`,
  the Decisions line): `docs/programmes/agents-many-nodes/an-t14-shared-state-trait.md`,
  `docs/programmes/agents-many-nodes/map.md`

## Decision log

- 2026-11-23 — Node-local handles are parameters of the trait's methods,
  not fields of `LocalState`, because the ledger stays per node (an-t01)
  and `LocalState` built per call keeps tests that switch `HOME` per test
  correct.
- 2026-11-23 — The seat, cycle-promotion and handoff call sites are
  narrowed out (named above), because routing them changes signatures that
  existing tests build, and the ticket requires the suite unchanged.
- 2026-11-23 — `swap_registry_pointer` compares the active id read just
  before, under a registry lock file held across compare and promote
  (`Registry::promote` itself takes none). This differs from today only
  when two promotes race, where today's code silently lost one update: a
  deliberate fix. The narrowed `learn/cycle.rs` promote stays unfenced.
- 2026-11-23 — The trait stays one public trait with `&Ledger` and
  `&Registry` parameters (an-t06: public API, pinned by the closed repo);
  `docs/shared-state.md` marks it unstable until an-t13 consumes it, and an
  older relais refuses a machine.toml that sets the key
  (`deny_unknown_fields`), which the doc and CHANGELOG say.
- 2026-11-23 — Implementation review (round 1, `dependencies.explicit`):
  no global factory and no global epoch table. `register_external` and
  `current()` are replaced by `current_with(external)`, called only from
  the CLI's composition root (`cli/ctx.rs`, `EXTERNAL_CLIENT`, `None` in
  public relais: the line a private build replaces); `LocalState` holds its
  own epoch table; the doctor finding takes `client_linked` from the CLI.
  This supersedes the Behaviour's `register_external`, `OnceLock` and
  process-wide table wording, kept as reviewed.
- 2026-11-23 — Narrowed with it: the daily spend check
  (`runner/review/engine.rs`) and the runner's widen and timed-out settles
  (`runner/engine.rs`) stay on the ledger, because library code may not
  build a `SharedState` itself and `RunConfig` cannot gain a field without
  editing the runner tests that build it as a struct literal. holds-until:
  a change that may edit those tests gives `RunConfig` a `shared_state`
  field; until then the global cap (an-t10's slices) is not enforced
  through the seam, and the node's own `[spending]` ceiling applies as
  today.
- 2026-11-23 — The selector type lives in `policy` (`SharedStateSelection`,
  re-exported as `shared_state::Selection`): `policy` importing
  `shared_state` closed a module cycle the cycle check refused.

## Verification

Baseline first: before Phase 1, the `origin/main` binary's stdout, stderr
and exit code for `relais status`, `relais --help`, `relais doctor` and a
scratch `relais programme claim` are saved outside the worktree.

- Phase 1: scratch `XDG_CONFIG_HOME` with `shared_state = "external"`:
  `echo '{}' | relais hook` → exit 0; `relais doctor` → a failing finding
  naming the key; `relais trust grant` on that file → key kept → hook exit
  0; doctor exit 3 with one `✗ shared_state` line; under external every
  `load_machine` command refuses, so the key-kept check runs with
  `shared_state = "local"`: kept, and a file without the key gets none
  (also `tests/shared_state_cli.rs`)
- Phase 3: same scratch file: `relais trust list >out 2>err; echo $?`
  (`trust_command` reads through `load_machine` first, `cli/trust.rs:21`)
  → `trust show` (the subcommand is `show`, not `list`) and `programme
  claim`: exit 3, 0 bytes on stdout, one `relais:` line naming the file and
  the fix; `shared_state = "bogus"` → exit 2, `... is invalid: ...
  shared_state = "bogus": expected "local" or "external"`
- Phases 3–6: with no key, and with a mode-000 machine.toml (as a non-root
  user), the baseline
  commands → byte-identical output and exit codes → no key: `--help`,
  `status`, `trust show`, `programme claim`, `hook` byte-identical; `doctor`
  differs only by facts outside this change (ledger schema v48 → v49 from
  main's #546, the installed greengate version, other sessions' runs);
  mode-000: `trust show` gives the same `cannot read the machine settings`
  line and exit 2 from both binaries; `cargo test --workspace` (macOS) →
  2610 lib + 63 + 351 `it` + 16 + 16 + 3 new CLI tests pass; no existing
  test file modified (`git diff origin/main... --diff-filter=MD
  --name-only` lists no test file), and every removed line is in a
  production function
- Phase 2: `cargo test -p relais shared_state` → conformance passes for
  `LocalState` and the fake → 9 passed (both conform, three fake-only
  expiry and degraded tests, selector tests); `cargo clippy -p relais
  --features conformance --lib -D warnings` → clean
- Phase 6: two concurrent `LocalState` swaps from threads in a test → one
  succeeds, one `CasConflict` → passes (each thread opens its own registry
  and lock description)
- Linux and macOS: CI's cargo-test matrix runs on the PR.

## Implementation review

- implementation-review: rework → Delta approve-with-changes; fixed the two globals and six banners; deliberate: the reviewed Behaviour stays, the Decision log records the change. 91k+56k, 67+24 s.
- architect: approve-with-changes → Delta approve; fixed the runner locating its own service (reverted, narrowed); deliberate: node-local types (an-t06), text pre-filter (test literals). 57k+43k, 23+33 s.
- backend: approve-with-changes → Delta approve; fixed holds-until, CLI refusal test, promote conflict exit 3; global epoch table gone. 67k+45k, 84+30 s.
- lang:rust: approve-with-changes → Delta approve-with-changes; fixed MicroUsd, exhaustive source, Lock variant; deliberate: TicketGrant fields, String selector error, boxed cause, release_intent string. 67k+37k, 52+11 s.

## Outcome

Shipped the trait, `LocalState`, the selector and its refusal, the fake
and the conformance suite, and routing for config, ticket and intent
claims, `relais decide` and `relais promote`. Not shipped: spend and the
runner's settles through the seam (`RunConfig` first), coordinator seats,
the other direct machine-settings readers. Surprise: `policy` importing
`shared_state` closed a crate-wide module cycle.

<!-- panel: repos=relais,homelab adds=contract reviewers=architect,backend,lang:rust,po,tui,unix body-sha=c9cc3dc6c48f -->
