# Full reviews: one SharedState per relais replay

Round 1: backend approve-with-changes (high: a fourth build, `tuning_bounds` at replay.rs:93; medium: verification cannot tell main from branch; medium: no reasons for omitted profiles); lang:rust approve-with-changes (ugrep needs ERE or fixed strings; no test tells one build from three); tui approve; unix approve (verification thin). All accepted.

Round 2: backend approve-with-changes, lang:rust approve-with-changes; findings medium or low, carried into implementation: the Picture should show `tuning_bounds_of`; the pilot runs with `--dry-run` (exit 0 on both) and without (same exit 3 on both, stderr naming the missing external state, proving the build was reached).
