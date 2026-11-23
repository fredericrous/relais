# Full reviews: The correction charge read rule's fallbacks are tested and visible

## Round 1

- **backend** (opus): approve-with-changes, 28k, 28 s.
  1. medium, Verification rows as input → expected: accepted.
  2. low, `skipped_candidates` is ledger-wide, so t05 reads it as "some charges may be missing": accepted.
  3. low, "visible" means "visible to a reader of the struct": accepted.
  4. low, name the call sites: rejected, a duplicate of lang:rust#1.
- **lang:rust** (opus): approve-with-changes, 31k, 36 s.
  1. medium, name the call sites and the `CorrectionCharges` re-export: accepted.
  2. medium, assert the `!(legacy && unparsed)` invariant, or make the pair an enum: accepted the assertion. The enum is left to t05, since the ticket names the bool split.
  3. low, the corrupt row's `at` sorts after the good row's: accepted.
  4. low, document the ledger-wide count: rejected, a duplicate of backend#2.
- **tui** (opus): approve, no findings, 23k, 26 s. Note for t05: give it a tui seat when the count reaches a person.
- **unix** (opus): approve, no findings, 22k, 25 s. Note for t05: stderr and an exit status when the count is shown.

## Round 2

- **backend**: approve-with-changes, 33k, 19 s. All round-1 findings resolved. Carried into implementation:
  - low: the new tests fail to compile on the pre-change code, so check them against a mutation instead;
  - low: the invariant is asserted only where a test returns a correction row.
- **lang:rust**: approve-with-changes, 36k, 22 s. All round-1 findings resolved. Carried into implementation:
  - medium: mutation checks (`unparsed` forced to false, skip counter not incremented);
  - low: a clippy `-D warnings` gate row.
- **tui**: approve, 25k, 13 s.
- **unix**: approve, 24k, 11 s.

codex: not run (programme ticket, skipped for cost).
