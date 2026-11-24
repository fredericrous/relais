# Full reviews: Route a relayed session on its own state

- backend (opus): R1 approve-with-changes, 7 findings (2 high: double budget/stale capture, session reset drop), 40k, 62 s; R2 approve-with-changes (b2 vs non-goal, Verification dup, fx.kv marker), 45k, 31 s; final approve, 46k, 12 s.
- lang:typescript (opus): R1 approve-with-changes, 5 findings (3 high: note channel, test (a) cannot fail on id, wait before reading state), 47k, 71 s; R2 approve-with-changes (high: b2 vs non-goal; fx.kv marker; six tests), 52k, 20 s; final approve-with-changes (low: b2 must assert the wait, carried), 53k, 14 s.
- tui (opus): R1 approve-with-changes, 4 findings (high: toast fallback), 28k, 34 s; R2 approve-with-changes (3 low, carried: toast for (d), stdout in unparseable note, six tests), 32k, 15 s; final approve, 33k, 8 s.
- unix (opus): R1 approve-with-changes, 4 findings (high: channel with zero runs), 32k, 46 s; R2 approve-with-changes (fx.kv marker, six tests), 36k, 26 s; final approve-with-changes (same two, carried), 38k, 17 s.
- Carried into implementation (bound into the relais contract): final-flush marker in fx.kv, read and cleared at session.start, (d) with a fresh store and a toast when no run; (b2) asserts the launch is pending until waitFor(remainingMs) passes; the unparseable note quotes the start of stdout; Verification counts six tests.
- codex: not run (optional).
- Person delta ("a task of the same programme?"): wording "routing task" + programmes Non-goal; backend approve (48k, 19 s), lang:typescript approve-with-changes (b2 low, carried; 56k, 26 s).
- Person delta 2 (next ticket never from another programme → park): Phase 2 files the follow-up, Decision log entry; backend approve, 1 low carried (check the filed ticket's number is free on every branch; 51k, 30 s).
