# Full reviews: relais reads greenspotter names

Round 1: backend, lang:rust, platform, tui, unix, all approve-with-changes (28-41k tokens, 31-63 s).
Round 2 (resumed): backend approve-with-changes, lang:rust approve, platform approve, unix approve, tui approve-with-changes.

Carried into implementation:
- backend (low): Phase 2's "lists candidates" check names a commit adding a `.unwrap()` to a `.rs` file, the reviewer prompt shows it, `rulings.jsonl` gets a `greenspotter.ruling/1` line and no candidates gap.
- tui (low): Phase 2 runs the `RELAIS_ADR_LINT` grep as a step, not only in Behaviour.
- tui (low), rejected: no new gap text naming the old variable or an install step; ADR-0072 allows no alias and the release notes carry the removal.
