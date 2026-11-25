---
status: active
branch: prog/agents-many-nodes/an-t24-feed-global-counter-fully
repos: [relais, homelab]
adrs: []
plan-id: p-01M89S0GMG2AE41ZPDYB5ASYJ0
---
# Feed the global counter from every settle

## Review panel

👉 **Decide:** none — approve if the counter alone being the day, with each leftover gap named, beats keeping `max(global, local)`.
📍 relais, homelab · 3 phases · 2 PRs · next: Phase 1. Panel: architect, backend, lang:rust, po, tui, unix (opus); codex: not run (programme ticket agent).
**Changed by review:** no ledger writer feeds the counter twice (Gap 5 made deliberate); settles persist before they are sent; `adds=contract` for the conformance case.
**Verdicts:** 3 approve, 3 approve-with-changes (round 2, backend final); 7 carried; words 983 → 1650 (×1.68, blocking and high exempt, mediums named in the reviews); remove 0, merge 0.
📄 Full reviews: [2026-11-25-feed-global-counter.reviews.md](2026-11-25-feed-global-counter.reviews.md)

## Picture

```
 coordinator (seat book)                  runner, no daemon (gate None)
   settle journal ----+                     book_usage
   reserve failed ----+--> PendingSettle      |
   restart: file -----+    {reservation|     v
                           none, actual,   node::book_unmanaged_spend
                           since}            acquire settle:<dispatch>
   sync: persist if journal added;           -> reserve 0 -> settle
         flush; persist if changed           -> release
   flush: settle_spend                       failure: event + stderr
     none: reserve 0 on the book's lease     (with a gate the coordinator
           or a settle:<dispatch> seat,       books it; a failed
           store + persist, then settle       gate.settle expires at the
   kept past 24 h / 500: dropped, 1 line      reservation)
   file: seat-settles.json (0600) beside the ledger
 node::daily_spent: Some(global) -> global alone; None -> the ledger
```

Every settle, queued or runner-made, ends at the same counter, and the ledger no longer props it up.

## Goal

an-t17 fed the global counter from the coordinator's settles, but spend
still reached only the ledger when the runner ran with no daemon, when a
reserve failed after launch, when waiting settles died with a restarted
coordinator, and when the primary refused a settle. This change closes each
(or names it deliberate), caps settles that never clear, and drops the
ledger floor in `node::daily_spent`, so the global counter alone is the day
wherever one exists.

## Non-goals

- Changing the `SharedState` trait or `LocalState` (it keeps no counter:
  `daily_spent` stays `None` and the ledger is the day).
- Persisting reservations or leases of running dispatches across a restart
  (an adopted dispatch reserves again; the old reservation ends with its
  lease on the primary).
- Spend booked before the counter existed: the day starts at the cutover.
- Queueing a runner's own settle when the primary is unreachable (below).

## Code review

- relais: backend, lang:rust, unix, tui (new state file, new coordinator and runner stderr lines)
- homelab: none — markdown only
- relais-fabric: none — not touched here; it runs the new conformance case at its next pin bump

## Behaviour

