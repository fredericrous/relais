+++
type = "research"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais"]
+++
# What reads relais's baseline, verification and review results today

## Question

Which parts of relais consume what the baseline, the candidate verification
and the semantic review produce, and which of them break when those stages
are removed?

## Look at

- `crates/relais/src/runner/` (`baseline.rs`, `verification.rs`,
  `machine.rs`, `engine.rs`, `attempt.rs`): the state machine and what each
  stage hands the next.
- `crates/relais/src/learn/` (`dataset.rs`, `features.rs`, `evaluate.rs`):
  where the training label comes from.
- `crates/relais/src/ledger/` and its tables (`outcomes`, `check_timings`,
  `verification_snapshots`, `review_outcomes`, `review_parts`, `holdout_*`,
  `decisions`).
- The plugin (`plugin/`) pane and `decide` flow: `needs_review`,
  `review_findings`, orchestrator decisions.
- `SPEC.md` §10 and §18, and ADR-0003, ADR-0006, ADR-0010, ADR-0045.

## Answer looks like

A table with one row per consumer: the file or table, what it reads (baseline
failures, candidate verdict, review findings), what it does with it, and
whether it would be removed, moved to amont, or rewired to an amont verdict.
It ends with the list of SPEC sections and ADRs that must be amended.

## Resolution

2026-11-10, Explore agent (grep and targeted reads, no LSP). The relais
`inspect` run for this ticket was cancelled while it was still queued.

**Already built:** skipping a stamp-proven base (`runner/baseline.rs:64-91`,
`verify/stamps.rs`) and reading candidate stamps.

**The reviewer is two dispatches:**
- the patch reviewer (`runner/review/*`, about 2,400 lines, called from
  `runner/verification.rs:63-175`);
- the inspect report review (`judge_inspection`,
  `runner/verification.rs:513-620`; SPEC §10). It is **the only
  verification an `inspect` run has**, and it settles `llm_review`
  evidence.

**What happens to each consumer:**
- **Remove:**
  - the review branch of `verification.rs`;
  - `review_required` (`attempt.rs:900`, `scheduler.rs:1060`);
  - review repairs (`attempt.rs:281`, `spend.rs`);
  - the review observations in `machine.rs` (`ChecksAndReviewPassed`
    becomes checks-only);
  - `runner/review/*`, the `review-parts` successor and decide paths, and
    review floors and risk hints (`route/`, `policy/`);
  - the 19 `relais-reviewer-*` agent files and the pane's review row.
- **Keep as legacy values, stop emitting:** `State::NeedsReview` and the
  `Review*` reasons (`lifecycle.rs`), `ReviewRecord` in receipts, the
  `review_parts` and `verification_snapshots` tables (expand-only), and
  `needs_review` in the plugin's states.
- **Keep:** `ledger.review_outcomes`, which is not the reviewer. It holds
  amont-agent plan-review labels.
- **Learner:** the learner reads no review verdict. Only the salvage-positive
  rule keys on historical `needs_review` with `review_findings`, so new data
  just stops adding to it. `LABEL_POLICY_VERSION` stays 4.
- **Rewire the baseline consumers** to a base run on demand: the new-failure
  filter, `BaselineFailureNotWaived`, `guarded_preexisting`
  (`attempt.rs`, `machine.rs:499-603`, `verify/mod.rs:343-400`),
  `baseline_resource_exhausted` (`exhaustion.rs:174-239`), and the reuse of
  the base's results for a tree identical to the base
  (`verification.rs:720-906`).

**Surprises that feed t05:**
- Removing the reviewer leaves `inspect` runs unverified.
- Review is forced whenever a candidate edits test or verification inputs
  (`attempt.rs:900`). That is the guard against a worker weakening the
  tests, and it goes with the reviewer.

**To amend:**
- SPEC §4, §5, §6, §9, §10, §11, §17, §18 and §23.
- relais ADR-0004 (`needs_review` stops), ADR-0006 (check), ADR-0010
  (budgets cover review) and ADR-0011 (review findings and repair).
- decisions ADR-0045 (holdouts cite `llm_review`).
- decisions ADR-0064 is untouched: greenlift keeps acceptance.
