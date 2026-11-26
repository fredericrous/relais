---
status: active
branch: prog/agents-many-nodes/an-t35-read-the-shared-counter
repos: [relais, homelab]
adrs: []
plan-id: p-01M8CGWWH532NRMW8NKG9NKMH9
---
# Read the shared day counter

## Review panel

👉 **Decide:** none — approve if one `relais counter` run, over one new trait read, gives the 5% check its per-day verdict.
📍 relais, homelab · 4 phases · next: Phase 1. Panel: backend, architect, po, lang:rust, tui, unix (opus); codex: not run (programme ticket agent).
**Changed by review:** log coverage (`covers_from`), waiting settles and multi-node verdicts in `short_percent`; clock-free conformance (`a_counter_adds_up`); `contract` seats added.
**Verdicts:** round 2: lang:rust approve; backend approve; architect, po, tui, unix approve-with-changes; 8 carried; words 1071 → 1558 (×1.45, fixes of unverifiable statements); remove 0, merge 0.
📄 Full reviews: [2026-11-26-read-the-shared-counter.reviews.md](2026-11-26-read-the-shared-counter.reviews.md)

## Picture

```
 relais counter [--days N] [--json]
   |
   |-- SharedState::spent_between(day, next) -> counter (None: local)
   |-- Ledger::spend_between(day, next)      -> ledger (this node)
   |-- coordinator.log(.1) "(dropped_micros=n)", by stamp -> dropped
   |     short% = (ledger - counter - dropped - waiting) / ledger
   |     > 5 "short"; edge, partial, log_incomplete, no_counter
   '-- seat-settles.json                     -> waiting settles
 failure: one stderr line; --json adds {"error","exit"} (an-t32)
```

One command run replaces the log search and gives the 5% check its verdict per UTC day.

## Goal

an-t31's counter lines say "no relais command reads the shared primary yet".
The person cannot see the day's global spend or the settles still waiting,
and the 5% check (restore `max(global, local)` in `node::daily_spent` if a UTC
day's counter falls over 5% below the ledger sum, due when the external client
ships, an-t24 and an-t28) needs each day's `dropped_micros=` summed from the
coordinator log. Add the read, the command, and the check as one run (an-t35).

## Non-goals

- No read of other nodes' ledgers, logs or waiting settles: the command shows
  the shared counter and this node's side; a multi-node comparison belongs to
  the external-client work.
- The run-stderr line "was not booked to the shared counter" and its
  `shared_spend_not_booked` decision are not in `coordinator.log`; the command
  does not read them (named in the docs).
- No change to `node::daily_spent`; the command reports whether the floor is
  due, it does not restore it.
- No read of the slice, seats or the publish queue.

## Code review

- relais: backend, lang:rust, unix, tui
- homelab: none — markdown only

## Behaviour

**Decisions (answering the ticket):**

1. **Trait read: one method**, `SharedState::spent_between(from, to) ->
   Result<Option<MicroUsd>, SharedStateError>`: what the global counter booked
   in `[from, to)` (RFC 3339), `None` when the implementation keeps no counter
   (`LocalState`), One rule for every implementation: bounds are checked first (malformed, or `from > to`, is `Refused`), then `None` or `Some`; `None` exactly when `daily_spent` answers `None` (checked in `a_counter_adds_up` on every implementation); `from == to` is `Some(0)` or `None`. `daily_spent` means "since
   a start", so a past day would need two reads subtracted from a primary
   that may count per day; one half-open read is unambiguous for a client.
   The waiting settles are **not** a trait read: they are this node's file
   (`seat-settles.json`), another node's are not this node's to know.
2. **Command: new `relais counter`**, not `report` (a cost report of the
   ledger, imports usage, writes) nor `doctor` (verdicts about setup, spawns
   probes). It reads only.
3. **The 5% check is in the command**: per UTC day it prints the counter, the
   ledger sum, the dropped amounts summed from the log, and `short_percent`.

