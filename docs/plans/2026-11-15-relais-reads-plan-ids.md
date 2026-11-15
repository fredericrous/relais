---
canonical: amont-agent:docs/plans/2026-11-15-every-plan-gets-an-id.md
phases: [1]
status: active
branch: feat/plan-id-reader
repos: [relais]
---
# relais reads plan ids

Pointer: Phase 1 of the plan "Every plan gets an id, and the panel counts", which lives in amont-agent.

relais's review-outcome reader (`crates/relais/src/review_outcome.rs`) declares:
- every field greenpiste writes today (`plan.seat_source`, `plan.seat_classes`, `findings_unparsed`, and a finding's `n` with optional role and disposition);
- the two fields greenpiste 4.3.0 adds (`plan_id`, `plan.counts`).

All are optional, so `deny_unknown_fields` keeps every row.

Beyond the canonical plan's list: `Finding` also accepts a finding parsed from the reviewer's text (`n` and a severity, no role or disposition). 141 rows of the live ledger have only that shape, and more carry `n`. The implementation review asked for it to be recorded here.

## Verification
- `cargo test -p relais --lib review_outcome`: 18 passed, including `rows_as_greenpiste_writes_them_parse`.
- A copy of this machine's ledger (2026-11-15), parsed line by line with `Row`: 565 parsed and 996 invalid before; 1,561 parsed and 0 invalid after.

<!-- panel: repos=relais body-sha=f1267bf8ce83 -->
