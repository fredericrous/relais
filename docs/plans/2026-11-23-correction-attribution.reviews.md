# Full reviews: A correction records its files and charges one task

Round 1 (opus): backend approve-with-changes (30k, 36 s), tui approve-with-changes (25k, 17 s), unix approve-with-changes (25k, 20 s), lang:typescript approve-with-changes (36k, 41 s), lang:rust approve-with-changes (43k, 71 s).
- backend: 1 version skew (accepted as a Your-steps statement), 2 observability (accepted: pilot counts, `legacy`), 3 numbers (24 h window rejected: §5 rule; query time accepted), 4 order and 0.72.0 record tests (accepted), 5 remove the `queueCorrected` re-send (rejected: crash guard), 6 source of `stop` (accepted).
- tui: 1 reinstall flags (rejected: the install help lists the user and write flags), 2 doctor line (accepted), 3 non-goal on showing a charge (accepted), 4 `legacy: bool` (accepted).
- unix: 1 [high] skew exit 2 (rejected: plugin embedded in the binary, docs/INTEGRATIONS.md:3-12), 2 triple send (accepted), 3 `stop` over 999 (accepted), 4 live ledger sha (accepted).
- lang:typescript: 1 [high] `/flag` (accepted), 2 cap and one hash call (accepted), 3 round-trip and `at` tests (accepted), 4 await in `endTask` (accepted).
- lang:rust: 1 chrono window, 2 read-merge-write union, 3 SET clause test, 4 `TaskKey` and `Result` (all accepted); 5 deny_unknown_fields (noted).

Round 2 (opus, resumed): unix approve (31k, 14 s); lang:rust, tui, lang:typescript, backend approve-with-changes.
- lang:typescript: [high] one `hashFiles` over the union cannot be split per entry (accepted: one call per entry).
- carried into implementation: lang:rust, parse millisecond `at` and skip an unparsable `ended_at`; lang:rust, clear the temp table per call or filter `stop` in Rust; tui, hash the `.backup` copy, not the live file; tui, say a capped entry was already sent; unix, cap vs a ninth correction; backend, `files_present` vs empty files for `legacy`, `BEGIN IMMEDIATE` for the union, first 8 plus `corrections_full`, a subagent-edit test, drop the "showing a charge" non-goal, per-entry `hashFiles` under `Promise.all`; backend 4 (prompt path) withdrawn.

Codex (high effort): approve-with-changes.
- [high] a flag could still be charged away (accepted: `corrections_charged_away` excludes a task with a `flag` row).
- [high] a late early snapshot could overwrite `outcome_uncorrected` (accepted: ended records only, first kept).
- [medium] capture the session before awaiting in detached producers, drain before the final flush (carried into implementation).

Backend re-runs on the final body: approve-with-changes (38k, 81 s; 42k, 12 s); carried: `flagged: bool` on `CorrectionCharge`.
