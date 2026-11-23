---
status: active
branch: prog/agents-many-nodes/an-t17-route-seats-through-shared-state
repos: [relais, homelab]
adrs: []
plan-id: p-01M85CQ14N25GQSSQ84EE284V0
---
# Coordinator seats through SharedState

## Review panel

👉 **Decide:** none — approve if mirroring seated agent dispatches into the shared half, local first, with a refused seat answered as a wait, is the shape.
📍 relais, homelab · 3 phases · next: Phase 1. Panel: architect, backend, lang:rust, po, tui, unix (opus); codex: not run (programme ticket agent).
**Changed by review:** syncs serialized with failures keyed by dispatch; holder from the host name; renew outside the book mutex.
**Verdicts:** 6 approve-with-changes (round 2, backend final); 14 carried; words 1457 → 1894 (×1.30, blocking and high exempt); remove 0, merge 0.
📄 Full reviews: [2026-11-24-seats-through-shared-state.reviews.md](2026-11-24-seats-through-shared-state.reviews.md)

## Picture

```
 runner / hook --socket--> coordinator handler thread
   1. lock admission; handle(request), pure and unchanged; unlock
   2. SeatBook::sync, admission unlocked (A6):
        settle journal -> settle_spend
        seat gone      -> release_seat
        seat new       -> acquire_seat, then reserve_spend
   3. reply was Granted and its seat or reservation failed:
        lock admission; still this grant's dispatch, unlaunched?
        unclaim -> Queued; slice exhausted: revoke -> Refused
   -> reply
 reconcile thread, every 15 s:
   renew_seat each lease (lease_lost -> lost set), then sync
 serve exit: release every lease
```

The admission state still decides who gets a seat on this node; the seat
book mirrors the seated agent dispatches into the shared half and turns a
refused shared seat into a wait.

## Goal

an-t14 left the coordinator's seats off the seam: they live in
`AdmissionState` behind the `Gate` trait. After this change a
coordinator holds one `Arc<dyn SharedState>`, built at the CLI's
composition root, and every agent seat it grants, renews and frees goes
through `acquire_seat`, `renew_seat` and `release_seat` with an-t11's
epoch. The seat's spend reservation rides with it (`reserve_spend` at
acquire, `settle_spend` when the dispatch settles), which an-t16 left to
this ticket so the global counter gets a writer. With `LocalState` every
call grants, so a single node sees no change.

## Non-goals

- Routing the daemon's own `machine.toml` read (limits, lease TTL, disk
  floor) through `machine_settings`: an-t14 narrowed it, and it is not a
  seat.
- Shared seats for local classes (`heavy_local`, `indexing`, `training`):
  they bound this machine's CPU and disk, not the shared model budget;
  greengate's slots stay a separate layer (an-t11).
- an-t11's node-level lease over all of a node's seats: the trait is per
  seat; a closed client may batch renewals behind it (fog, below).
- Carrying the lease token to the runner, or a new wire variant: the
  runner keeps speaking `Decision` over the socket.
- Changing the trait or `LocalState`'s semantics.

## Code review

