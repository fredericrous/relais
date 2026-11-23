+++
type = "task"
mode = "afk"
status = "open"
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
