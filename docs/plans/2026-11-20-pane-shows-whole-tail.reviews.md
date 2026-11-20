# Pane shows the whole pre-clear tail — full reviews

## Full reviews (reference)

- **backend** (opus): R1 approve-with-changes (44k, 81 s): adds=screen [blocking], text before LAST_MESSAGES [high], two-button Row, opening position (rejected: opens at top), ROWS_CAP, open event, Actual column, Code review reason. R2 approve-with-changes (50k, 30 s): carried A events.ts missing from Phase 1, B Actual cells, C caption "three cuts", D 500-row ROWS_CAP row. R2 re-binds: inline branch into the Phase 1 spike [high, applied] (58k, 24 s); approve-with-changes (55k, 58 s); after Codex approve-with-changes (64k, 36 s), carried: `pane.ts:279-282` comment and which agent Buttons survive at six rows.
- **lang:typescript** (opus): R1 approve-with-changes (43k, 47 s): header Row variant, hideTail resets shown, openTail bumps generation, live spike first, trimTail edge rule, register.ts dropped; carried: 60-row test vs ROWS_CAP. R2 approve (47k, 31 s); carried: N rows counts all rows, `… K earlier rows` at the cap.
- **tui** (opus): R1 approve-with-changes (35k, 86 s): inline frame, `… N earlier rows` only at ≥2 rows, `· N rows` title, `label: 'back'`. R2 approve (38k, 20 s); carried: table cells, N after the cap with `· oldest cut`.
- **unix** (opus): R1 approve-with-changes (49k, 64 s): cleanLine not cutLine (240-char cut), trimTail room 0/1/2, ESC/BEL test. R2 approve (54k, 28 s); carried: `'x\n'.repeat(1000)` at the cap.
- **ux-research** (opus): R1 approve-with-changes (30k, 37 s): separate view unbacked by research (recorded); carried: cut marker visible, first-click on open position, inline frame max height.
- **game-ux** (opus): R1 approve-with-changes (27k, 26 s): inline 100×30 check [high], hint names top start and Esc, Esc/header-width/title rows; carried: none beyond the rows added.
- **codex**: approve-with-changes: drop the LAST_MESSAGES cutoff [high], reserve header plus newest tail row [high], both applied; carried: `cutBytes` drops text after combining marks (wrap on cells and bytes).
