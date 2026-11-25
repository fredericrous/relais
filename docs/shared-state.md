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
| machine settings | every command that reads them through the CLI's `load_machine` (`relais run`, `trust`, `plan`, `replay`, `learn`, ...), the recipe tuning bounds, the `[pricing]` table, the hook's admission settings, `relais native router-state` and `router-observe` |
| programme ticket claim | `relais programme claim` |
| handoff reservation | `relais programme next --claim` |
| a session's work holds renewed or given back | `relais programme claim` (renew), the hook on a resumed `SessionStart` (renew) and on `SessionEnd` (give back) |
| successor intent claim and release | `relais run --intent` |
| decision answer with a successor intent | `relais decide` with the revise answer |
| registry pointer read, swap and rollback | `relais promote`, `relais rollback`, and `relais learn cycle`'s automatic promote and rollback |
| daily spend ceiling check | `relais run`, `relais dataset replay` (through `RunConfig::shared_state`) |
| the runner's widen and timed-out settles | `relais run`, `relais dataset replay` |
| coordinator seats and their spend reservations | `relais coordinator daemon` (below) |

The ceiling check reads the larger of the global counter and this node's
ledger. The coordinator's settles feed the counter (below), but some spend
still reaches only the ledger (the runner's own settles, a dispatch whose
reserve failed after launch, settles lost on a restart), so the ledger stays
the floor until all of it reaches the counter. A shared read that fails stops
the run before dispatch.

