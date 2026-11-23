# Coordinator seats through SharedState: full reviews

Panel on opus: architect, backend, lang:rust (slice Phase 1), po, tui, unix. Round 1 whole panel, round 2 delta (all six), backend re-run on the final body. Codex not run (programme ticket agent).

## Round 1 (body 0da1b73fa937, 1457 words)

- unix, approve-with-changes (26k, 20 s): 1 shutdown without a signal handler leaves leases to the TTL (accepted); 2 exit-3 verification tightened (accepted); 3 log lines on stderr, token omitted (accepted); 4 dropped connection after Granted (rejected: today's lease grace, unchanged).
- po, approve-with-changes (26k, 25 s): 1 counter-writer verification row (accepted); 2 a shared refusal reads as a queue wait, needs a named line (accepted); 3 Outcome measures and rollback (accepted); 4 split seats and spend (rejected: a reservation needs the seat token); 5 pack-refresh line (rejected: its own commit).
- lang:rust, approve-with-changes (35k, 30 s): 1 [blocking] concurrent syncs race (accepted: one sync at a time); 2 failure reported to the wrong caller (accepted: keyed by dispatch id); 3 constant holder shared by every node (accepted: host name); 4 step 4 vs unclaim wording (accepted); 5 verification wording (accepted); 6 fake under cfg(test) (rejected: lib tests include it).
- tui, approve-with-changes (28k, 35 s): 1 downgrade has no visible cause (accepted: one stderr line; status fields left out, a wire change); 2 slice refusal detail text (accepted); 3 one line per state change (accepted).
- architect, approve-with-changes (40k, 53 s): 1 sync serialization (accepted, merged); 2 resume re-reserves (accepted); 3 reservation on restart (accepted as the closed client's duty); 4 unclaim after a stale read (accepted).
- backend, approve-with-changes (38k, 56 s): 1 sync race (accepted); 2 no bound on a shared call (accepted as a statement: the client bounds each call); 3 stale unclaim (accepted); 4 pending settles in memory (accepted); 5 observability (accepted as stderr lines); 6 socket test row (accepted); 7 no merge.

## Round 2 (body dd968b53718a, 1846 words)

All round-1 findings resolved. New, carried into implementation (medium or low, no body edit):
- po (32k, 14 s): a per-call bound for the request path with a stalled-fake test (medium); the Queued rollback count comes from stderr lines (low); Windows hostname path untested (low).
- tui (33k, 12 s): one downgrade line per dispatch and cause, plus a recovery line, not one per poll (medium).
- architect (47k, 26 s): lock-order sentence to state book then admission, never the reverse (medium); Phase 2 writes the closed client's reservation-expiry duty into docs/shared-state.md (low).
- lang:rust (42k, 19 s): the `local` fallback is a constant holder, refuse or persist an id; name the hostname dependency (medium); renew pass deadline (medium); clear failure records when a dispatch leaves the seated set (low); the restart reservation statement is an an-t11 assumption (low).
- unix (32k, 14 s): request-path sync bounded to a few calls with deadlines (medium); drop per-dispatch failure state when the dispatch leaves (low).
- backend (46k, 32 s): 8 [high] renew under the book mutex stalls handlers (accepted, applied); 9 latency baseline on main and counter comparison only under the fake (low, carried).

## Backend, final body (2c32f0c49652, 1894 words)

approve-with-changes (44k, 11 s): finding 8 resolved; 9 carried (low); 10 [low] the "whole sync" sentence vs the renew exception, carried: the implementation and docs state renew as the exception.
