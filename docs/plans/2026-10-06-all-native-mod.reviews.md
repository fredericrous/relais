# relais: every dispatch native, through a relais mod — full reviews

## Full reviews (reference)

**lang:typescript, round 1** (approve-with-changes, 40k tokens, 37 s), applied:
- [high] `$.process.spawn` yields chunks, not lines → a line splitter with remainder;
- [high] no `node:fs` in a hooks module, and `process.run` times out at 30 s → stdin payloads, explicit `timeoutMs`, retries;
- [medium] spawn correlation → `next.origin` + the dispatch id in `description`;
- [medium] the notification drop → the exact format plus `spawnedBy`;
- [medium] "JS checked by .d.ts" had no checker → `register.ts`, deps injection, `plugin validate` in `make check`;
- [low] pin the Claude Code range.

**backend, round 1** (approve-with-changes, 42k tokens, 65 s), applied:
- [high] the verdict path → the `done` line and `mcp__relais__status`;
- [high] stray stdout → `--protocol`, one writer, a parse test;
- [high] several `turn.complete` per run;
- [medium] spawn correlation;
- [medium] coordinator down → retries, nothing on disk;
- [medium] rollback;
- [medium] `RELAIS_SESSION` vs `RELAIS_SESSION_ID` → `RELAIS_HOST` + the hello heartbeat;
- [medium] the mod API as a dependency;
- [low] numbers.

**backend, round 2** (approve-with-changes, 41k tokens, 41 s), applied:
- [high] "the last turn" is unknowable → the end signal from `$.agent.list()` and usage summed;
- [medium] a revert double-counts;
- [medium] TS/JS wording;
- [medium] the `claude` binary in CI → S0;
- [low] M2 subcommands;
- [low] driven → expected for isolation.

**backend, bind 1** (approve-with-changes, 37k tokens, 39 s), applied:
- [medium] a stale plugin test;
- [medium] repair → new-dispatch mapping, the end only after a turn;
- [low] an S0 item for `$.agent.list()`;
- [medium] the revert cleanup did not stick → keep `native_usage_messages` written.

**backend, bind 2** (approve-with-changes, 34k tokens, 22 s, on the final body): carried as binding items above:
- [medium] a repair can hang when no turn follows the continue;
- [medium] "no transcript parsing" contradicts the id read; missing or partial transcript handling;
- [low] the agent-file skip is added code.

**lang:typescript, round 2** (approve-with-changes, 33k tokens, 26 s, on the final body): round-1 items 1–4 and 6 resolved, 5 partly. Carried as binding items above:
- [medium] a stale `failed`/`killed` can end a repair → record the status at the continue;
- [low] whether `plugin validate` type-checks;
- [low] timers cleared on unload, `bound` idempotent.

**Pane delta, after the person's "no blind spots" change** (on body ba59de77b8c9):
- backend (approve-with-changes, 38k tokens, 27 s): blocker resolved (adds=ui). New: caps on `events.jsonl` and `status`, backpressure, stderr mirrored, the pane's pass bar.
- ui-design (approve-with-changes, 47k tokens, 35 s): placement thresholds 144/110 and `isPlaced`; mockups before M1 (ADR-0016); missing states; focus/holdToasts; stderr styling.
- ux-research (approve-with-changes, 52k tokens, 43 s): placement; a persistent outcome on the status line (Carbon, NN/g #1); reload after /clear; text labels (WCAG 1.4.1); focus; a hallway test for the unbacked numbers.
- react (approve-with-changes, 47k tokens, 50 s): one `$.state` writer per 100 ms tick; a read-only render hook; `isPlaced`/`reason`; `truncate-end`; pane view tests.
- game-ux (approve-with-changes, 33k tokens, 29 s): no focus steal; per-run buffers; elapsed time and attempt k/max; a 250 ms emit-to-render bar; the outcome's diff summary; advertise `/relais-status`; merged toasts.

All carried as the binding list in the Review panel.
