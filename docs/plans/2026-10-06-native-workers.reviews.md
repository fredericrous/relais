# Full reviews: 2026-10-06-native-workers

**backend, round 1** (rework, 38k tokens, 43 s), eight findings, all applied:
- [blocking] the fallback worktree cannot be prepared before the spawn → S0 gate, option B;
- [blocking] a marked spawn runs unenforced when the coordinator is down → deny;
- [high] repair usage counted twice, settle keyed by agent only → per-dispatch booking, (agent_id, dispatch);
- [high] no waits after the bind, and orphans → `--native-spawn-wait`, `wall_timeout`, crash handling;
- [high] re-binds, SendMessage, double admission → denied, gated, exempt;
- [medium] SPEC §23 allow vs ask → decided by permission mode;
- [medium] N2 unbudgeted → usage moved into N2;
- [medium] verification entries not checks → driven → expected.

**backend, round 2** (approve-with-changes, 41k tokens, 36 s): all eight resolved. Six new findings, applied:
- [high] the isolation contradiction under B;
- [medium] a broken sentence;
- [medium] the offset timing race → message ids;
- [high] the live-checkout check and fixture tests;
- [medium] dontAsk;
- [low] the import dedup wording.

**backend, bind 1** (approve-with-changes, 44k tokens, 38 s), applied:
- [high] the wrong lease for crash detection → heartbeats;
- [medium] the per-record token sum → distinct message.id, last usage;
- [low] hook latency on unmarked calls → an S0 measurement.

**backend, bind 2** (approve-with-changes, 34k tokens, 30 s), applied:
- [medium] the lease named backwards → agent lease = `binding_lease_secs`, renewed by heartbeats.

**backend, bind 3** (approve-with-changes, 36k tokens, 32 s, on the final body):
- [low] the before-bind reaper named wrong → carried as the binding item in the review section.