**Pending settle.** The seat book's waiting list holds `PendingSettle {
dispatch, reservation: Option<Reservation>, actual, since, tries }`. Flushing
one: with a reservation, `settle_spend` on it (idempotent by reservation id).
Without one (the dispatch never got a reservation; one that booked 0 is
skipped, there is nothing to count), reserve `0` under the dispatch's lease
when the book holds one (never re-acquiring the dispatch's own seat, which
bumps its epoch and fences the book's token, an-t11), else under a seat on
resource `settle:<dispatch>` acquired for the purpose and released after
(a failed acquire keeps the entry; a `LeaseLost` on a stale lease retries on
a fresh `settle:` seat: the work is over, so the fence protects nothing).
The new reservation is stored in the entry and the file is persisted before
`settle_spend` at `actual` is sent, so a settle whose answer was lost, or
a restart, retries the same reservation id and never books twice. Reserving
zero cannot exhaust the slice, and a settle is a true-up, so the counter
takes the full amount. An outage keeps the entry and stops the pass.

**Order of a sync.** Journal entries move into the list, the file is
persisted if any were added, the list is flushed, the file is persisted again if it changed; a sync that changes nothing writes nothing (a sync runs after every request, and each write fsyncs the directory under the book lock).
A crash between taking the journal and the first write is the one window
left (spend of a dispatch settled in that instant: deliberate, the file
cannot protect what was never written).

**Gap 3, reserve failed after launch.** A journal entry for a dispatch with
no reservation is queued unreserved. A launched dispatch whose acquire failed
likewise. The reserve-failed line becomes `seat {id}: reserve failed:
{error}; the spend is booked to the shared counter when it settles`.

**Gap 1, restart.** The list is written to `seat-settles.json` beside the
ledger with `fsutil::write_atomic_with_mode(.., Some(0o600))` (owner-only,
it holds seat tokens; directory fsynced), and read by `Coordinator::start`
(missing is empty). An unreadable file is renamed to
`seat-settles.json.unreadable-<unix-secs>`, one line names that path and
the cause, the list starts empty, and the daemon still starts. The file holds
plain fields through private serde types in `seats.rs`, so no public type
gains a derive. A coordinator built with no ledger persists nothing.

**Gap 4a, kept settles that never clear.** Every kept settle counts its
tries; `since` is unix seconds from a wall clock injected into `SeatBook`
(`with_clock`; production the system clock, tests a scripted one), never read
ambiently in `seats.rs` (effects.no-ambient-access). One older than
`SETTLE_KEEP_SECS` (24 h: the counter is a UTC day, older spend belongs to
another day) or beyond the newest `MAX_WAITING_SETTLES` (500) is dropped.
A pass writes one line for everything it dropped: the count, the total in
dollars, the oldest dispatch, "not booked to the shared counter; check the
shared primary". Every new line goes through the book's injected `log`.

**Gap 4b, a refused settle.** Deliberate: dropped as today, since the
primary answered and a retry repeats the refusal. The line says the shared
counter will not count it (the ledger floor is gone) and to check the
primary. decided-by: agent (ADR-0080).

**Gap 2, the runner with no daemon.** `Engine::book_usage`, when
`config.gate` is `None` (the only unmanaged launch is the `else` branch in
`managed_launch`), calls `node::book_unmanaged_spend(shared, holder,
dispatch, cost)` with `config.shared_state`: acquire a seat on
`settle:<dispatch>`, reserve 0, settle at the cost, release. Holder:
`Principal::node(host name)`, `local` when none. With a gate the coordinator
books the settle, so this never runs twice. Unknown cost books nothing. The
run never fails on it: an error is a `Decision` event
(`shared_spend_not_booked`) and one stderr line: `dispatch {id}: {cost} was
not booked to the shared counter: {error}; this node's ledger has it; check
the shared primary`. Deliberate: not queued (a run is short-lived and a queue
file shared between processes needs a lock for one rare case), and a refused
acquire (the node at its seat cap on the primary) is reported the same way,
not retried. decided-by: agent (ADR-0080).

**Gap 5, a gate settle that failed: deliberate.** `launch.rs` ignores
`gate.settle`'s result. The dispatch then stays on the coordinator, whose
lease expiry settles it unknown (`settle(id, None)`), which the journal books
at the reservation, a lower bound. The counter holds the reserve instead of
the cost, not nothing, and booking it again from the runner would count it
twice. decided-by: agent (ADR-0080).

**Writers of ledger spend.** Every one reaches the counter: the runner's
`book_usage` (a coordinator settle of the dispatch total, or Gap 2), the
per-turn bookings of `relais native usage` (inside that dispatch total) and
hook-admitted agents (`settle_by_agent`, journalled). Deliberate gap: a hook
that carried on with no daemon (`on_coordinator_unreachable = carry_on`) and
its `relais native usage` turns reach only the ledger. `seed` is not spend.
The counter's amounts are what dispatches settled (the reservation when
unknown), the ledger's are actuals: the day's cap reads the counter's basis
once the floor goes.

**Conformance.** One new case, `a_settle_may_exceed_its_reservation`: reserve
0, settle 500, never refused. It holds a closed client to what the runner
path relies on; relais-fabric's run of it is owed at its next pin bump.

**The floor.** `node::daily_spent` returns the global count when the state
keeps one and the ledger only when it does not. Its doc and the
`holds-until` comment go.

**Tests.** `seats/tests.rs`: a reserve that failed after launch still books
at settle (and the reserve-failed line no longer says "this node only");
a restarted book (new `SeatBook`, same file) settles what the old one left
waiting; an unreserved settle whose first settle answers `Unreachable`
books once, not twice; a settle past the age cap (scripted clock) is dropped
with one line; 500+ dropped in one pass write one line with a `$` total; a
refused settle is dropped with the new line; an unreadable file starts empty,
is kept aside and named in the line; the file mode is 0600. `node.rs`: the
counter alone is the day; `book_unmanaged_spend` leaves the counter at the
cost and the slice reduced by it, books nothing for an unknown cost, and
reports a failing primary and a refused acquire. A runner test: no gate
feeds the counter, a gate does not. A coordinator test for Gap 5: a
dispatch released but never settled is settled unknown by the reconcile pass
and the counter rises by its reservation exactly once.

## Phases

- [x] Phase 1 — pending settles, persistence, caps, unreserved booking:
  `crates/relais/src/coordinator/seats.rs`,
  `crates/relais/src/coordinator/seats/tests.rs`,
  `crates/relais/src/coordinator/mod.rs`
- [x] Phase 2 — runner settle, the floor, conformance, docs and changelog:
  `crates/relais/src/shared_state/node.rs`,
  `crates/relais/src/shared_state/conformance.rs`,
  `crates/relais/src/runner/launch.rs`,
  `crates/relais/src/runner/review/engine.rs`,
  `docs/shared-state.md`, `CHANGELOG.md`
- [ ] Phase 3 — homelab: the an-t24 Resolution and Decisions-so-far line:
  `docs/programmes/agents-many-nodes/an-t24-feed-global-counter-fully.md`,
  `docs/programmes/agents-many-nodes/map.md`

## Verification

- Input: a fake state with a counter, a daemon-less run booking 120 micros
  → expected: counter 120, slice down 120, no ledger floor involved →
  observed: counter 120 and slice down 120
  (`a_run_with_no_daemon_books_its_cost_to_the_counter_and_the_slice`), and a
  mock-backend run leaves counter = ledger
  (`a_run_with_no_gate_books_its_cost_to_the_shared_counter`).
- Input: an unreserved settle whose first settle failed, the primary then
  applying it → expected: the counter rises by `actual` once → observed: 400,
  not 800
  (`an_unreserved_settle_whose_first_settle_failed_books_once_on_the_same_reservation`).
- Input: book A leaves 3 settles waiting, book B opens the same file →
  expected: counter rises by their sum, file empty → observed: 60, no file,
  mode 0600 (`a_restarted_book_settles_what_the_last_one_left_waiting`).
- Input: a dispatch released and never settled → expected: the reconcile
  pass settles it and the counter holds its reservation once → observed: 1000
  once (`a_dispatch_released_but_never_settled_books_its_reservation_once`).
- Input: a run under a gate → expected: the runner books nothing → observed:
  counter 0 (`a_run_under_a_gate_leaves_the_counter_to_the_coordinator`).

- Input: the whole crate's tests and CI's clippy on the pushed tree →
  expected: green → observed: the commit gate's `cargo test -p relais`
  (lib 2739, bin 352, integration targets; the commit gate of f1ceaa9f8, the merge with main) passes; `cargo clippy
  --all-targets -- -D warnings` clean, also with `--features conformance`.

## Implementation review

- Round 1 (tree 634a2558): implementation-review and the four profiles returned approve-with-changes; five blockers and the medium findings fixed (Pass enum, release through `Book::release`, test discards, no real wait, names, typed file errors, set-aside of unreadable files).
- Delta 1 (tree 5e439311): rust and unix approve; implementation-review blocking on a `ResourceId` wildcard, fixed; backend and tui lows fixed.
- Delta 2 (tree c2a2567c, after the merge with main): tui and backend approve; implementation-review approve-with-changes: the rename-fails test is weak, `deliberate:` its line count and the unmoved directory pin the branch, and the plan's suite row now names the pushed head's gate.
- `deliberate:` backend 1 (the in-process `fed_counter` guard: three call sites mint a fresh id per launch, no production counter until the external client ships); tui 4 (no primary-status command yet) and tui's low on the persist line repeating during a write failure; lang:rust 6 (`with_settle_file` call order, documented).
- Tokens and seconds are in the transcript; each reviewer ran on opus.

## Decision log

- 2026-11-25: no production state keeps a counter yet (`cli/ctx.rs`
  `EXTERNAL_CLIENT` is `None`); observing this in production is deferred to
  the external-client ticket, ceiling: its release. There, compare per UTC day
  the counter's total with the sum of every node's ledger and count the
  dropped-settle and `shared_spend_not_booked` lines; a counter more than 5%
  below the ledger sum on any day puts the floor back (one line in
  `node::daily_spent`).
- 2026-11-25: refused settle, the runner's unreachable primary or refused
  seat, and a failed gate settle are deliberate (above), decided-by: agent
  (ADR-0080).
- 2026-11-25 (implementation, departures from the body, all carried in the
  reviews sidecar): unreserved settles are booked only on the reconcile
  tick; the runner's holder comes in as `RunConfig.node_host` (read once in
  `cli/run.rs` and `cli/replay.rs`, `effects.no-ambient-access`), and
  `seat_holder` moved from `cli/coordinator.rs` into `shared_state::node`;
  `Selection::Local` writes no file and books nothing; a failed `gate.settle`
  writes a `gate_settle_failed` decision and line; the test fixtures gained
  `execute_presented_with` for the gated test.
- `system_clock` in `seats.rs` is the production default of the injected
  `Clock` (`with_clock`) and the only ambient read in the book.
- The lost-answer test is a conflict plus a direct `settle_spend`, because
  `FakeState::settle_spend` ignores degraded mode.
- The carry_on hook gap in the body is wrong: such an agent has no dispatch
  on the ledger (`book_turn_usage` answers `NoSuchDispatch`), so it is not
  metered at all; `docs/shared-state.md` says so.
- One `book_usage` call per dispatch id at its three call sites, and each
  site mints a fresh id per launch (`ids.dispatch_id()` in `attempt.rs`,
  `review/engine.rs`, `scheduler.rs`), so a resume or successor never reuses
  one: the per-run `fed_counter` guard is the in-process guard the
  idempotence rule allows for deliveries that do not outlive the process.
  The ledger sum for the 5% rollback check is owed by the external-client
  ticket.
- After implementation review: a `settle:<dispatch>` seat is released through
  `Book::release` (one line per change) and `cap_settles` stops at the first
  unreachable answer; a reservation minted while the file cannot be written
  is dropped unsent, and no reservation is made while the last write failed
  (`store_failing`); a non-NotFound read error is set aside like a parse
  failure, and when the rename fails nothing is kept on disk; the file error
  names its cause (`SettleFileError`); `flush_settles` takes a two-variant
  `Pass`, not a bool; the conformance case has a second half (a settle above
  a zero reservation after its seat went back).

<!-- panel: repos=relais,homelab adds=contract reviewers=architect,backend,lang:rust,po,tui,unix body-sha=89ad2bb99d96 -->
