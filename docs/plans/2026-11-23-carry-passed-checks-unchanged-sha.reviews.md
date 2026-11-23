# Full reviews: Carry passed checks to an unchanged sha

All seats ran on opus. Programme-run plan (blue-factory t32); the
programme's approval is the plan's approval.

## backend
- R1 approve-with-changes (43k, 84 s): [high] re-run every command up to
  the last failure, carry only passes after it — accepted; [medium] seat
  tui (pane builds checks from check events) — accepted; [medium]
  observable pass bar, recount excluding cached rows — accepted.
- R2 approve-with-changes (49k, 26 s): [medium] pilot must put the failing
  command first — accepted; [medium] count the prefix rule's saving —
  accepted, measured: t27's 16 pairs re-ran passes 58.6 run-min in 10
  pairs, 30.1 of them after the last failure in 5 pairs.
- R3 approve-with-changes (55k, 25 s): [low] send `CheckCarried` after the
  re-run checks — fixed via lang:typescript; [low] numeric recount target
  ≤ 28.5 run-min over 16 pairs (~1.8/pair) — carried into implementation.
- R4 approve (57k, 13 s).

## lang:rust
- R1 approve-with-changes (29k, 20 s): [high] command independence —
  accepted (prefix rule; independence opt-in rejected for now, see
  Decision log); [high] 25 `CheckOutcome` literals in mod.rs, stamps.rs,
  holdout_run.rs — accepted; [medium] per-outcome JUnit gate — accepted;
  [medium] explicit `Carry` holder arm — accepted; [medium] flaky carry
  test — accepted; [low] `INSERT OR REPLACE` keeps `carried_from` —
  accepted.
- R2 approve-with-changes (35k, 13 s): [medium] Goal/Phase 2/test bullet
  still said "only the failed" — accepted; [low] pilot order — accepted.
- R3 approve-with-changes (41k, 16 s): [medium] `Event` is Serialize only:
  pin the serialized string — carried; [medium] arm in exhaustive
  `Event::is_protocol_line` — carried; [low] ordering — fixed in R4.
- R4 approve (43k, 5 s).

## lang:typescript (seated in round 3, when the pane joined)
- R1 approve-with-changes (31k, 16 s): [high] carried check must be
  non-running, `pane.ts` and `Check` type in Phase 1, pane tests in
  Phase 3 — accepted; [medium] emit after the re-run checks so the pane
  keeps profile order — accepted; [medium] leave `detail`/`wait`, add a
  timeline row — carried; [low] no-step no-op — carried; [low] replay
  fixture with an unknown kind — carried.
- R2 approve (33k, 9 s): [low] carried event after a step closed must not
  reopen it — carried; [low] wording for the no-re-run case — carried.

## tui
- R1 approve-with-changes (33k, 50 s): [medium] `(carried)` report row —
  accepted; [medium] pilot names the report line and event — accepted;
  [medium] reason text format, 80 columns, `+N more` — accepted; [low]
  seat tui — accepted.
- R2 approve-with-changes (39k, 19 s): [blocking] pane must not parse
  reason prose: typed `CheckCarried { label, from }` event — accepted.
- R3 approve-with-changes (42k, 11 s): [medium] pilot checks both the
  decision `what` and the event `kind` — carried; [low] pane test file in
  Phase 3 — fixed via lang:typescript.
- R4 approve (44k, 7 s).

## unix
- R1 approve-with-changes (27k, 30 s): [medium] report would merge carried
  rows into `(reused)` — accepted (with tui's patch).
- R2 approve-with-changes (31k, 10 s): [low] pilot order — accepted.
- R3 approve-with-changes (37k, 17 s): [low] event timing when nothing
  re-runs — carried; [low] "per pair" vs total — carried.
- R4 approve (37k, 8 s).

codex: not run (programme-run ticket; no Codex review requested).
