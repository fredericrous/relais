# Full reviews: Route the learning cycle and hooks through SharedState

Panel on opus, three rounds (round 1 full, round 2 delta, a final delta
after three high findings). Codex: not run (programme ticket agent).

## Round 1 (body 03e2a02ad4f2, 1550 words)

- architect — approve-with-changes (29k, 20 s). 1 high: default trait
  bodies and a merge order (rejected: a default lets a closed client skip a
  shared release silently; the rebase order is in Phase 3). 2 high: the
  hook fallback belongs at the composition root, holds end by TTL
  (accepted). 3 medium: heal only on ancestry (rejected: artifacts have no
  ancestry). 4 medium: a crash between a shared handoff grant and the claim
  file is bounded by `HANDOFF_HOLD_SECS` (accepted). 5 low: name the reader
  call sites (rejected: already named).
- backend — approve-with-changes (33k, 43 s). 1 seat unix (accepted). 2
  high: take the flock before reading previous and hold it across the CAS
  (accepted). 3 TTL arithmetic and a lapse case (accepted, own wording). 4
  the hook enforces its own deadline (accepted). 5 a heal writes a note
  (accepted). 6 rewrite the no-heal row (accepted). 7 rebase on an-t16/17
  (accepted).
- lang:rust — approve-with-changes (58k, 75 s). 1 high: previous can change
  between CAS and write (rejected as resolved by backend 2). 2 high: keep
  `NothingToRollBack` for previous == active (accepted). 3 high: test the
  flock on both paths (accepted). 4 a lock-wait `Refused` is a note in the
  cycle (accepted, merged with tui 3). 5 a wrapper seam for the cycle-loses
  test (accepted). 6 only `cli/ctx.rs` builds a state (accepted). 7 the grep
  row names the in-file test modules (accepted).
- po — approve-with-changes (27k, 21 s). 1 high: Outcome and measures
  (rejected: written at close, no counter exists). 2 high: Phase 1 first and
  mergeable (accepted). 3 the heal conflict names its resolving command and
  repeats per cycle (accepted). 4 the no-heal row (rejected, duplicate). 5
  the follow-up ticket id in the Resolution (accepted). 6 the hook line once
  per session (rejected: one process per event).
- tui — approve-with-changes (28k, 21 s). 1 high: the rollback line in its
  own wording (accepted). 2 high: the hook line's text and stdout identity
  (accepted, merged with architect 2). 3 self-contained cycle notes
  (accepted). 4 a dedicated `Compensation` message (accepted). 5 name held
  tickets when all are held (rejected: `relais programme frontier` shows
  them). 6 piloted diff of stdout, stderr and exit (accepted).
- unix — approve-with-changes (28k, 20 s). 1 seat unix (rejected as
  duplicate of backend 1). 2 the flock wait is bounded (accepted, own
  wording: 2 s, `Refused`, exit `Blocked`). 3 match promote's wording
  (rejected: tui 1 wins; stdout stays empty). 4 stdout still a valid hook
  answer (accepted).

## Round 2 (body 42ae8b7a27c0, 2034 words)

- backend — approve-with-changes (38k, 22 s): all seven resolved; carried:
  8 tui reason in Code review (low), 9 the Phase 1 split option contradicts
  one PR (medium; the PR stays one), 10 bound the hold of the flock across
  a remote CAS (low).
- lang:rust — approve-with-changes (66k, 22 s): 8 high, the hook deadline
  needs an owned `Arc` and a detached thread (applied); 9 low, the heal's
  `Registry::evaluated` runs under the flock (carried).
- architect — approve-with-changes (39k, 28 s): high, `handle` takes
  `Arc<dyn SharedState>` (applied).
- tui — approve-with-changes (37k, 19 s): N1 high, `relais status` takes a
  run id (applied: the pointer dropped); N2 medium, pin the heal, lock-wait
  and `Compensation` texts (carried); N3 low (carried).
- po — approve-with-changes (35k, 25 s): 7 medium, a hook deadline row
  (covered by the rust 8 test); 8 low, quoting (carried).
- unix — approve (35k, 10 s).

## Final delta (body aa0703c7c3ee, 2095 words)

- architect approve (42k, 15 s): low, split the deadline so the local pass
  keeps `ONCE_GRACE` (carried).
- backend approve-with-changes (43k, 9 s): 8, 9, 10 still carried; the hook
  thread never holds the registry flock.
- lang:rust approve (65k, 7 s): never join the abandoned thread (carried).
- po approve (37k, 7 s).
- tui approve (39k, 7 s): the abandoned thread prints nothing (carried).
- unix approve (38k, 8 s).

Carried into implementation (9): backend 8, 9, 10; lang:rust 9 and the
no-join note; tui N2, N3 and the silent thread; architect's deadline split.
