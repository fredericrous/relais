---
status: done
branch: prog/relais-modularise/t05-wait-loop-cancel-seam
repos: [relais, amont]
adrs: [ADR-0022, ADR-0015]
plan-id: p-01M8E24ZNTPPCY8HQFD0ES3JMZ
---
# relais waits through greengate's wait loop

## Review panel
👉 **Decide:** none — approve if a caller-owned pause in greengate's one loop, and relais waiting through it, is the whole t05.
📍 amont, relais · 2 phases · 2 releases · next: Phase 1. Panel: backend, lang:rust, architect, po, tui, unix (opus).
**Changed by review:** closures that borrow-check (`Halt`, no shared local); cancel and pre-join tests; pilot holds 360 s past `LEASE_GRACE`.
**Verdicts:** round 1 6 approve-with-changes; round 2 architect, unix, backend approve; 11 carried; words 1109 → 1436 (×1.29, high fixes after round 2); remove 0, merge 0.
📄 Full reviews: [2026-11-27-relais-waits-through-greengate-loop.reviews.md](2026-11-27-relais-waits-through-greengate-loop.reviews.md)

## Picture

```
 today                                after this plan
 -----                                ---------------
 relais slot::take                    relais slot::take
   own loop: poll -> stall ->           wait_in_line_paced(waiter, bounds,
   heartbeat -> queue.wait (sleep)        on_wait, pause)
                                            |
 greengate hooks, slot run                  +--> one loop: poll -> on_wait
   wait_in_line: poll -> on_wait ->         |  -> stall -> ceiling -> pause
   stall -> ceiling -> sleep 200ms          |
                                        greengate hooks, slot run
                                          wait_in_line = paced + sleep 200ms
```

A relais verification command used to wait in its own copy of the loop;
after this plan it waits in the crate's one loop, pausing through relais.

## Goal

Programme `relais-modularise`, ticket
[t05](../programmes/relais-modularise/t05-wait-loop-cancel-seam.md). relais's
`verify/heavy.rs` re-implements `greenharness_slots::wait_in_line` (about 80
lines), because it must heartbeat its coordinator lease, charge its budgets,
honour a cancel and sleep through its injected `Sleeper` between polls.
greengate's slots crate gains a seam that lets a caller own the pause between
polls and end the wait; relais then waits through that one loop (amont
ADR-0022: "the one loop every caller uses"), and its copy of `load1`
(`procs::load_average`) goes too.

## Non-goals

- No change to `wait_in_line`'s signature, to `LineEnd`, or to the two
  greengate callers (`greengate-runtime` `host_slots.rs`, `slot_cmd.rs`):
  additive only, a minor release (no 6.0).
- No change to the stall rule, the poll-failure count or the admission rule.
- The `holds-until` in relais's stall test (a real-clock sleep because the
  crate's `admitted_at` has no seam) stays as it is.
- No other relais module split (t04 and later).

## Code review

- relais: backend, lang:rust, architect (unix, tui left out: no flag, output or exit-code change)
- amont: backend, lang:rust, architect (unix, tui left out: no flag, output or exit-code change)

## Behaviour

**amont, `greenharness-slots`.** A new public function, additive:

```rust
#[non_exhaustive]
pub enum Pause<'a> {
    /// The poll answered: this waiter is in line.
    Waiting(&'a Queue),
    /// The poll failed; `failures` in a row so far (< POLL_FAILURES).
    Retrying { error: &'a io::Error, failures: u32 },
}
#[non_exhaustive]
pub enum Ended<B> { Line(LineEnd), Caller(B) }

pub fn wait_in_line_paced<B>(
    waiter: Waiter,
    bounds: WaitBounds,
    on_wait: impl FnMut(&Queue),
    pause: impl FnMut(Pause<'_>) -> ControlFlow<B>,
) -> Result<Lease, Ended<B>>
```

