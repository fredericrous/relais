# Settle budget-exhausted trials by cause: full reviews



**backend** (opus): r1 approve-with-changes (52k, 98 s), r2 approve-with-changes (61k, 29 s), r2 re-run approve (63k, 16 s), post-Codex approve-with-changes (68k, 19 s).
- r1: seat unix (high, accepted); subtract the suspend inside waits (medium, accepted); a new first-trial `Ledger` query (medium, accepted); the path is `docs/SPEC.md` (low, accepted); Suspend and Observe rows (medium, accepted); remove `Ceiling` (low, accepted).
- r2: the detail line cannot know the outcome (medium, accepted); `Limit` carries only `suspended_ms` (low, carried into implementation).
- re-run: only the replay has an `outcome=` end line; a live arm's outcome is the trial row (low, carried).
- post-Codex: a child run inherits the root's `suspended_ms` so far, plus a test (medium, carried).

**lang:rust** (opus, sliced Phases 1–2): r1 rework (59k, 90 s), r2 approve-with-changes (71k, 53 s), re-run approve (66k, 90 s), post-Codex approve (76k, 17 s).
- r1: production reads `LedgerBudgetClock`, so the readings go on `ledger::Clock` (blocking, accepted); engine-level suspend tests (high, accepted); `Ceiling` must be `pub` (high, moot once Ceiling was removed); the scheduler rebuild carries `cause` (medium, accepted); settle-path tests and the first-trial lookup (medium, accepted).
- r2: the detail line cannot know the outcome (high, accepted); name darwin's `clock_gettime(CLOCK_MONOTONIC)` behind a `// SAFETY:` wrapper (medium, carried).
- post-Codex: `Budgets::binding() -> Exhausted`, routed into `end_on_exhausted` (low, carried).

**tui** (opus): r1 approve-with-changes (25k, 16 s), r2 approve-with-changes (33k, 17 s).
- r1: the detail line names the cause (high, accepted, then narrowed); whole minutes (medium, accepted); the Piloted row reads the terminal (medium, accepted); seat tui (low, accepted).
- r2: the detail line must not say "counted as" (high, accepted); one wording per cause, pinned by the table test (medium, carried); exit 5 means a script reads `outcome=` (low, carried).

**unix** (opus): r1 approve-with-changes (28k, 38 s), r2 approve (37k, 15 s).
- r1: a sleep-counting monotonic clock, never the wall clock (medium, accepted); exit code 5 for every cause (low, accepted); the code-review reason (low, superseded).
- r2: the detail line goes to stderr with no escape codes off a TTY (low, carried); Observe bands of 70–90% and 5–20% with their sources (low, carried).

**codex** (gpt, final): approve-with-changes.
- A total-bound package stop is labelled `WallClock` (`scheduler.rs` 678 and 755) and would settle rejected (high, verified, accepted).
- The `Clock` defaults must return 0 without consuming `FixedClock` timestamps (medium, verified, carried).
- Replay's lookup uses `NewReplayTrial.arm_recipe_id`, since its row is inserted already settled (medium, carried).
