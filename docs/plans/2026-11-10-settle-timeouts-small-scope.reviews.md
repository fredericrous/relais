# Full reviews: relais settles a timed-out tree and a small overflow

## Full reviews (reference)

Panel seats on opus-5-5; seated from areas (cli, contract, lang:rust).

- **po** (27k, 43 s; r2 20 s): r1 approve-with-changes, with success and rollback numbers, weekly-rate baseline, a half-done-tree test and the ship order, all accepted. r2 carried: a success number for B (small `scope_exceeded` decisions ≤ 2/week, from ≈ 7) and one dated check 14 days after each release.
- **unix** (34k, 43 s; r2 12 s): r1 group-kill confirm and atomic snapshot writes (accepted, merged); CLI code and cancel-in-wait were duplicates. r2 carried: the SPEC §9 exit-code table and `--help` list 7 as "cancelled or settled", and the stderr assertion for the "preserved candidate" line.
- **tui** (32k, 48 s; r2 19 s): r1 code through `RunOutcome`, plugin text by code, report text line, tui back on code review, exit-7 CHANGELOG (all accepted). r2 #6 high = backend r2 #1.
- **backend** (62k, 79 s; r2 30 s; final 16 s + 38 s): r1 interrupted-transition lookup, retire the worktree, settle chains bounded, measurable checks (accepted). r2 #1 high (Settled plumbing in B3) applied. Carried: write the non-test `scope_exceeded` baseline count into the plan before release; `release_scenarios.rs` in A3's Files; A3 sentence trimmed to what A3 adds; `last_snapshot` set only after publication, fault test on both paths. Final: approve.
- **architect** (57k, 102 s; r2 25 s): r1 ADR-0010 sentence, count settles from `successor_intents`, failed-verify salvage route (accepted). r2 approve; carried: detect a settle's successor via `Ledger::intent_of_run` and the actor of its decision.
- **lang:rust** (65k, 173 s; r2 26 s): r1 `Awaited::TimedOut` + `Option` on `LaunchResult`, flag at worker call sites, snapshot split details, key names, resolve test, more tests (accepted). r2 approve; carried: `GroupKill::NothingLeft | Killed` both confirmed, exhaustive match.
- **codex** (gpt-6-astra, high effort): approve-with-changes. #1 high, every automatic successor review ≥ `Required` (`attempt.rs:900` confirmed). #2 high, candidate published last in one ledger transaction (`COALESCE` at `runs.rs:480`). Both applied; backend approved the delta.
