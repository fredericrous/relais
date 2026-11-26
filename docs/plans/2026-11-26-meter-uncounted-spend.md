---
status: active
branch: prog/agents-many-nodes/an-t28-meter-uncounted-spend
repos: [relais, homelab]
adrs: []
plan-id: p-01M8AV8C8687YVY1G3RE96STS7
---
# Meter the spend that still misses the counter

## Review panel

👉 **Decide:** none — approve if a retried settle plus three named, ceilinged gaps beats new hook metering.
📍 relais, homelab · Phase 1 built · next: Phase 2 (the homelab Resolution). Panel: backend, lang:rust, tui, unix (opus); codex: not run (programme ticket agent).
**Changed by review:** only `Unavailable` is retried, with a line before the second try; `dropped_micros=<n>` token; each gap names its ceiling.
**Verdicts:** round 2: 2 approve, 2 approve-with-changes; 4 carried (implemented); words 872 → 1070 (×1.23); remove 0, merge 0.
📄 Full reviews: [2026-11-26-meter-uncounted-spend.reviews.md](2026-11-26-meter-uncounted-spend.reviews.md)

## Picture

```
 spend path                  today                  after this plan
 ----------                  -----                  ---------------
 settle the primary refuses  dropped, 1 line        gap kept; the line
                             (no amount)            carries the dollars
 runner gate.settle fails    reservation booked     retried once; a lost
                             (1 try)                answer books once;
                                                    persistent = gap kept
 hook spawn, carry_on,       nowhere                gap kept; pinned by
 no daemon                                          a test; journal marks
```

A spend path that used to slip past the counter silently is now either
booked, or named with a test and with a number the 5% check can sum.

## Goal

an-t24 fed the global counter from every settle it could reach and named
three paths it left out. This ticket (an-t28) decides each one, so the
check of the counter against the ledger (more than 5% below on a UTC day
puts the ledger floor back) can be trusted:

1. **A settle the primary refuses** stays a named gap. A conforming primary
   never refuses a settle (conformance `a_late_settle_is_never_refused`,
   which relais-fabric runs), so a refusal is a client bug, and re-booking
   it elsewhere would go to the same primary that just disowned it. What
   changes: the drop line carries the settle's amount, so the 5% check can
   add the dropped dollars back.
2. **A failed `gate.settle`** is metered for the transient case: the runner
   tries the settle twice. The coordinator's settle is idempotent (a
   duplicate is `Applied`; a settle after both ends is `Unknown`, Ok), so a
   retry after a lost answer never books twice. A settle that fails on both
   tries stays the named gap (the coordinator settles it unknown when its
   lease lapses, booking the reservation) and still writes one
   `gate_settle_failed` decision and line.
3. **A hook spawn under `carry_on` with no daemon** stays a named gap,
   pinned by a test. It misses both the ledger and the counter, so it does
   not skew their comparison; booking it to the counter alone would lift
   the counter above the ledger and hide an undercount elsewhere. A
   hook-admitted agent settles at its reservation (no usage in a hook
   payload), and `dispatch_reserve_micros` defaults to 0, so even with a
   daemon such an agent books $0: there is no figure to book. The hook journal already marks it: a `PreToolUse` on
   the Agent tool journalled `decision: silent` with `coordinator_debug:
   null`.

## Non-goals

- Booking hook-admitted agents' real cost (no usage in the payload; a
  separate ticket if wanted).
- Changing the `SharedState` trait, `LocalState`, `FakeState` or the
  settle file format.
- A retry in the launch-failed branch (`launch.rs`, `settle(None)` after a
  launch that produced no process): its reservation is the honest figure
  and the coordinator reaps it at lease grace.
- `verify/heavy.rs`'s settle: a local class takes no shared seat.

## Code review

- relais: backend, lang:rust (tui and unix left out: one stderr line gains an amount, no new command or flag)
- homelab: none — markdown only

