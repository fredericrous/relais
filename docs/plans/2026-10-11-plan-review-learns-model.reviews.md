# Plan review learns which model each reviewer needs — full reviews

Round 1
- **backend** (36k, 212 s): approve-with-changes. Blocking: Phase 3 overturns learning-loop D6, so it becomes a person's decision before Phase 3. High: adoption arithmetic and strata that will not fill (pooled class first); shadow twin must review a snapshot of the body; `miss` needs a declared role→area table and a section unchanged since round 1. Medium: join key and record input, ingestion owner. Low: fallback join. All accepted.
- **lang:rust** (51k, 126 s): approve-with-changes. High: `completed_agents` captures the agent id only under `resumable`; `Registry::promote` has one global pointer, so per-class rows. Medium: separate `review_outcomes` table, `LabelSource::Review` and `DATASET_VERSION`; pin `ONE_SIDED_95`; dedupe on agent id. All accepted.
- **tui** (23k, 30 s): approve-with-changes. High: report section needs width, row cap, `--json`, empty state; `record` output and exit codes. Medium: `review-outcome` flags and caller; model visible in the panel line. Low: threshold from config. All accepted.
- **unix** (33k, 60 s): approve-with-changes. High: dedupe race, so `flock`. Medium: `open_private` and file modes, exit codes for both commands, ingest trust boundary, stored-prompt retention (ADR-0002). Low: SIGINT mid-write. All accepted.

Round 2
- **backend** (30k, 25 s): approve-with-changes. Resolved all 7. New high: threshold arithmetic (10% needs 29, one miss clears at 30) and `unknown` labels excluded from n; both applied. Medium: no cost payback arithmetic; partial transcript semantics unspecified. Low: remaining count drifts after a miss.
- **backend, final body** (25k, 25 s): approve-with-changes. Both resolved; arithmetic re-derived. No blocker.
- **backend, delta after your decisions** (31k, 43 s): approve-with-changes. D6 resolved. High: `miss` needs the round-1 body, which the skill's scratch copy does not keep; applied. Medium: remit table owner, `LabelSource::Review` timing. Stale header fixed.
- **backend, final body** (25k, 12 s): approve-with-changes. High resolved. Medium: rounds 2 and 3 body shas, `bodies/` retention. Low: missing-body bias, header wording. No blocker.

carried into implementation
- Cost payback: (saving per cheap review) − (twin cost ÷ 6) − (exploration misses), with a break-even in plans.
- `record` with a missing agent id writes the rows that resolved and exits 1 naming the missing ones.
- The remaining count in the report is computed from the current miss count k.
- One definition of `unknown` in Behaviour 2; Phase 0(d) measures pushed and unchanged-section; the estimate becomes a range (about 13–19 plans at 0.5).
- Decision-log entry about the labelled fraction moves to the Phase 0 checklist.
- Phase 4 verification seeds `n_min` clean rows from config, not a literal 19.
- Every round's row stores the body sha it reviewed; `record` writes `bodies/<sha>` on each call (write-once); the miss diff takes the earliest row's sha for the plan path.
- `bodies/` gets the 90-day prune and 0700 mode of ADR-0002, with a test.
- `review-outcome` counts `unknown` rows caused by a missing body separately.
- The remit table has an owner: Phase 1 builds and tests it, Phase 2 states how relais reads it.
- `LabelSource::Review` and the `DATASET_VERSION` bump move to the Phase 8 notes unless the dataset must be rebuilt now.
- Leftover wording: "until a class is decided", "adds labels and exploration", "which ran cheap", "Phase 3–4 checks" become Phase 8 or neutral wording.
