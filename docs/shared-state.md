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
| no `shared_state` key, or `shared_state = "local"` | `LocalState` | Today's behaviour: the file, the node's ledger, the claim store, the registry's pointer files. Seats are always granted, the spend slice is unbounded, publish is a no-op and the node is always online. |
| `shared_state = "external"` | a closed client a private build registers | Talks to a shared primary. A public build has no client, so it refuses (below). |

`shared_state` is a top-level key. Any other value is an invalid
`machine.toml`. A relais older than the release that added the key refuses a
file that sets it (`deny_unknown_fields`), so remove the key before going
back to an older relais.

## Which commands go through it

| Operation | Where |
|---|---|
| machine settings | every command that reads them through the CLI's `load_machine` (`relais run`, `trust`, `plan`, `replay`, `learn`, ...) |
| daily spend | the per-day ceiling check before a dispatch and before a review |
| programme ticket claim | `relais programme claim` |
| successor intent claim and release | `relais run --intent` |
| decision answer with a successor intent | `relais decide` with the revise answer, and the runner's widen and timed-out settles |
| registry pointer read and swap | `relais promote` |

Not routed yet, so under `external` they still read or write the node's own
files: the other direct readers of `machine.toml` (`tuning_bounds` and two
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

- Implement `SharedState` and register its factory with
  `relais::shared_state::register_external` before any command runs. The
  CLI is private to the `relais` binary, so how a private build's `main`
  reaches that call is the private build plan's to settle.
- Run the suite: build `relais` with `--features conformance` and call
  `relais::shared_state::conformance::run_all(make)`. It checks what both a
  single node and a primary must hold: the epoch rises on every acquire, a
  stale epoch is `lease_lost`, a late settle is never refused, a reserve
  beyond a granted slice is refused, a ticket has one holder, an intent is
  claimed once, a registry swap compares first, publish replays
  idempotently by node and sequence, and a fresh state is online and empty.
  `relais::shared_state::fake::FakeState` is the in-memory reference with a
  primary's semantics (expiry, degraded mode, a queue).
- The trait is unstable: until a closed client consumes it, a change to it
  ships in a minor release, and the client pins the exact public version.
- `LocalState` fences seats and tickets with a process-wide epoch table
  only; fencing across processes and nodes is the external client's job.