## Behaviour

- `coordinator/seats.rs` `flush_settles`, `Refused` arm: the line becomes
  `seat <id>: booking the spend of <amount> was not accepted: <error>;
  dropped, the shared counter does not count it; if it repeats, check the
  shared primary (dropped_micros=<n>)` (`<amount>` is the `MicroUsd` display
  of `actual`; `dropped_micros=<n>` is a fixed token the external-client
  work's 5% check sums per UTC day; the cap line gains the same token).
- `runner/launch.rs`: `gate.settle(dispatch, spent)` is tried up to
  `SETTLE_TRIES = 2` times, stopping at the first `Ok`. Only
  `GateError::Unavailable` (no answer, a timeout) is retried: `Refused` and
  `Protocol` are answers and repeat themselves. A settle waits up to
  `WRITE_RESPONSE_TIMEOUT` (30 s), so the worst case is 60 s at the end of a
  run; a failed first try writes `relais: dispatch <id>: the coordinator did
  not answer the settle (<error>); trying once more` before the second. A
  cancelled run still retries: its spend happened. When the settle is not
  taken: `relais: dispatch <id>: the coordinator did not take the settle of
  <cost> after <n> tries: <error>; the shared counter books the reservation
  instead; run `relais coordinator status`` (`<n>` formatted from
  `SETTLE_TRIES`, 1 for an answer) and the `gate_settle_failed` decision,
  once, carrying the last error. A retry answered `Unknown` counts as taken,
  as today: it cannot tell a forgotten entry from a restarted coordinator.
- `docs/shared-state.md`, "What still misses the counter": the refused
  settle names its amount line; the failed gate settle says it is retried
  once; the `carry_on` entry says why it does not skew the comparison, that
  the ceiling misses it, and how the journal marks it.
- `docs/shared-state.md`: each gap names its ceiling: a refused settle is
  0 on a conforming primary; a failed gate settle is off by cost minus
  reservation per failure; a `carry_on` spawn is $0 on the counter (the
  default reservation) and unbounded in real cost, which the ceiling
  misses. In the daemon stderr list, the refused-settle line is quoted
  with its amount and token, and the cap line's quote matches the code
  (`oldest dispatch:`).
- `CHANGELOG.md` `## Unreleased`: one line.

## Phases

- [x] Phase 1 — relais: the amount in the refused-settle line, the settle
  retry, the tests, docs and changelog:
  `crates/relais/src/coordinator/seats.rs`,
  `crates/relais/src/coordinator/seats/tests.rs`,
  `crates/relais/src/runner/launch.rs`,
  `crates/relais/src/runner/tests/mod.rs`,
  `crates/relais/src/hook/respond.rs`,
  `docs/shared-state.md`, `CHANGELOG.md`
- [ ] Phase 2 — homelab: the an-t28 Resolution (status closed, Decisions
  line):
  `docs/programmes/agents-many-nodes/an-t28-meter-uncounted-spend.md`,
  `docs/programmes/agents-many-nodes/map.md`

## Verification

- Input: two refused settles (reserved and unreserved) → expected: one
  drop line each, naming the amount, counter 0 → test
  `a_settle_the_primary_refuses_is_dropped_once_and_the_next_one_books`
  and `a_refused_unreserved_settle_frees_its_own_seat`, extended.
- Input: `FaultyGate` (a real `LocalGate` inside) with a new
  `Fault::SettleAnswerLost` (the inner settle applies, then the call is
  `Unavailable`) → expected: the retry answers Ok (`Unknown`: the entry was
  forgotten after release), `inner.status()` shows the run settled at the
  cost once, no `gate_settle_failed` → new runner test.
- Input: `FaultyGate` with `Fault::SettleUnavailable` → expected: two
  tries, one `gate_settle_failed`; with `Fault::SettleRefused` → one try,
  one `gate_settle_failed` → new runner tests.
- Input: `carry_on`, an unreachable gate, a `FakeState` in the hook's
  shared claims, `dispatch_reserve_micros` 500, a spawn then its
  `SubagentStop` → expected: `Silent`, `fake.spent()` 0, the journal entry
  `decision: silent`, `coordinator_debug: null` → new hook test.

Observed on f3910c69a (rebased on main 1bebd973b):

- Refused settles: one drop line each, `$0.00001 … (dropped_micros=10)`
  and `$0.0004 … (dropped_micros=400)`, counter 0; the cap lines carry
  `(dropped_micros=1500000)` and `(dropped_micros=3000000)`
  (`coordinator::seats`, all pass).
- `a_settle_whose_answer_was_lost_is_retried_and_booked_once`: settles
  `[Some(100), Some(100)]`, the `LocalGate` run settled at 100 once, no
  `gate_settle_failed`, one `gate_settle_unconfirmed` (the retry is answered
  `Unknown`, the entry forgotten after release).
- `a_settle_no_coordinator_answers_is_tried_twice_then_reported_once`: 2
  settles, 1 `gate_settle_failed`; `a_refused_settle_is_not_retried_and_is_reported_once`:
  1 settle, 1 `gate_settle_failed`. The stderr lines were seen in the first
  red run (`trying once more`, `after 2 tries`, `after 1 try`); a passing
  test's stderr is captured, so the final `in <n> ms` wording is unobserved.
- `a_settle_retry_pause_is_jittered_within_half_its_base`: 50 ids all in
  [500, 1500) ms, varying, the same id the same pause, zero stays zero.
- `a_carry_on_spawn_with_no_coordinator_is_metered_nowhere`: `Silent`
  twice, journal `silent` / `null`, `fake.spent()` 0 with a 500-micro
  reservation set.
- The first run of the runner tests went red twice on the fixture (an empty
  candidate, then heavy-check settles at $0 counted as faults); both fixed
  in the test double, not the code.
- Pre-commit gate on f3910c69a: `cargo test` 3232 passed, 0 failed (lib
  2749, integration 354, others), clippy `-D warnings` clean; `cargo clippy
  --all-targets --features conformance -- -D warnings` clean.

## Decision log

- 2026-11-26: the three paths decided as in the Goal, decided-by: agent
  (ADR-0080).
- 2026-11-26: `greenbelay plan-review record` exited 1 (no transcript of the
  panel visible to a ticket agent); the panel ran, its reviews are in the
  sidecar.
- 2026-11-26 (implementation, after review): the retry waits a jittered
  pause (`RunConfig.settle_retry_pause`, 1 s from the CLI, zero in tests);
  a retry answered `Unknown` writes `gate_settle_unconfirmed`, since it
  cannot tell a lost answer from a forgotten dispatch; the Code review
  reason "one stderr line gains an amount" understates it (two new runner
  lines), the profile set is kept.

## Implementation review

- implementation-review: round 1 approve-with-changes; blocking (no backoff) fixed with a jittered, injected pause; plan ticks and observed rows filled; vacuous-fault guard added. Delta (tree e50743c6) approve-with-changes: its two mediums are this closing commit; candidate d04a2ba77be5 fine. Tokens 50k + 41k, 36 s + 38 s.
- backend: round 1 approve-with-changes; unconfirmed retry now reported (`gate_settle_unconfirmed`), pause added, fault-cost guard. Delta approve; its low (no test pins a first-try `Unknown` as confirmed) kept `deliberate: the guard is two lines beside its comment; a test would reopen review for a low, filed as a follow-up`. 72k + 38k.
- lang:rust: approve; typed `SettleTaken`/`SettleNotTaken`, bounded `for` loop, `calls` no longer bumped by Unavailable (all three lows fixed).

<!-- panel: repos=relais,homelab adds= reviewers=backend,lang:rust,tui,unix body-sha=b30818e85eae -->
