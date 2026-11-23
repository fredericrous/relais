---
status: done
branch: prog/phase8-learner/t14-charge-read-fallbacks
repos: [relais]
adrs: []
programme: docs/programmes/phase8-learner
ticket: t14-charge-read-fallbacks
plan-id: p-01M84V47VNTPEM8753WH3EE93W
---
# The correction charge read rule's fallbacks are tested and visible

## Review panel

👉 **Decide:** none — approve if t05 should read an unreadable correction row as `unparsed`, apart from `legacy`, with skipped candidates counted.
📍 relais · 1 phase · no release · next: Phase 1. Panel: backend, lang:rust, tui, unix (opus).
**Changed by review:** Verification rows as input → expected; the count is ledger-wide; call sites and re-export named.
**Verdicts:** final: 2 approve, 2 approve-with-changes; 4 carried; words 570 → 636 (×1.12); remove 0, merge 0.
📄 Full reviews: `2026-11-23-charge-read-fallbacks.reviews.md`

## Picture

```
 candidate A (router_tasks)       correction of B (router_reassess)
 ended_at, files_json parse?      at, files_json parse?
   no  -> skipped_candidates+1      no            -> unparsed, charged []
   yes -> candidate                 yes, files [] -> legacy, charged []
                                    yes, files    -> charged to the
                                                     candidates it overlaps
                 |
 CorrectionCharges { charges, skipped_candidates }
                 |
 corrections_charged_away: an uncharged row (unparsed, legacy,
 or no overlap) keeps B corrected
```

Each damaged row now lands somewhere a reader of the struct can see: a skipped
candidate in the count, an unreadable correction row marked `unparsed`.

## Goal

`Ledger::router_correction_charges` (t12) falls back silently on damaged
rows, and its `legacy` flag means two things. Before t05 (the gates) reads
`CorrectionCharge`, each fallback gets a test, a row that could not be read
is told apart from one written before the plugin sent files, and the
candidates skipped for damage are counted in the result.

## Non-goals

- No schema change and no ledger version step: a read-path change only.
- No release of its own: nothing outside tests reads the struct until t05.
- `corrections_charged_away` keeps its signature and its rule.
- The write path (`record_router_batch` refusing a corrupt stored list) is
  unchanged.

## Code review

- relais: backend, lang:rust (unix, tui left out: no command line, no output a person reads)

## Behaviour

- `CorrectionCharge` gains `unparsed: bool`: the row's `at` or its
  `files_json` does not parse. Its `charged` is empty, and its task stays
  `corrected`.
- `legacy` becomes: both parse and the files list is empty (written before
  the plugin sent files). `legacy` and `unparsed` are never both true; every new and tightened test asserts it (`!(legacy && unparsed)`).
- `router_correction_charges` returns `CorrectionCharges { charges,
  skipped_candidates }`. `skipped_candidates` counts candidate tasks (main,
  ended verified or accepted) skipped because their `ended_at` or their
  `files_json` does not parse. A count, not a warning: a test can tell a
  skipped candidate from one that never matched, and t05 can report it.
  It is global, not per correction row: t05 reads a non-zero count as
  "some charges may be missing" (a missed charge leaves B `corrected`).
- The doc comments of `router_correction_charges`, `legacy` and `unparsed`
  say this.

The tests are the Verification rows. In the corrupt-row test the good row's
`at` sorts first, since rows come back ordered by `at`.

## Phases

- [x] Phase 1 — split the flag, count the skips, add the two tests and
  tighten two existing ones: `crates/relais/src/ledger/router.rs`,
  `crates/relais/src/ledger/mod.rs` (re-export `CorrectionCharges` at line 81).
  Call sites move to `.charges`: the `correction_charges` helper, `charge_case`,
  and `a_correction_is_charged_to_an_earlier_verified_task` (its struct literal
  gains `unparsed: false`). The tightened tests are that one (asserts
  `!unparsed`, `skipped_candidates == 0`) and the unparseable-time and legacy
  tests.


## Verification

| Check | Input | Expected |
|---|---|---|
| corrupt correction row | task b with a good `["x"]` row and a second row set to `'not json'` | two charges; the corrupt one `unparsed && !legacy && charged.is_empty()`; b not in `corrections_charged_away` |
| corrupt candidate | two verified candidates both `["x"]`, one set to `'not json'` | `charged` is exactly the other; `skipped_candidates == 1` |
| flag split | unparseable-time row; empty-files row | first `unparsed && !legacy`; second `legacy && !unparsed` |
| suite | `cargo test -p relais --lib ledger::router` | all pass; the two new tests fail when run against the pre-change code |

## Decision log

- 2026-11-23 — `unparsed` split from `legacy`, and a count rather than a
  warning, because t05 will read the struct and a count is testable; decided
  by agent (ADR-0080) in the t14 ticket.
- 2026-11-23 — the fleet pack refresh (`.adr/packs/decisions.pack`,
  `HEADS.md`) rides this branch, as worktree-task's Start step requires.
- 2026-11-23 — scope change from the code review (lang:rust, high,
  `types.variants-as-unions`): `charged`, `legacy` and the planned
  `unparsed: bool` become one `read: ChargeRead` field (`Charged(tasks)`,
  `Legacy`, `Unparsed`), so the "never both true" rule is enforced by the type,
  not by asserts. The Behaviour bullets on `unparsed`/`legacy` read as this
  enum. The candidate test also damages an `ended_at` (backend, lang:rust).

## Outcome

Phase 1 is shipped on this branch. The flags became one `ChargeRead` enum (Decision log), so the Behaviour bullets on `unparsed`/`legacy` read as its `Unparsed`/`Legacy` cases. Each check is input → expected → actual:

- corrupt correction row → kept and uncharged, b stays corrected → `a_row_with_unreadable_files_is_kept_unparsed_and_keeps_its_task` passes.
- corrupt candidate (files on one, `ended_at` on another) → the third is charged, count 2 → `a_candidate_with_unreadable_files_is_skipped_and_counted` passes.
- flag split → unparseable time reads `Unparsed`, empty files read `Legacy` → passes.
- `cargo test -p relais --lib ledger::router` → all pass → 33 passed, 0 failed.
- mutation `None => Legacy` → unparsed-row tests fail → 2 failed, rc 101.
- mutation with no skip increment → candidate test fails → 1 failed, rc 101.
- `cargo clippy -p relais --all-targets -- -D warnings` → clean → rc 0.
- pre-commit gate → passes → passed. On the second commit the host was full, so it deferred clippy and cargo-test to CI; the runs above are on that same code.

## Implementation review

- implementation-review: approve-with-changes, then the delta: approve-with-changes. Phase tick and Outcome fixed here; the pack refresh is in the Decision log. 45k + 54k.
- backend: approve. The `ended_at` skip case was fixed. Deliberate, left to t05: a read snapshot across the three SELECTs, and naming the damaged tasks. 62k.
- lang:rust: approve-with-changes, then the delta: approve. Fixed: the bools became the `ChargeRead` enum, and the `ended_at` case was added. Deliberate: the failing column (t05 decides it) and a doc-wrap nit. 48k + 52k.

<!-- panel: repos=relais adds= reviewers=backend,lang:rust,tui,unix body-sha=242d44b50714 -->
