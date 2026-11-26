# Full reviews: Read the shared day counter

`greenbelay plan-review record` cannot bind a programme ticket agent's panel
(no transcript); the findings were applied by hand and are kept here.

## Round 1 (body 551ad6448bae, 1071 words)

- backend: approve-with-changes. 1 blocking `adds=contract` (accepted). 2 high log rotation hides old days (accepted: `covers_from`, `log_incomplete`). 3 high waiting settles make a day look short (accepted: subtracted by `since` day). 4 homelab file in Phase 4 (accepted). 5 low ms bound format (accepted).
- lang:rust: approve-with-changes. 1 high log coverage (dup). 2 high waiting (dup). 3 high `run_all` has no clock (accepted, then refined by architect). 4 medium one bounds rule (accepted). 5 medium `stamped` pub(crate) (accepted). 6 low CHANGELOG names the method (accepted).
- unix: approve-with-changes. 1 high coverage (dup). 2 medium rotation mid-read: inode once (accepted). 3 low BrokenPipe (accepted). 4 low `--days` exit 2 (accepted).
- tui: approve-with-changes. 1 high day-edge drop reads `short` (accepted: `edge`, `partial`). 2 medium text columns and width (accepted). 3 medium stderr lines (accepted). 4 medium text binary check (accepted).

## Round 2 (body 86b60d871230, 1412 words; new seats architect, po)

- architect: approve-with-changes. 1 high portable case cannot know the day start (accepted: clock-free `a_counter_adds_up`). 2 high `None` vs `daily_spent` link (accepted). 3 low CHANGELOG names the suite (accepted).
- po: approve-with-changes. 1 high single-node ledger vs global counter (accepted: `counter_above_ledger`, multi-node summing). 2 medium `cutover` marker (carried). 3 medium `+00:00` and `Z` bound formats (carried). 4 low Outcome section (carried).
- backend: approve-with-changes. 6 Picture stale, 7 test row cell (accepted); 8 `edge` hides a real shortfall (carried: two-day gap at most 5%); 9 line-final token (carried).
- lang:rust: approve. New low: line-final token (carried).
- tui: approve-with-changes. Picture stale and EPIPE test (accepted).
- unix: approve-with-changes. EPIPE test cannot trigger via `head` (accepted).

## Round 2 final (body 4471a7b2fdc1, 1558 words, x1.45)

- backend: approve. All resolved; new low: Picture verdict line lacks `counter_above_ledger` and `no_ledger_spend` (carried).

Carried into implementation (8): `edge` rule needs the two days' gap at most 5%; `cutover` marker; `+00:00` and `Z` bound formats in the ledger test; line-final `dropped_micros` token only; an Outcome section; Picture verdict line completeness; text row width at its widest; `waiting.error` in a test.

Codex: not run (programme ticket agent).
