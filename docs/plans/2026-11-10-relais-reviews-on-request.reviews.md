# Reviews: relais reviews on request and checks the base lazily

- **backend** (opus-5-5): round 1 approve-with-changes (27k, 30 s), with 7 findings. They covered: the Code review line; a receipt contract check (no consumer found); single-flight memo; log stems by index; a visible line for unreviewed tests; numbers (carried); scripted checks. Round 2 approve-with-changes (31k, 26 s). Re-bound on the final body (32k, 7 s): no blocker.
- **lang:rust** (opus-5-5): round 1 approve-with-changes (46k, 71 s), with 6 findings. They covered: base time kept off the work budget (`charge_base`); a merged view; JUnit memo test; `finish_attempt` asserts; cache stops filling (resolved: read-only legacy); `Cargo.lock`. Round 2 approve-with-changes (60k, 29 s): #7 high, the Phase 2 test gate restored.
- **tui** (opus-5-5): round 1 approve-with-changes (25k, 22 s), with 5 findings. They covered: the Code review line; on-demand progress step; the `relais resume` text; a plain line for unreviewed tests; old receipts render. Round 2 approve-with-changes (40k, 28 s): #2 high, gates restored.
- **unix** (opus-5-5): round 1 approve-with-changes (33k, 56 s), with 3 findings. They covered: log stems and setup once; the Code review line; an on-demand step. Round 2 approve-with-changes (39k, 25 s).
- **Carried into implementation:**
  - backend r2#6: the estimate line becomes a post-release measure against 0.74.
  - backend r2#7, rust r2#8: no counter; the endings tests assert the `relais resume` line.
  - backend r2#8: remove `verify-base` on every ending, and test it.
  - backend r2#9: report base time apart from `waited_in_work` if cost output shows it.
  - backend re-bind: the real grh run is a post-release observation, not a Phase 2 gate.
  - rust r2#9, r2#10: the measure wording; the grep evidence (recorded in the Decision log).
  - unix r2#4: under `--json`, carry `reviewed: false`, print no plain line, and test that the output still parses.
  - tui r2#1: name the pane fixture `claude-plugin/tests/fixtures/status-captured-run.timeline.ts` and the run printer.
  - tui r2#3: a pass threshold on the after-measure.