One loop, in this order on every poll: poll → granted returns; waiting calls
`on_wait`, then the stall check, then the ceiling check; a failed poll counts
a failure and ends at `POLL_FAILURES` with `LineEnd::PollFailed`, then the
ceiling check. Then `pause` is called where the loop slept, and it does the
pausing: `Continue(())` polls again, `Break(b)` ends the wait with
`Ended::Caller(b)` (the waiter leaves the queue as on any other end).

`wait_in_line` becomes a thin wrapper: `pause` sleeps 200 ms and continues
(`B = Infallible`), the `Ended::Line` mapped back to `LineEnd`, so its
callers and its behaviour are unchanged. The internal `wait_in_line_with`
seam takes the `pause` closure in place of `sleep`; every existing test of it
passes without an assertion edit. A new test drives a `Break` from `pause`
(after two waiting polls) and one from a `Retrying` pause, and checks the
order: `on_wait` before the stall end, `pause` never after a stall, ceiling
or `PollFailed` end. Code stays on the crate's 1.74 floor (no `is_none_or`
etc.; CI's `msrv` job checks it).

ADR-0022 fixes the stall rule, the failure count and the ceiling, not the
poll cadence; the cadence of a caller that owns the pause is its own (relais's
`poll`, 1 s by default). The crate's README and `docs/host-slots.md` name the
new entry point.

**relais.** `slot::take` in `verify/heavy.rs` builds its `Waiter` as now and
calls `wait_in_line_paced` with `WaitBounds { window: Some(stall_window),
ceiling: Duration::MAX }` (relais's budgets end a wait; the crate's loop now owns the `Stall`, applied
because a finite `stall_window` is below `Duration::MAX`) and:

- `on_wait`: `|_| {}`; `pause` keeps `joined_at: Option<usize>` itself, set
  from each `Waiting(q)` (two closures sharing a local would not borrow-check);
- `pause(Waiting(q))`: heartbeat the lease, then `queue.wait(Holding::Slot
  { position, behind(q), Bound::Stall }, cancel, sleeper)`; a `LeaseStop`
  breaks with it;
