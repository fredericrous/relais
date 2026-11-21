# Full reviews: relais learning loop, Phase 8 entry

Sidecar of [2026-11-21-learning-loop-phase-8-entry.md](2026-11-21-learning-loop-phase-8-entry.md); the panel ran on 2026-11-21.

## Full reviews (reference)

Panel seated from the phases' files: backend, lang:rust (slice), tui, unix; all on opus-5-5. Reviewed body at round 1: sha d01c2bb26361, 2071 words.

### plan-review-unix
Round 1 (31k tokens, 40 s): approve-with-changes. 1 high: `cp` of a WAL ledger misses the `-wal`; use `sqlite3 ".backup"` (accepted). 2 medium: zsh has no pipefail by default (accepted). 3 low: remove the scratch dir (accepted). Round 2 (38k, 43 s): approve-with-changes; all resolved. New 1 medium: the baseline the Expected paragraph relies on was not in the block (accepted, exempt as unverifiable). New 2 medium: unchained steps; `set -eo pipefail`, one step per line (accepted).

### plan-review-backend
Round 1 (41k, 82 s): approve-with-changes. 1 high: unpriced sessions are the heavy ones, so a 10% exclusion bar on the savings gate (accepted; later superseded by Codex #1). 2 medium, remove: the v49 column protects no row (0 stops) and a forward-only migration blocks rolling back to 0.75.0 (accepted). 3 medium: VACUUM INTO (rejected: duplicate of unix #1). 4 medium: the Verification had no numbers (accepted; the main session then corrected "drawn+undrawn rises by 11" to drawn only, and Codex replaced it with a per-stratum oracle). 5 low: picture said 1 session, the plan says up to 4 (accepted). Round 2 (43k, 13 s): approve-with-changes; all five resolved; new 1 medium: Phase 4 schema 49 → 50 stale after the removal (accepted, exempt); new 2 low: test name still says stops (carried); new 3 low: scratch rationale stale (accepted, exempt). Re-bind (46k, 7 s): approve. Final re-bind after Codex (55k, 17 s): approve-with-changes, no blocker; 1 medium: the SQL oracle hardcodes the three strong outcomes instead of excluding `unknown` and `answered` (carried: `TaskOutcome` has exactly five variants today).

### plan-review-language (rust, slice)
Round 1 (55k, 87 s): approve-with-changes. 1 high: `tests/it/holdout_report.rs:139` pins schema 19 and the pre-commit gate runs the full `cargo test` (accepted). 2 medium: `(None, Some(_))` arm line contradicts itself (accepted). 3 medium: JSON hides the ratio's denominator; add `priced_completed` (accepted). 4 low: `.as_deref()` (accepted). 5 low: clippy `-D warnings` (accepted). 6 low: assert the exact interval (accepted). Round 2 (63k, 22 s): approve; all resolved; no Rust loose end from the stop-version removal. New, all low, carried: test name; a later v3 column defaults to 1 for rows older than the release; print the excluded block only when `cost.is_some()`.

### plan-review-tui
Round 1 (36k, 59 s): approve-with-changes. 1 high: the arm line's fixed text already fills 80 columns, so the cost figure wraps; own line, test filter dropped (accepted). 2 medium: model ids cut with `~` cannot be pasted into machine.toml; one full id per line (accepted). 3 medium: header wording (accepted). 4 medium: stop line version mismatch (rejected: moot after backend #2). Round 2 (38k, 9 s): approve-with-changes; all resolved; new 5 medium: the header test needs a fixture with a stratum, since an empty exploration renders nothing (carried).

### codex (gpt-6-astra, high effort, read-only; `git status` clean after)
approve-with-changes (572 words). 1 high: a session-count threshold cannot bound omitted cost; the savings gate is inconclusive whenever either arm has an unpriced session (accepted, applied; backend confirmed it supersedes its bar). 2 medium: the manual check compared displayed sums over five retained strata with the wrong population; a whole-ledger SQL oracle per (band, scope) under both policies (accepted, exempt as unverifiable; backend's final finding refines its strong-outcome predicate, carried).

### Carried into implementation (5)
1. Rename `the_exploration_section_names_its_label_policy_and_the_stops` to `…_names_its_label_policy`.
2. That test's fixture holds at least one stratum (an empty exploration renders nothing at `report.rs:168`).
3. A later label policy v3 that adds a stop column defaults it to 1 for rows older than that release.
4. Print the excluded-sessions block only when `cost.is_some()` (a fully unpriced arm already names its models).
5. The SQL oracle's strong predicate is `outcome NOT IN ('unknown','answered')`, as the code defines it.