- relais: backend, architect, lang:rust, tui (the coordinator's new stderr lines; unix left out: no new command, flag or exit code)
- homelab: none — markdown only (the ticket closure)

## Behaviour

**Where the SharedState sits.** `relais coordinator daemon` calls
`ctx::shared_state()?` (so `external` without a client refuses with exit
3, as every routed command does) and hands it to `run_daemon`, then to
`Coordinator::start(…, shared, holder)`. The coordinator owns a
`SeatBook` (`crates/relais/src/coordinator/seats.rs`): the shared state,
the holder (`Principal::node(<this host's name>)`, `gethostname` on Unix,
`COMPUTERNAME` on Windows, `local` when neither answers; never a constant,
so two nodes never share a holder, and a restart on one host re-takes its
own seats with a higher epoch; the closed client keys the real holder by
its authenticated principal, an-t12) and, under one mutex,
the lease per dispatch, the reservation per dispatch, the lost set and
settles waiting for a reachable primary. The pure `handle()` and the `Gate`
trait do not change; tests pass `Arc::new(LocalState::new())`.

**What a seat is.** A `model_work` dispatch whose lifecycle holds a seat
(`Unclaimed`, `Claimed`, `UsageSettled`; `Lifecycle::holds_seat`).
`AdmissionState::shared_seats()` lists them with their `reserve_micros`
and whether a caller was told to launch. A diff of that set against the
book catches every path a seat changes on (grant, drain from the queue,
`Waiting` and resume, release, cancel acknowledge, `settle_by_agent`,
reconcile expiry) without touching each path.

**Sync** (`SeatBook::sync(&Mutex<AdmissionState>)`, after every served
request but `Ping`, `Status` and `Shutdown`, and on every reconcile tick).
One sync runs at a time: the book mutex is held across the whole sync,
shared calls included, and never together with the admission lock (book
first, admission inside, briefly). A failed acquire or reserve is recorded
in the book by dispatch id, whichever thread made the call, and the handler
that granted that id consumes it. A two-thread test asserts one acquire
per dispatch. Renew does not hold the book mutex across its calls: it copies the tokens under the mutex, renews them without it, and applies each result under the mutex only when the book still holds that token, so a slow primary never stalls the handlers for a renew pass. The trait has no timeout: a closed client bounds each call
so a renew pass of N leases fits the 15 s tick.
1. Under the admission lock, briefly: read `shared_seats()`, the ids still
   in `dispatches`, and take the spend journal; then unlock. No shared
   call is made under the admission lock (A6).
2. Each journal entry (`settle()` pushes the dispatch and what it booked:
   the spend, or the reservation when usage is unknown) is settled with
   `settle_spend` on its reservation, then the reservation is dropped. A
   settle is never refused for a stale seat (`TrueUp`); an unreachable
   primary keeps it waiting for the next sync.
3. A lease whose dispatch no longer holds a seat is released (a failure
   is one stderr line, token omitted, and the token is dropped: its TTL ends it). A lost
   dispatch that left the seated set leaves the lost set.
4. A seated dispatch with no lease and not lost is acquired, then its
   `reserve_micros` reserved unless the book already holds a reservation
   for it (a resumed `Waiting` parent keeps the one it had). When the dispatch is not yet launched
   (`Unclaimed`, or the one this reply grants), a failure of either
   releases the shared seat only (the local seat stays, see below) and is recorded for the granting handler; when it is
   already running (resumed, adopted at start), the work goes on, a failed
   acquire is retried at the next sync and a failed reserve is one log
   line (the node's ledger still books the spend; an-t16's larger-of rule
   keeps it the floor).
5. A reservation whose dispatch is gone from `dispatches` without a settle
   (withdrawn, discarded unclaimed) is settled at zero.

**Why local first, then shared.** an-t15's node functions ask the shared
half first. Seats go the other way: the admission queue (caps, fairness,
hard limits, disk) decides whether this node wants a seat at all, and
asking the primary first would hold cluster seats for requests that then
queue here. The undo is local and in memory, so it cannot fail the way
an-t15's local writes can.

**The answer when the shared seat is refused.** When `handle()` answered
`Granted` to an admission request and the book holds a failure for that
dispatch id, the coordinator relocks admission and acts only when the
dispatch is still the one this reply granted and has not launched (no
binding); otherwise the reply stays the local outcome:
- seat refused, primary unreachable, or reserve refused for any reason
  but the slice: `AdmissionState::unclaim` turns it back to `Unclaimed`
  (it keeps its local seat and stays withdrawable, its heartbeat restarts
  `UNCLAIMED_GRACE`), and the reply is `Queued { position: 1 }`. The
  runner and the hook already poll a queued answer, bounded by the
  admission wait budget, and withdraw on exhaustion. The next poll is
  `Granted` locally and asks the primary again.
- the slice cannot cover the reservation (`SliceExhausted`):
  `AdmissionState::revoke_grant` gives the seat and the reservation back
  without marking the id finished, and the reply is `Refused {
  BudgetExceeded }` with detail `shared spend slice exhausted: reservation <n> micros, slice left <m> micros`, which the runner stops on as a reached limit.
`Response::Error` is not used: the client reads it as a protocol
disagreement, not an outage. Because `Queued` carries no reason, the
coordinator log (stderr) gets one line when it turns a grant into a wait:
`seat <dispatch>: shared seat <refused: reason | primary unreachable>;
queued until the shared state grants it`.

**Renew** (reconcile thread, every `RECONCILE_EVERY` = 15 s, an-t11's
renew interval): `renew_seat` on each lease. `LeaseLost` drops the token
and puts the dispatch in the lost set, which sync never re-acquires (a
re-acquire would bump the epoch and defeat the fence); the work in flight
finishes, its reservation stays and settles as a true-up. Any other error
keeps the token for the next tick. Renew, release, reserve and settle
failures write one coordinator stderr line per change of state
(`seat <dispatch>: <op> failed: <error>`, then `seat <dispatch>: <op>
recovered`), never one per tick, and never the token.

**Shutdown.** When `serve` returns (`Shutdown` or the idle exit), every
lease left is released, best effort; a killed daemon's leases end at
their TTL. Settles waiting for the primary are in memory and lost on a
restart (an-t16's larger-of floor bounds the undercount); an unsettled
reservation ends with its seat's lease, which the closed client honours.

**LocalState.** Seats are always granted, a reservation fences within the
instance, a settle books: every reply is today's. Its epoch table keeps a
row per dispatch for the daemon's life (accepted).

**Tests.** `seats.rs` unit tests with `FakeState`: acquire on grant,
release when a dispatch settles and frees its seat, the journal settles
the booked amount; degraded primary turns a grant into `Queued` and the
next poll after recovery is `Granted`; a slice smaller than the
reservation gives `Refused { BudgetExceeded }` and the same id may be
asked again; a renew that finds the lease lost is not re-acquired; a
withdrawn unclaimed dispatch settles its reservation at zero; a
restarted coordinator re-takes an adopted dispatch's seat under the same
holder with a higher epoch. One socket test through `Coordinator::start`
and `RemoteGate` with a degraded fake answers `Queued`. 

## Phases

- [ ] Phase 1 — seat book, admission hooks, coordinator wiring and tests:
  `crates/relais/src/coordinator/seats.rs`,
  `crates/relais/src/coordinator/mod.rs`,
  `crates/relais/src/admission/mod.rs`,
  `crates/relais/src/cli/coordinator.rs`,
  `crates/relais/src/shared_state/mod.rs`
- [ ] Phase 2 — docs: `docs/shared-state.md` (move seats from "not routed"
  to routed, with the answers above), `CHANGELOG.md`
- [ ] Phase 3 — homelab: the an-t17 Resolution and its Decisions-so-far
  line: `docs/programmes/agents-many-nodes/an-t17-route-seats-through-shared-state.md`,
  `docs/programmes/agents-many-nodes/map.md`

### 🧑 Your steps

## Decision log

- 2026-11-24: local first, then shared, for seats (the queue decides
  whether a seat is wanted); a refused shared seat is a wait (`Queued`),
  an exhausted slice is a budget refusal; no new wire variant.
- 2026-11-24: the reservation rides with the seat in this ticket (an-t16's
  Resolution leaves the global counter's writer to an-t17).
- 2026-11-24: the fleet pack refresh rides this branch.

## Verification

| Check | Input | Expected | Actual |
|---|---|---|---|
| seat tests | `cargo test -p relais --lib coordinator::seats` | the fake tests above pass | |
| conformance | `cargo test -p relais --lib --features conformance shared_state::` | unchanged, pass | |
| suite | `cargo test --workspace` | passes with no existing test edited beyond the `Coordinator::start` argument | |
| piloted daemon | branch binary, scratch `RELAIS_STATE_DIR`: `relais coordinator daemon` with `shared_state` absent, then `= "external"` | absent: serves, `relais coordinator status` as on `main`; external: one stderr line, empty stdout, exit 3, no socket created (check before bind) | |
| lint | `cargo clippy -p relais --all-targets --features conformance -- -D warnings` | clean | |
| counter writer | `FakeState` test: one grant, then settle | shared counter shows the reservation after grant and the settled amount after settle | |
| socket downgrade | `cargo test -p relais --lib coordinator::` socket test with a degraded `FakeState` | the admission reply is `Queued { position: 1 }` with the stderr line; after recovery the next poll is `Granted` | |

## Outcome

Measure after release: shared-refusal `Queued` count against grants (any on a single node is a rollback); single-node request latency p95 (a rise above 20% is a rollback); shared counter against node ledger after settle.

<!-- panel: repos=relais,homelab adds=contract reviewers=architect,backend,lang:rust,po,tui,unix body-sha=2c32f0c49652 -->
