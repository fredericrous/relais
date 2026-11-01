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
