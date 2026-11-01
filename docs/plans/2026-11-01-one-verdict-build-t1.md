---
status: active
branch: feat/per-check-timings
repos: [relais]
canonical: decisions:docs/plans/2026-11-01-one-verdict-build.md
phases: [T1]
---
# One verdict per tree, build: T1 per-check timings (pointer)

The canonical plan is decisions `docs/plans/2026-11-01-one-verdict-build.md`.
This branch is phase **T1**: relais records per-check timings (queued,
admitted, started, ended, slot wait, run time) per check and per retry, in
the receipt and in a `check_timings` ledger table, and `relais report` shows
per-label percentiles. Design: decisions
`docs/programmes/one-verdict-per-tree/t09-per-check-durations.md`.

## Verification record

| Input | Expected | Actual |
|---|---|---|
| `make lint` (fmt, clippy `-D warnings`, cycle scripts) | exit 0 | exit 0 |
| `make test` (full suite, incl. release scenarios) | exit 0 | exit 0: 2123 + 42 + 284 + 1 passed, 0 failed |
| `policy::tests::the_authority_hash_survives_effort_becoming_data` | pinned hash unchanged | passes (relais.toml untouched) |
| `receipts_already_on_disk_still_parse` + new fixture test `a_receipt_with_check_timings_round_trips` | old receipts parse with no timing; new round-trips | passes |
| `timings_aggregate_per_label_and_cached`, `nearest_rank_is_the_ceiling_rank` | per-label, per-cached aggregates; ceiling rank | passes |
| `the_check_timings_section_reads_per_label` | `make test (reused): 3 run(s), 1 retry · run 950ms/4m05s · wait 12.3s/1m01s` | passes |
| `a_check_that_times_out_then_passes_is_flaky_timed_out_then_passed` (extended) | each run carries its own timing; retry queued after the first ended | passes |
| Ledger schema | v47 adds `check_timings`; version pins moved to 47 | `a_v45_ledger_…` upgrade test passes at 47 |

Two decisions made while building:

- **Schema:** a new table (`check_timings`, v47), not new columns on the receipt tables. The receipt keeps the per-check timing too, but reports read the table.
- **Reused outcomes:** a cached baseline or an identical candidate is marked `cached`, with no fresh time claimed. `relais report` shows them as `(reused)`, so they never count as work.

## Decisions

- **Shape of the fields:** the plan words them as flat fields on `CheckOutcome` and `Attempt`. They are nested under one `timing: Option<CheckTiming>` (`#[serde(default, skip_serializing_if = "Option::is_none")]`) on each, so an old receipt reads `None` and writes unchanged. Derived values `slot_wait_ms` and `run_ms` and the `cached` flag live inside `CheckTiming`.
- **The clock:** `wall_clock()` reads `chrono::Utc::now()` directly. This is deliberate: `run_attempt` is an action, so it owns the clock, and the ledger (which has an injectable clock) is only handed the finished strings.

## Implementation review

- **Round 1 on tree `7ca8aae5`, by the parent:** approve-with-changes. Fixed: a runner-level timing test, and the nested-shape note above. The parent runs the Delta and lands.
- **Reviews this fork ran earlier (not recorded by the push hook, which reads the parent's):**
  - Round 1 on tree `f2ead769`: approve-with-changes. `nearest_rank` made read-only, setup steps record timing rows, and a kept new-shape receipt fixture `TIMED_RECEIPT_6` was added.
  - Delta on `80416aca`: approve. Rebind after a rebase and one error-message rewording, on `7ca8aae5`: approve.
- **Left open on purpose:** the live check (a real relais run, then `relais report --since …`) waits for the relais release. A branch binary would migrate the real ledger to v47 early, which has gone wrong on this machine before. The v47 migration is covered by the schema upgrade tests. After the release the check should show the receipt's `timing`, `-setup` rows, and `(reused)` rows on their own lines.
- **Status:** phase T1 is built. The relais release comes after S2.
