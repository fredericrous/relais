+++
type = "task"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais"]
+++
# The correction charge read rule's fallbacks are tested and visible

## Question

t12's read rule (`Ledger::router_correction_charges`) falls back
silently on damaged rows: a candidate task whose `files_json` does not
parse is skipped, and a correction row whose `at` or `files_json` does
not parse is kept with nothing charged and `legacy = true`. Only the
unparseable-time case has a test. And `legacy` now means both "written
before the plugin sent files" and "could not be read". Should the
fallbacks be tested and reported, and should `legacy` split from
`unparsed`? (t12 implementation review: lang:rust 9–10, backend 1–2.)

## Look at

- `crates/relais/src/ledger/router.rs`, `router_correction_charges` and
  `files_from_json`.
- `CorrectionCharge` and its readers. Today only the tests read it. t03/t05
  (the gates) will be the first real readers.
- t12's plan, `docs/plans/2026-11-23-correction-attribution.md`, its
  Implementation review.

## Answer looks like

Two tests:
- a stored correction row with `files_json = 'not json'` comes back kept,
  uncharged, its task not in `corrections_charged_away`;
- a candidate with corrupt files is skipped while another candidate still
  charges.

Then one of these two, decided before t05 reads the struct:
- `CorrectionCharge` gains `unparsed: bool`, with `legacy` meaning files
  that parse but are empty;
- `legacy` stays as it is, and its doc comment says it also covers rows
  that could not be read.

Also either a warning per skipped row, or a count returned with the
charges.

## Resolution

The fallbacks are tested, `legacy` is split from unparsed rows, and the
skips are counted. The split is an enum rather than the ticket's
`unparsed: bool`: the code review (lang:rust, rule
`types.variants-as-unions`) found that two flags which must never both be
true are a state the type allows. Decided by agent (ADR-0080); the plan is
`docs/plans/2026-11-23-charge-read-fallbacks.md`.

- `CorrectionCharge`'s `charged` and `legacy` become one field,
  `read: ChargeRead`, with three cases:
  - `Charged(tasks)`: the earlier tasks charged, sorted; empty when none
    overlaps.
  - `Legacy`: the time and files parse and the files are empty.
  - `Unparsed`: the row's `at` or `files_json` does not parse. The row is
    kept with nothing charged, so its task stays `corrected`.

  Only a non-empty `Charged` charges a correction away.
- `Ledger::router_correction_charges` returns `CorrectionCharges { charges,
  skipped_candidates }`. `skipped_candidates` counts the candidate tasks
  skipped because their `ended_at` or `files_json` does not parse. It is a
  count, not a warning, so a test can tell a skipped candidate from one
  that did not match, and the read path writes nothing to stderr. The count
  is ledger-wide, not per correction row: t05 reads a non-zero count as
  "some charges may be missing".
- New tests (the candidate test damages one candidate's files and
  another's `ended_at`, so both skip causes are counted):
  - `a_row_with_unreadable_files_is_kept_unparsed_and_keeps_its_task`: a
    good row plus a corrupted one. The test fails if the row is dropped,
    because b would then be charged away.
  - `a_candidate_with_unreadable_files_is_skipped_and_counted`: three
    matching candidates, two damaged. The third is charged, and the count
    is 2.
- Tightened tests: the unparseable-time test and the legacy test now
  assert their `ChargeRead` case.
- Mutation checks:
  - with `Unparsed` read as `Legacy`, both unparsed-row tests fail;
  - without the skip increment, the candidate test fails.
- The schema and the ledger version are unchanged, and there is no release
  of its own: only tests read the struct until t05.

Notes for t05 from the panel:
- When the count or `unparsed` reaches a person, seat tui and unix. The
  message goes to stderr, names the damaged task and the command to
  inspect it, and has a documented exit status.
- t05 decides whether the skipped task keys should be returned, not just
  the count.
- t05 decides whether `Unparsed` should name the column that failed. The
  review left it out because nothing reads it yet.