Not routed yet, so under `external` they still read or write the node's own
files: the daemon's read of its limits in `cli/coordinator.rs`; `relais
doctor`, which checks the file itself; and every write
of `machine.toml` (`relais trust grant`, the learning envelope, the routing
envelope and its off switch), since the trait has no settings write yet.
`runner/attempt.rs`, `holdout.rs`, `cli/recipe.rs` and the pricing
preflight only name the file in a message. `announce_schema`, `publish`,
`queued` and `mode` have no caller yet: nothing publishes today.

Every write of the registry's pointer files goes through
`node::swap_registry_pointer` or `node::roll_back_registry`, under one
flock beside the pointers (`.pointer.lock`): it is taken before the
pointers are read and held across the shared compare-and-set and the local
write, so a promote and a rollback judged against the same artifact cannot
both land. The wait is bounded (2 s); a lock still held is refused, and
nothing changes. A shared pointer that names an artifact this node's
pointer does not (a promote that stopped between its two halves) is made
active here first, with one stderr note, when the node holds that artifact
and its evidence checks; otherwise the operation is refused with a sentence
that names it and says why (exit 3 from `relais promote` and `relais
rollback`, a note in the learning cycle): nothing run on this node moves
past it until the artifact is stored here or another node moves the
shared pointer. A failure to read the node's own artifact files is that
error, not a refusal.

## Coordinator seats

The coordinator daemon builds the implementation once at start (an
`external` without a client refuses it with exit 3 before anything is
bound) and holds it in a seat book. The admission queue still decides who
gets a seat on this node; the book mirrors that decision into the shared
state:

- An agent dispatch (`model_work`) that holds a seat holds a shared seat
  (`acquire_seat`, every acquire bumps the epoch) and a reservation of its
  `reserve_micros` under it (`reserve_spend`). Local classes (heavy checks,
  indexing, training) bound this machine only and take no shared seat.
- The book follows the admission state after every request that can change
  it and on every reconcile tick (15 s): a freed seat is released, a settle
  is booked (`settle_spend`, at the spend reported, or at the reservation
  when usage is unknown), a dispatch that leaves without a settle (withdrawn,
  never claimed) settles its reservation at zero. A request takes only
  the seat it asks for; other new seats (a dispatch drained from the
  queue, an adopted one) are taken at the next tick or at their own poll.
  Once a pass finds the primary unreachable it makes no more shared calls:
  the waiting settles, the freed seats not yet tried (which keep their
  tokens) and the new seats are tried again at the next sync, so a request
  served while the primary is down pays for one failed call, not one per
  seat.
- Each tick renews every held seat. A seat found `lease_lost` is not taken
  again: its work in flight finishes and its reservation settles as a
  true-up.
- A grant whose shared seat or reservation is refused, or whose primary is
  unreachable, is answered `Queued`: the runner and the hook poll it within
  their admission wait, and each poll asks the shared state again. A
  reservation the node's slice cannot cover is answered `Refused
  (budget_exceeded)`, `this node's share of the shared budget is used up:
  this run needs $<n>, $<m> is left; raise this node's share on the shared
  primary`. A dispatch already running keeps its seat when its
  reservation fails; its spend is then booked on the node only.
- The daemon's stderr carries one line per change, never the lease token:
  `seat <dispatch>: the shared state did not grant the seat (<cause>); this
  run waits and asks again; if it lasts, check the shared primary`, then
  `seat <dispatch>: shared seat granted`; `renew`, `release`, `reserve` and
  `settle` failures likewise. A settle the primary refuses (`Refused`) is
  dropped with one line: the shared budget does not count it, the node's
  ledger does. Any other settle failure is kept and retried.
- The holder is `Principal::node(<host name>)`, so two nodes never share
  one; under `external` a host with no name is refused (exit 3, with the
  command that sets one). A restarted
  daemon on the same host takes its adopted seats again at a higher epoch,
  which fences the dead daemon's tokens.
- When the daemon stops (`relais coordinator stop`, the idle exit) every
  seat goes back; a killed daemon's seats end at their TTL. Settles that
  were waiting for an unreachable primary are in memory and are lost on a
  restart; the node's ledger still has the spend.
- `LocalState` grants every seat and reservation and books every settle, so
  a single node answers what it answered before.

## The refusal

With `shared_state = "external"` and no client in the build, the routed
commands print one line on stderr and exit 3 (`Blocked`), with nothing on
stdout:

```
relais: machine.toml <path> sets shared_state = "external", but this relais build has no shared-state client; set shared_state = "local" or install the build that has one
```

Under `relais run --protocol`, the `done` line names it `shared_state_absent`.
`relais hook` keeps exiting 0 with the same answer: its claim passes run
on the claim files alone, and a resumed `SessionStart` or a `SessionEnd`
adds one stderr line, `relais: shared claims skipped, the local claims were
updated: ` followed by the line above. Its admission settings are read from
`machine.toml` as before. With a client, the hook waits at most 1 s for
the shared half of a claim pass, then goes on with the claim files; a hold
it could not renew or give back ends by its TTL.
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
  promote killed between the shared swap and the local one leaves the
  shared pointer ahead of the node's; the next promote or rollback on that
  node completes it (above).
- Implement `SharedState` and give its factory to the CLI: public relais
  builds the implementation in one place, `cli/ctx.rs`, from
  `EXTERNAL_CLIENT` (`None` here), and a private build replaces that one
  line. Library code never builds one itself.
- Run the suite: build `relais` with `--features conformance` and call
  `relais::shared_state::conformance::run_all(make)`. It checks what both a
  single node and a primary must hold: the epoch rises on every acquire, a
  stale epoch is `lease_lost`, a late settle is never refused, a reserve
  beyond a granted slice is refused, a ticket has one holder, an intent is
  claimed once, a registry swap and a rollback compare first, a session's
  end gives back its work holds and keeps its handoff reservations (and a
  reservation is never made over a held ticket), a handoff reservation
  retried by its own holder is reserved again while another holder's is
  refused, publish replays idempotently by node and sequence, and a fresh
  state is online and empty. A primary runs
  `run_all_with_clock(make, advance)` instead: it adds the checks only a
  primary holds to (a decision grant lapses; a primary refuses another
  holder's reservation with `Held`, naming the reserver).
- The programme claims have three session-wide operations beside the
  per-ticket ones: `reserve_handoff` (a `handoff` hold for the next
  session, `HANDOFF_HOLD_SECS`, taken over by any session's `work` claim;
  a retry by its own holder, a `next --claim` whose claim file was never
  written, is answered again, and a primary should move the hold's end to
  `HANDOFF_HOLD_SECS` after the retry (the fake does; the suite does not
  check it yet); another holder is refused while it lasts, with `Held` from a
  primary or by the claim file under `LocalState`),
  `renew_tickets` and `release_tickets` (every `work` hold of one holder,
  for a resumed session and a session end). The hook calls them with no
  lease in hand, so they are keyed by the holder, never fenced, and never
  refused for a hold already gone.
- The hook reads the admission settings and runs its claim passes through
  the client on every event it handles; each call it makes waits at most
  1 s (`SHARED_CLAIM_DEADLINE`), then goes on with the defaults or the
  claim files. A client that answers slowly costs every tool call that
  long, so it answers `machine_settings` from its cache (an-t10).
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
- Seats: the coordinator calls each seat method from one thread at a time,
  except `renew_seat`, which runs without the book's lock; the trait has no
  timeout, so the client bounds each call (a renew pass over every held seat
  must fit the 15 s tick, inside the 90 s lease). An unsettled reservation
  must end with its seat's lease on the primary (an-t11): a daemon that dies
  before settling leaves it behind, and its successor reserves again.
- A command builds its state once and hands that instance to every step:
  `relais run` reads the machine settings and prices, claims and releases
  a successor intent, and runs the worker through the one it built, so a
  client that keeps per-instance state releases what it granted.
- The trait still speaks a few relais value types (`MachineSettings`,
  `MicroUsd`, `RunId`, `Contest`), so a client builds against one exact
  relais version, as an-t13's build plan pins it.
