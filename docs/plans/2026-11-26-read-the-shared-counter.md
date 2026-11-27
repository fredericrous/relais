---
status: done
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
| `waiting_micros` | waiting settles whose `since` falls on that day (late, not lost); `null` when `seat-settles.json` cannot be read |
| `short_percent` | `(ledger - counter - dropped - waiting) / ledger * 100`, `null` when the counter, the dropped sum or the waiting sum is `null`, or the ledger is 0 |
| `check` | `no_counter` (local), `no_ledger_spend`, `log_incomplete` (day before `covers_from`, or the oldest day once the log was rotated; never `ok` or `short`), `waiting_unknown` (the settles file was unreadable), `partial` (today), `cutover` (the first fed day and the days before it that spent while the counter read 0),  `edge` (over 5 while the next day is below 0: a drop logged after midnight, not a floor), `ok`, `counter_above_ledger` (below -5: another node feeds the counter, or it double-counts; this node's ledger cannot judge the day), or `short` (over 5: the floor is due; 0 to -5 is `ok`) |

Also `selection`, `waiting` (`count`, `micros`, `oldest_since`, from
`seat-settles.json`, read by a new `coordinator::seats::waiting_settles(path)`
that reuses the file's row type; a file that is absent is zero, one that is
unreadable is said in `waiting.error`; in text mode `waiting.error` and a non-zero `log.unparsed` are each one stderr line `relais counter: <message> (<path>)`, exit still 0) and `log` (`path`, files read,
`unparsed`, `covers_from`). Both log files are opened before either is read and an inode seen twice is read once (a rotation between opens neither double-counts nor skips); a final line without a newline is skipped, not counted in `unparsed`.

Text output: a header line naming the selection, one row per day (columns `day counter ledger dropped waiting short% check`, money as dollars to four places so columns line up, each row at most 80 columns, no ANSI escape, the verdict a word), above them a summary line (`N day(s) short, newest <day>: the floor is due`, or `no day is short (J of M judged)`, or `no day could be judged`), after them a waiting line (`waiting: N settle(s), $x, oldest <date>`, or `unreadable` with the shipped stderr line) and a footer saying the dropped sum comes from this
node's log and a `cutover` day is left out of the check. Under `local` it
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
`PRIMARY_UNREAD` constant becomes "`relais counter` shows this node's side; no
relais command reads the primary itself"; the lines keep asking the primary's
operator to check the primary.

## Phases

- [x] Phase 1 — trait read, `LocalState`, `FakeState` (per-day booking),
  conformance cases `a_counter_range_is_checked` (`run_all` 14 to 15, no clock: a range in 2000 is 0 or `None`, `from==to` is 0 or `None`, malformed or reversed is `Refused`) and `a_counter_adds_up` (`run_all` 15 to 16, no clock: after a settle, `spent_between(start, far future)` equals `daily_spent(start)` and a split at any midpoint adds up to the whole); the two-day reading is a `FakeState` unit test, since a client's clock origin is unknown to the suite:
  `crates/relais/src/shared_state/mod.rs`, `crates/relais/src/shared_state/local.rs`,
  `crates/relais/src/shared_state/fake.rs`, `crates/relais/src/shared_state/conformance.rs`,
  `crates/relais/src/cli/decide.rs` (its test double forwards the method).
- [x] Phase 2 — ledger and settles reads: `crates/relais/src/ledger/usage.rs`,
  `crates/relais/src/coordinator/seats.rs`, `crates/relais/src/coordinator/mod.rs` (`stamped` made `pub(crate)`).
- [x] Phase 3 — the command: `crates/relais/src/cli/counter.rs`,
  `crates/relais/src/cli/mod.rs`, `crates/relais/src/cli/ctx.rs` (SOURCES),
  `crates/relais/tests/shared_state_cli.rs`.
- [x] Phase 4 — docs, counter lines, CHANGELOG: `docs/shared-state.md`,
  `crates/relais/src/shared_state/mod.rs` (`PRIMARY_UNREAD`),
  `crates/relais/src/coordinator/seats.rs`, `CHANGELOG.md` (names the new required trait method for the closed client and that `conformance::run_all` runs the two new cases), homelab
  `docs/programmes/agents-many-nodes/an-t35-read-the-shared-counter.md`.

## Verification

| Check | Input | Expected | Actual |
| --- | --- | --- | --- |
| conformance | `run_all` and `run_all_with_clock` on `LocalState` and `FakeState` | malformed or reversed bounds `Refused` on both, `from==to` 0 or `None`; fake books a settle on its day, 0 outside, equal to `daily_spent` over a wide range; `LocalState` `None` | pass: `shared_state::conformance` (`local_state_conforms`, `the_fake_conforms`, `the_fake_conforms_with_its_clock`) with `a_counter_range_is_checked` and `a_counter_adds_up` (`run_all` now 16 cases); `fake::a_settle_is_counted_on_the_day_it_was_booked` |
| ledger read | unit test on `spend_between` | rows in the ledger's stored `at` format (milliseconds): one inside counts, one at `...00.000Z` on the end bound does not, one at `...00.123Z` on the start bound does | pass: `ledger::usage::spend_between_counts_the_day_in_either_stored_format` (+00:00, ms and Z rows; the `.000Z` end-bound row excluded, sum 1,110) |
| log sum | unit test of the log parser, lines built with `coordinator::stamped` and the two real drop formats | sums per day across two files, once per inode, skips a bad stamp and counts it, a day before the oldest stamp is `null` and `log_incomplete` | pass: `cli::counter` tests: line-final token, half line left, `the_daemons_own_stamp_is_what_the_parser_reads`, hard-linked inode read once, two files add up, an unreadable log names its file |
| verdict | unit test of the per-day check | 4% `ok`, 6% `short`, -8% `counter_above_ledger`, 6% with next day below 0 `edge`, a waiting settle that covers the gap `ok`, today `partial`, `local` `no_counter`, ledger 0 `no_ledger_spend` | pass: `cli::counter` assess tests: floor, dropped/waiting added back, quiet days not a cutover, `no_counter`/`log_incomplete`/`waiting_unknown`, partial and edge, short stays short; summary line test |
| command, binary | `relais counter --json` in a temp HOME: local; and under an absent `external` | local: one object, `selection: local`, `check: no_counter`; external: stdout `{"error","exit":3}`, stderr one line, exit 3 | pass: `shared_state_cli::a_local_counter_is_one_object_with_no_counter_verdicts` and `a_counter_under_an_absent_external_layer_is_refused_on_both_streams` (exit 3, both streams, object under `--json`) |
| command, text | `relais counter` and `--days 0`, temp HOME; and text and `--json` spawned with stdout piped and the read end dropped before the first write (output is under the 64 KiB pipe buffer, so a shell `head` would not raise EPIPE) | rows at most 80 columns, no ESC byte, stderr empty; `--days 0` exit 2 with the object under `--json`; closed stdout: the binary's own exit 0, stderr empty | pass: `the_counter_text_fits_and_a_bad_day_count_is_exit_2` and `a_counter_whose_stdout_is_closed_still_exits_0`; pilot on the branch binary in a temp HOME with a log line and a waiting settle: text rows within 80 columns, `--days 0 --json` printed `{"error":"--days takes 1 to 31, not 0","exit":2}`, exit 2 |
| seats lines | `coordinator::seats` tests | the new tail, `dropped_micros` kept | pass: `coordinator::seats` tests unchanged and green with the new `PRIMARY_UNREAD`; `the_waiting_settles_are_read_from_their_file` (absent, valid, bad, directory) |
| suite | `cargo test -p relais`, `cargo clippy -p relais --all-targets -- -D warnings` | 0 failed, clean | `cargo test -p relais` on `074da60dd`: lib 2769, bin 91, it 355, shared_state_cli 17 + 1 (the width test, since fixed), 0 other failures; after the review fixes: lib `coordinator::seats shared_state ledger::usage`, bin 91+, `shared_state_cli` 18, all pass; `cargo clippy -p relais --all-targets -- -D warnings` clean |

## Implementation review

- implementation-review: round 1 approve-with-changes (blocking `.ok()` reason, cutover rule, tests, plan lines: all fixed); Delta approve-with-changes (plan Actuals and Behaviour text, fixed in the closing commit); final Delta approve-with-changes (plan text; `unwrap_or` fixed); last Delta approve-with-changes, only the plan's ticks and check list, fixed here. ~71k, 77k, 53k, 46k tokens.
- backend: round 1 approve-with-changes (rotated oldest day, cutover after quiet days, unread settles, tests: fixed; `deliberate:` no default body for the new trait method, the trait is documented public and unstable); Delta approve-with-changes (rotation race, judged count: fixed); final approve.
- lang:rust: round 1 approve-with-changes (`try_exists`, typed settles error, `range_bounds`, path in log error: fixed; `deliberate:` `Check` wire names stay one `name()`); Delta approve-with-changes (dead arm, integer money, `source()`: fixed); final approve-with-changes, `unwrap_or` fixed, `deliberate:` an `is_judged` method and a typed log error (lows); last Delta approve.
- unix: round 1 approve-with-changes (closed stderr: fixed); Delta approve-with-changes (exit 2 for an unreadable log: fixed); final approve.
- tui: round 1 approve-with-changes (6 items: fixed); Delta approve-with-changes (summary above the header and judged counts, waiting width: fixed; `newest` finding withdrawn, rows are newest first); final approve.

## Decision log

- 2026-11-26: `stamped` made `pub` (the plan said `pub(crate)`): the bin crate's test reads the daemon's stamp through it. The log parser and verdicts live in `cli/counter.rs`, not a lib module.
- 2026-11-27: Windows CI showed the added subcommand overflowed the 1 MiB main stack at startup (140 integration failures); `crates/relais/build.rs` now links the Windows binary with an 8 MiB stack. Not in the reviewed plan; reviewed as a delta.
- 2026-11-26: the panel could not be bound by `plan-review record` (no transcript for a ticket agent); its reviews are in the `.reviews.md` sidecar.


<!-- panel: repos=relais,homelab adds=contract reviewers=backend,architect,po,lang:rust,tui,unix body-sha=4471a7b2fdc1 -->
