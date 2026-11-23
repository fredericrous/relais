# Shared state

relais keeps one ledger per node. A few things cannot be per node once
several nodes run work: the machine's trust and spending settings, the
daily spend cap, coordinator seats, the claims on programme tickets and
successor intents, decision answers, the learned registry's active pointer,
the schema version, and the append-only rows other nodes read. The
`relais::shared_state` module names those operations as one trait,
`SharedState`. Everything else stays on the node.

## Implementations

| `machine.toml` | Implementation | What it does |
|---|---|---|
| no `shared_state` key, or `shared_state = "local"` | `LocalState` | Today's behaviour: the file, then the node's ledger, claim files and the registry's pointer files decide (relais writes them in `shared_state::node`). Seats are always granted, the spend slice is unbounded, publish is a no-op and the node is always online. |
| `shared_state = "external"` | a closed client a private build registers | Talks to a shared primary. A public build has no client, so it refuses (below). |

`shared_state` is a top-level key. Any other value is an invalid
`machine.toml`. A relais older than the release that added the key refuses a
file that sets it (`deny_unknown_fields`), so remove the key before going
back to an older relais.

## Which commands go through it

| Operation | Where |
|---|---|
| machine settings | every command that reads them through the CLI's `load_machine` (`relais run`, `trust`, `plan`, `replay`, `learn`, ...) |
| programme ticket claim | `relais programme claim` |
| successor intent claim and release | `relais run --intent` |
| decision answer with a successor intent | `relais decide` with the revise answer |
| registry pointer read and swap | `relais promote` |

Not routed yet, so under `external` they still read or write the node's own
files: the daily spend ceiling check and the runner's widen and timed-out
settles (the runner would have to look the implementation up itself, since
`RunConfig` cannot gain a field without editing the tests that build it; it
gains one in a later change); the other direct readers of `machine.toml` (`tuning_bounds` and two
more in `cli/ctx.rs`, `cli/coordinator.rs`, `doctor/mod.rs`,
`router/mod.rs`, `holdout.rs`, `runner/attempt.rs`, `cli/learn.rs`,
`cli/native.rs`); the coordinator's own seats (`AdmissionState` behind the
`Gate` trait); the learning cycle's automatic promote and rollback
(`learn/cycle.rs`); handoff reservations and the session hooks' claim
refresh and end. `announce_schema`, `publish`, `queued`, `slice`, the seat
operations and `mode` have no caller yet: nothing publishes today.

## The refusal

With `shared_state = "external"` and no client in the build, the routed
commands print one line on stderr and exit 3 (`Blocked`), with nothing on
stdout:

```
relais: machine.toml <path> sets shared_state = "external", but this relais build has no shared-state client; set shared_state = "local" or install the build that has one
```

Under `relais run --protocol`, the `done` line names it `shared_state_absent`.
`relais hook` never reads a routed path, so it keeps exiting 0.
`relais doctor` adds a failing `shared_state` finding with the same text.

## For a closed client

- The trait is the shared half only, in plain values: no `Ledger`, claim
  store, registry, callback or borrowed request crosses it, so each call can
  go to a primary on another node as it is. relais keeps the node half
  (`relais::shared_state::node`): for each operation it asks the shared half
  first, then writes the node's ledger, claim file or pointer file, and gives
  the shared hold back once when that local step refuses. An operation with
  no shared half answers "the node decides" (`TicketAnswer::NodeDecides`,
  `Pointer::NodeDecides`, `Swap::NodeDecides`, `None` from `daily_spent`) or
  grants; that is all `LocalState` does. `node` is public only because the
  CLI is a separate binary target: a client implements the trait, never
  `node`.
- A failed give-back never hides the local answer: a refusal comes back as
  it is, with one `relais: the shared hold on … could not be given back`
  line on stderr, and a local error comes back as
  `NodeError::Compensation { local, release }`.
  A hold left behind ends by its TTL (tickets) or when a later claim
  replaces it (intents: `claim_intent(.., replacing)`, asked only when this
  node's own ledger saw the old run's claim and judged it abandoned). A
  `relais promote` killed between the shared swap and the local one leaves
  the shared pointer ahead of the node's; the next `relais promote` or
  rollback brings them back together.
- Implement `SharedState` and give its factory to the CLI: public relais
  builds the implementation in one place, `cli/ctx.rs`, from
  `EXTERNAL_CLIENT` (`None` here), and a private build replaces that one
  line. Library code never builds one itself.
- Run the suite: build `relais` with `--features conformance` and call
  `relais::shared_state::conformance::run_all(make)`. It checks what both a
  single node and a primary must hold: the epoch rises on every acquire, a
  stale epoch is `lease_lost`, a late settle is never refused, a reserve
  beyond a granted slice is refused, a ticket has one holder, an intent is
  claimed once, a registry swap compares first, publish replays
  idempotently by node and sequence, and a fresh state is online and empty.
  `relais::shared_state::fake::FakeState` is the in-memory reference with a
  primary's semantics (expiry, degraded mode, a queue).
- A failure of the node's own files comes back as `node::NodeError`
  (`Shared(SharedStateError)`, `Lock`, `Ledger`, `Claim`, `Registry`,
  `Compensation`); a client returns `SharedStateError` only.
- A decision grant is exclusive and lapses. While a grant is held,
  `answer_decision` answers `DecisionGrant::HeldElsewhere` to another ask;
  an unreleased grant lapses `DECISION_GRANT_TTL_SECS` (10 minutes) after it
  was given, so an answerer that died before its ledger write does not hold
  the decision for good. `LocalState` holds none: the node's ledger decides.
  The ledger fences a late answer only within one ledger, so a client with
  several answerers adds its own fence (an-t13 owns it).
  `conformance::run_all_with_clock(make, advance)` runs the lapse check; a
  client whose test primary has an injectable clock must pass it, and
  `run_all` alone does not cover the lapse.
- The trait is unstable: until a closed client consumes it, a change to it
  ships in a minor release, and the client pins the exact public version.
- `LocalState` fences seats and tickets within one instance only; fencing
  across processes and nodes is the external client's job.
- The trait still speaks a few relais value types (`MachineSettings`,
  `MicroUsd`, `RunId`, `Contest`), so a client builds against one exact
  relais version, as an-t13's build plan pins it.
