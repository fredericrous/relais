# Full reviews: Tidy what the shared-state commands print

`greenbelay plan-review record` and `plan-apply` found no transcript (a
programme ticket agent's session); the findings were applied by hand and are
kept here.

## Round 1 (body f2e2eac74164, 1218 words)

- unix: approve-with-changes (34k, 43 s). 1 high: `tests/it/learning_cycle.rs:605/618` read the installer lines on stdout (accepted). 2 medium: `decide --json` prints the receipt-skipped warning in plain mode only (accepted). 3 low: refusal prefix `relais decide:` vs main's `relais:` (accepted, stated). 4 low: `println!` panics on a closed pipe, use `writeln!` and treat `BrokenPipe` as success (accepted).
- backend: approve-with-changes (41k, 60 s). 1 high: `learning_cycle.rs` (dup, accepted). 2 medium: Phase 4 next step if PR 610 stays open (accepted: Phases 1–3 land first). 3 low: prefixes per path (dup, accepted). 4 low: pilot exit equals the object's `exit` (accepted).
- tui: approve-with-changes (37k, 64 s). 1 high: `learning_cycle.rs` (dup, accepted). 2 high: the salvage next step asks for what the node cannot know (accepted with another edit: the patch's "that run says so" is false across nodes; the line prints the command and says a salvage elsewhere does not show here). 3 medium: counter tail names no place (accepted: points to docs "Where to look"). 4 low: prefixes (dup, accepted).
- lang:rust: approve-with-changes (62k, 95 s). 1 high: `learning_cycle.rs` (dup, accepted). 2 high: no binary test of the `Refusal` path under `--json` (accepted). 3 medium: `launch.rs` line into a pure builder with a test (accepted). 4 medium: say which clause `trust grant`'s line follows (accepted: result sentence, stdout). 5 low: diagram prefix (accepted). 6 low: minutes computed from `DECISION_GRANT_TTL_SECS` (accepted).

## Round 2 (body cc5dd9591750, 1470 words, ×1.21)

- lang:rust: approve (68k, 14 s). All 6 resolved. New 7 low: no row tests the receipt-skipped line under `--json` (carried into implementation).
- backend: approve-with-changes (47k, 22 s). All 4 resolved. New 5 low: same receipt-skipped row (carried); 6 low: item 6 must add a section headed `## Where to look` (carried).
- unix: approve (39k, 12 s). All 4 resolved; no new finding.
- tui: approve-with-changes (43k, 16 s). All 4 resolved. New 1 medium: put the wait before `<next>` so "once the hold ends" does not attach to "does not show here" (carried into implementation; the code orders it so).

Codex: not run (programme ticket agent).