**`relais counter [--days N] [--json]`** (`--days` 1 to 31, default 7; today
is included and marked partial). Builds one `SharedState` (`ctx::shared_state`,
an-t29's census) and opens the ledger read-only (no ledger: every ledger figure
is 0). For each day, newest first:

| field | source |
| --- | --- |
| `counter_micros` | `spent_between(day, next day)`; `null` under `local` |
| `ledger_micros` | new `Ledger::spend_between(from, to)`, the `spend_since` query with `AND at < ?2` |
| `dropped_micros` | sum of every `(dropped_micros=<n>)` in `coordinator.log` and `coordinator.log.1` whose RFC 3339 stamp falls on that UTC day (a line without a readable stamp or number is skipped and counted in `log.unparsed`); a day before `log.covers_from`, the oldest stamp read (the log rotates past 1 MiB, one old file kept), is `null` |
| `waiting_micros` | waiting settles whose `since` falls on that day (late, not lost) |
| `short_percent` | `(ledger - counter - dropped - waiting) / ledger * 100`, `null` when the counter or the dropped sum is `null` or the ledger is 0 |
| `check` | `no_counter` (local), `no_ledger_spend`, `log_incomplete` (day before `covers_from`, never `ok` or `short`), `partial` (today), `edge` (over 5 while the next day is below 0: a drop logged after midnight, not a floor), `ok`, `counter_above_ledger` (below -5: another node feeds the counter, or it double-counts; this node's ledger cannot judge the day), or `short` (over 5: the floor is due; 0 to -5 is `ok`) |

Also `selection`, `waiting` (`count`, `micros`, `oldest_since`, from
`seat-settles.json`, read by a new `coordinator::seats::waiting_settles(path)`
that reuses the file's row type; a file that is absent is zero, one that is
unreadable is said in `waiting.error`; in text mode `waiting.error` and a non-zero `log.unparsed` are each one stderr line `relais counter: <message> (<path>)`, exit still 0) and `log` (`path`, files read,
`unparsed`, `covers_from`). Both log files are opened before either is read and an inode seen twice is read once (a rotation between opens neither double-counts nor skips); a final line without a newline is skipped, not counted in `unparsed`.

Text output: a header line naming the selection, one row per day (columns `day counter ledger dropped waiting short% check`, money through `MicroUsd` Display, each row at most 80 columns, no ANSI escape, the verdict a word), the
waiting line, then one sentence stating that the dropped sum comes from this
node's log and the cutover day is excluded by the person. Under `local` it
prints "this node keeps no shared counter (shared_state = local)" and the
ledger and waiting columns.

Streams follow an-t31/an-t32: result on stdout (`--json`: one object); any
failure, including `external` with no client (exit 3), one stderr line and
under `--json` the `{"error","exit"}` object (`error_object_under_json`).
Exit 0 whenever it read, `short` days included (the verdict is data); a closed stdout (`BrokenPipe`) is not a failure, as in `report.rs`. `--days` outside 1 to 31 exits 2: one stderr line and, under `--json`, the `{"error","exit":2}` object.

A dropped line's stamp is the time of the drop, up to 24 h after the spend; the
docs say a day-edge drop is credited to the day it was logged.

`docs/shared-state.md` gains the command in "Where to look" and the 5% check as
three steps (run `relais counter --days 14 --json`, read `short_percent` per
day, restore the floor when a non-cutover day says `short`; with more than one node feeding the counter, sum each node's `ledger_micros`, `dropped_micros` and `waiting_micros` from its own `--json` run against one `counter_micros`, because one node alone reads `counter_above_ledger`); the counter lines'
`PRIMARY_UNREAD` constant becomes "run `relais counter` for this node's view
of the counter" where the line is about the counter; lines about the primary's
own state keep asking its operator.

## Phases

- [ ] Phase 1 — trait read, `LocalState`, `FakeState` (per-day booking),
  conformance cases `a_counter_range_is_checked` (`run_all` 14 to 15, no clock: a range in 2000 is 0 or `None`, `from==to` is 0 or `None`, malformed or reversed is `Refused`) and `a_counter_adds_up` (`run_all` 15 to 16, no clock: after a settle, `spent_between(start, far future)` equals `daily_spent(start)` and a split at any midpoint adds up to the whole); the two-day reading is a `FakeState` unit test, since a client's clock origin is unknown to the suite:
  `crates/relais/src/shared_state/mod.rs`, `crates/relais/src/shared_state/local.rs`,
  `crates/relais/src/shared_state/fake.rs`, `crates/relais/src/shared_state/conformance.rs`,
  `crates/relais/src/cli/decide.rs` (its test double forwards the method).
- [ ] Phase 2 — ledger and settles reads: `crates/relais/src/ledger/usage.rs`,
  `crates/relais/src/coordinator/seats.rs`, `crates/relais/src/coordinator/mod.rs` (`stamped` made `pub(crate)`).
- [ ] Phase 3 — the command: `crates/relais/src/cli/counter.rs`,
  `crates/relais/src/cli/mod.rs`, `crates/relais/src/cli/ctx.rs` (SOURCES),
  `crates/relais/tests/shared_state_cli.rs`.
- [ ] Phase 4 — docs, counter lines, CHANGELOG: `docs/shared-state.md`,
  `crates/relais/src/shared_state/mod.rs` (`PRIMARY_UNREAD`),
  `crates/relais/src/coordinator/seats.rs`, `CHANGELOG.md` (names the new required trait method for the closed client and that `conformance::run_all` runs the two new cases), homelab
  `docs/programmes/agents-many-nodes/an-t35-read-the-shared-counter.md`.

## Verification

| Check | Input | Expected | Actual |
| --- | --- | --- | --- |
| conformance | `run_all` and `run_all_with_clock` on `LocalState` and `FakeState` | malformed or reversed bounds `Refused` on both, `from==to` 0 or `None`; fake books a settle on its day, 0 outside, equal to `daily_spent` over a wide range; `LocalState` `None` | |
| ledger read | unit test on `spend_between` | rows in the ledger's stored `at` format (milliseconds): one inside counts, one at `...00.000Z` on the end bound does not, one at `...00.123Z` on the start bound does | |
| log sum | unit test of the log parser, lines built with `coordinator::stamped` and the two real drop formats | sums per day across two files, once per inode, skips a bad stamp and counts it, a day before the oldest stamp is `null` and `log_incomplete` | |
| verdict | unit test of the per-day check | 4% `ok`, 6% `short`, -8% `counter_above_ledger`, 6% with next day below 0 `edge`, a waiting settle that covers the gap `ok`, today `partial`, `local` `no_counter`, ledger 0 `no_ledger_spend` | |
| command, binary | `relais counter --json` in a temp HOME: local; and under an absent `external` | local: one object, `selection: local`, `check: no_counter`; external: stdout `{"error","exit":3}`, stderr one line, exit 3 | |
| command, text | `relais counter` and `--days 0`, temp HOME; and text and `--json` spawned with stdout piped and the read end dropped before the first write (output is under the 64 KiB pipe buffer, so a shell `head` would not raise EPIPE) | rows at most 80 columns, no ESC byte, stderr empty; `--days 0` exit 2 with the object under `--json`; closed stdout: the binary's own exit 0, stderr empty | |
| seats lines | `coordinator::seats` tests | the new tail, `dropped_micros` kept | |
| suite | `cargo test -p relais`, `cargo clippy -p relais --all-targets -- -D warnings` | 0 failed, clean | |

<!-- panel: repos=relais,homelab adds=contract reviewers=backend,architect,po,lang:rust,tui,unix body-sha=4471a7b2fdc1 -->