- `B` is a private `enum Halt { Unusable, Stop(LeaseStop) }`: `Caller(Halt::Unusable)` → `Ok(None)`, `Caller(Halt::Stop(s))` → the `deferred` row and `Err(s)`;
- `pause(Retrying)` before any `Waiting`: break with `Halt::Unusable` (said
  once through `host_slots_off`, today's text "a host slot could not be
  locked: {error}"; the command runs unqueued, as today);
- `pause(Retrying)` after joining: heartbeat and `queue.wait` at the last
  position with no `behind`, as today; a `LeaseStop` there breaks with it too (a `deferred` row with `end_of(&stop)`).

Ends map as today: granted → `admitted` row and the overrun note;
`Line(Stalled)` → `SlotLineEnd::Stalled` and `admission_queue_ended`;
`Line(PollFailed { error })` → `SlotLineEnd::Unreadable { failures: POLL_FAILURES,
cause: error.to_string() }` and `admission_queue_unreadable` (the reason keeps
the cause, as today; the unreadable test asserts it); `Line(Ceiling)` cannot happen under
`Duration::MAX` and is mapped to a named `LeaseStop` value chosen in Phase 2
(stated in the PR) with a `deferred` row written directly with `end: ceiling`
(not via `end_of`), never `unreachable!`; `Caller(stop)` → the `deferred` row
with `end_of(&stop)`, as today. Every wait still writes one journal row.

`procs::load_average` and its unix and Windows arms are removed; its two
callers (`verify/mod.rs` test slack, `cli/replay.rs`) read
`greenharness_slots::load1()`. Parity: both use `getloadavg` on macOS and
Linux and give `None` on Windows; on another unix (BSD) the crate gives `None`
where relais read a value, and relais builds there for no one (recorded only).

relais's `greenharness-slots` requirement rises to the released minor
(`"5.<n>"`, read from the release, never assumed), in `Cargo.toml` and
`Cargo.lock`.

## Phases

- [x] Phase 1 — amont: `wait_in_line_paced`, `Pause`, `Ended`, the wrapper,
  tests, docs: `crates/greenharness-slots/src/lib.rs`,
  `crates/greenharness-slots/README.md`, `docs/host-slots.md`,
  `CHANGELOG.md`; PR on `feat/slots-paced-wait`, merged, then released as the
  next minor after 5.4.0 (in flight in another session) with the
  `tag-release` skill; the crates.io job's conclusion read.
- [x] Phase 2 — relais: switch `slot::take`, remove `load_average`, raise the
  requirement: `crates/relais/src/verify/heavy.rs`, `crates/relais/src/procs.rs`,
  `crates/relais/src/verify/mod.rs`, `crates/relais/src/cli/replay.rs`,
  `crates/relais/Cargo.toml`, `Cargo.lock`, `CHANGELOG.md`; the ticket closed
  (`docs/programmes/relais-modularise/t05-wait-loop-cancel-seam.md`,
  `docs/programmes/relais-modularise/map.md`), with the line count removed.

## Decision log

- 2026-11-27 — the seam is a caller-owned pause (`ControlFlow`), not an
  `on_wait` that answers continue/stop, because relais must also sleep through
  its injected sleeper, see failed polls, and heartbeat on retries; a stop
  answer on `&Queue` alone would break its "unusable before joining" fallback.
- 2026-11-27 — additive (`Ended<B>` wraps `LineEnd`), because `LineEnd` is
  matched exhaustively by greengate-runtime and relais pins `"5"`.
- 2026-11-27 — `Pause` and `Ended` are exhaustive, not `#[non_exhaustive]` as
  the panel had it: the decided rule `types.exhaustive-matching` wins (amont
  code review, lang:rust and architect), so relais writes no wildcard arm and
  a new variant is a major change. Carried findings 1 and 4 fall away with it.
- 2026-11-27 — the crate README is left as it is: 8 lines, no API list;
  `docs/host-slots.md` and the rustdoc name `wait_in_line_paced`.
- 2026-11-27 — `Line(Ceiling)` maps to a new `SlotLineEnd::Ceiling` (relais
  code review, architect and backend), not to `Stalled`: its journal row and the
  event's reason say "ceiling"; the event name stays `admission_queue_ended`.
- 2026-11-27 — the 360 s live pilot is not run. On a host at load ~19 to 494
  with 4 GB free, a real `relais run` behind a 6-minute holder costs a model
  dispatch and a slot for little more than the cancel test now asserts (one
  heartbeat per pause, against the recording gate); the lease-expiry path is
  the coordinator's, which t05 does not touch.
- 2026-11-27 — CI pins greentopo 2.0.0 while this Mac runs 3.0.0; the pack
  refresh's `HEADS.md` is regenerated with 2.0.0 in both repositories
  (greenharness/greengate#538 failed heads-fresh first). Follow-up f36.
- 2026-11-27 — kept as today, out of t05's scope (relais code review,
  backend): the heartbeat's result is still discarded (`let _ =`), and a lock
  that fails before joining still writes no journal row; `origin/main` does
  both the same.

## Verification

- Phase 1: `cargo test -p greenharness-slots` → the existing `wait_in_line_with`
  tests pass unedited, the new Break/Retrying/order tests pass → 106 passed
  (only `run_line`'s closures changed).
- Phase 1: `cargo +1.74.0 check -p greenharness-slots` (or CI's `msrv` job) →
  builds → OK locally (with greengate-runtime, no lockfile); CI `msrv` green.
- Phase 1: `greengate slot run -- sleep 1` with a held slot, the built binary →
  waits and runs as before (the wrapper path) → queued ~3 s behind a `sleep 4`
  holder, then ran, rc 0.
- Phase 1: `greengate slot run -- sh -c 'exit 7'; echo $?` with a held slot →
  prints 7 (the wrapper keeps the child's exit status) → 7, after a 5.2 s queue.
- Phase 2: `cargo test -p relais --lib verify::heavy` → the slot-wait tests
  (`a_stalled_line_ends_the_wait_and_the_row_says_so`,
  `an_unreadable_line_after_joining_ends_the_wait_with_its_own_cause`,
  `a_moving_line_outlives_the_admission_budget_and_the_grant_is_not_blocked`,
  `queued_time_is_charged_…`, `a_queued_command_says_it_is_waiting_…`) pass
  without an assertion edit → 59 passed against crates.io 5.5.0, no existing
  assertion edited; one earlier run hit the known load-dependent
  `setup_keeps_one_slot` flake (in the relais inbox twice already), which
  passed on reruns and on `origin/main`'s code.
- Phase 2: a new test sets the cancel flag during a queued host-slot wait →
  `LeaseStop` Cancelled, one `deferred` row with no `end`, and the waiter's
  slot file gone from `dir` (the `Caller(Break)` path leaves the queue) → passes;
  also asserts one heartbeat per pause (3).
- Phase 2: a `verify::heavy` test (else reliance on the crate's Retrying-Break
  test, stated) for a poll error before joining → `host_slots_off` once,
  `Ok(None)` → a `verify::heavy` test (an `admission` directory fails the lock
  after the ticket is made) passes: `host_slots_off` once, with "a host slot
  could not be locked".
- Phase 2: `cargo check -p relais --target x86_64-pc-windows-msvc` (when the
  target is installed) → builds without `procs::load_average` → not run: the
  check stops in `libsqlite3-sys`'s C build script (no cross C compiler here);
  no `load_average` reference is left under any `cfg`.
- Phase 2: a piloted relais run whose verification command queues behind a
  `greengate slot run -- sleep 360` holder, the run started with `GREENHARNESS_SLOT_STALL_S=600` (the verification lease is admitted `Unbound`, which goes stale at the 300 s `LEASE_GRACE`, not the 120 s `DEFAULT_AGENT_LEASE_TTL` that covers hook-`Leased` agents only; the default 300 s stall would end the wait first) → waits, the queued dispatch's heartbeat age in coordinator status stays under 60 s throughout (beats every 30 s), no `stale`/`unbindable` report names it, then runs;
  the journal row reads `admitted`, and the `waiting_for_slot` events and
  any `admission_queue_*` reason read as on `origin/main` → not run (Decision
  log); the heartbeat-per-pause assertion stands in.
- Phase 2: `git diff --numstat origin/main -- crates/relais/src/verify/heavy.rs
  crates/relais/src/procs.rs` → the net lines removed per file, expected
  about 40 in `heavy.rs` and 25 in `procs.rs`; under 20 net is recorded as
  "one loop, little code saved" and the ticket still closes → `procs.rs` −20,
  `heavy.rs` +115/−100: about −5 production lines net, plus 90 lines of
  new tests. One loop, little code saved.

## After release

- Filed as [t07](../programmes/relais-modularise/t07-slot-wait-week-count.md),
  due 2026-12-08, with the corrected counts: the `host_slots_off` "could not
  be locked" events come from the run events, not `waits.jsonl`, with a floor
  of 3 rows before a doubling counts.

## Outcome

Shipped: greengate 5.5.0 (`wait_in_line_paced`, #538, #540) and relais waiting
through it. Not shipped: the live pilot and the Windows cross-check (above).
Surprise: the copy was mostly end mapping, not loop, so the line saving is
small; and a fleet-pack refresh made with the Mac's greentopo fails CI's
pinned one.

## Implementation review

- implementation-review: approve after 2 deltas (round 1 approve-with-changes: plan results, caller count, pack refresh named; numstat and ceiling-event wording fixed); 59k+63k, 37+13 s.
- backend: approve after 2 deltas (Ceiling variant added; heartbeat `Err` and pre-join row kept as today, deliberate: same as `origin/main`, filed f45); 55k+57k.
- lang:rust: approve (fmt already clean on 1.94.1; lock-file-name comment added); 38k+41k, 15+14 s.
- architect: approve (`SlotLineEnd::Ceiling` is the named value for `Line(Ceiling)`); 51k+55k, 25+15 s. All on opus; final tree eb62c0e18f4e.

<!-- panel: repos=relais,amont adds=contract reviewers=backend,lang:rust,architect,po,tui,unix body-sha=965738b77130 -->
