---
canonical: decisions:docs/plans/2026-11-06-harness-part-reports-its-cost.md
phases: [3]
status: done
branch: feat/harness-step-cost
---
# A harness part reports its cost beside its catches

Pointer. The plan and its review panel live in decisions. This repository
carries Phase 3: `relais report` puts each harness step's cost beside what
it caught (`harness.steps[].models[]`: cost, tokens, completeness, cost per
catch), report schema 16 → 17. Phase 1 shipped in amont-agent#212; Phase 2
was measured 2026-11-06 and skipped (guard firings cost 0.39–1.13% of
session cost against a 1% bar). Status `done` is for this slice only.

## Code review

- relais: backend, lang:rust, unix, tui, architect (po left out: report fields only)

## Verification

Input → expected → actual, observed before the push.

- Unit tests in `report/harness.rs`, seeded ledgers that record non-zero usage → exact `cost_micros`, `tokens`, `cost_completeness`, `cost_per_caught_micros`: a parent run with a child run summed once; a `review_repair` attempt excluded from the repair step; mixed, one-silent and all-silent usage never complete or an actual zero; a dispatch's cost credited to its own model, not the run's last; `None` per catch when nothing was caught → pass. `usage_booked_through_record_usage_delta_is_read_back_whole` books a turn and a final row the way the review engine does → $1.00, 25 tokens, actual → pass. All `report::` tests pass.
- Schema pins: the two tests that pinned 16 pin 17 → pass.
- Real data, a copy of the ledger taken 2026-11-06 (`sqlite3 .backup`, 89 MB, 2,790 usage events; the branch binary ran only against it, with a scratch `RELAIS_STATE_DIR`, never the live ledger). `relais report --json` → schema 17. Final text:
  - `review [claude-fable-5-1]: 17 of 30 reviews led to revise` / `cost: $61.70 total (est.), $3.63/catch`
  - `review [claude-opus-5-5]: 2 of 3 …` / `$1.64 total (est.), $0.82/catch`
  - `review [unknown]: 0 of 0 …, 1 with no verdict` / `cost unknown` (JSON `cost_micros: null`, never `$0.00`)
  - `repair [claude-haiku-5-5]: 0 of 5 …` / `$0.42 total (est.), -/catch`; `repair [claude-sonnet-5-5]: 4 of 6 …` / `$2.98 total (partial), $0.75/catch`
  - Relais moved its review work from fable to opus on 2026-11-06 (v0.48.1): $3.63 per caught review on fable against $0.82 on opus, on a small opus sample.
- Cost of the new queries: all 663 review-dispatch queries ran in 0.4 s in one session on the copy (about 0.6 ms each). The full text report took 65 s on a debug build; no baseline was measured, so the added share is bounded by the 0.4 s only.
- Two measured facts behind deliberate choices: 5 of 1,166 turn rows lack a cost (completeness is judged row by row); 5 of 798 review-phase rows are advisor calls, $3.06 of $454.54, not counted.
- The commit gate passed on every commit. It refused two: unformatted Rust (the check relais had reported as a gap) and a subject over 50 characters.

## Implementation review

- implementation-review: approve-with-changes twice; the second pass left two lows, both fixed or recorded here (54k–75k tokens, 34–75 s a pass). Fixed: a doc comment on the wrong test, the verification record, the CHANGELOG wording.
- backend: approve-with-changes twice (71k, 100 s; 85k, 34 s). Fixed: a test through the production writer, the advisor note. Deliberate, measured: row-by-row completeness (5 of 1,166), the per-dispatch query (0.4 s).
- lang:rust: approve-with-changes, then approve (65k, 56 s; 72k, 18 s). Fixed: integer money, a running completeness fold, the doc note. Deliberate: micros stay `i64`, the clones stay.
- unix and architect: unix approve-with-changes then approve (61k, 63 s; 74k, 26 s); architect approve twice (54k, 63 s; 69k, 35 s). Fixed: the zero-count row is documented. Deliberate: the dispatch-key range is commented, not shared.
- tui: approve-with-changes, then approve (60k, 86 s; 61k, 12 s). Fixed: cost on its own line, `<$0.01`, the cost-only row. Deliberate: the qualifier words.

## Decision log

- 2026-11-06: the first relais run blocked waiting 30 minutes for a host lease (other sessions held the slots); its kept candidate was verified in a second run (`needs_review`, four findings: no test recorded a non-zero cost, a comment promising such tests, an unknown cost shown as `$0.00`, the attempt cost carried twice). A revision run fixed them; it ended `needs_decision` on a verification gap (amont's `pre-commit-cargo-fmt` verdict not stamped on the verified tree) and was salvaged at f791fab2 after `cargo fmt`.
- Departures from the canonical plan, all reviewed: `attempt_usage` replaces `attempt_cost`, so the repair section and the harness section read one cost source; the cost sits on its own indented line under each row (the plan had one line, which ran past 100 columns on real model ids), a known cost under half a cent reads `<$0.01`, and a model that ran only an earlier dispatch reads `earlier dispatches only`; the total takes a qualifier by kind, `(est.)`, `(lower bound)` or `(partial)` (the plan said `(partial)` for anything not complete, but every real row is estimated).
- `tokens` is the input, output and cache tokens of the same usage rows. The plan pointed at the Reviewers section's figure, which counts plan-review subagent tokens, a different quantity.
- A repair with no verdict counts its cost, as a review dispatch with no verdict does: both spent the money. Advisor calls (`advisor:` keys) are not counted, with a `holds-until` at 5% of review-phase rows.
- Nothing outside relais parses the old one-line text (searched relais, amont-agent, decisions, dotfiles).
