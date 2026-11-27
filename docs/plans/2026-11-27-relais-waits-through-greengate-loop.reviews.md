# Full reviews: relais waits through greengate's wait loop

Plan: [2026-11-27-relais-waits-through-greengate-loop.md](2026-11-27-relais-waits-through-greengate-loop.md).
All seats ran on opus. Codex: not installed.

## Round 1 (body 5b92e49eabce, 1109 words)

- **backend**, approve-with-changes (36k, 49 s): 1 [medium] `#[non_exhaustive]` on `Pause`/`Ended`, accepted; 2 [low] `on_wait` no-op, duplicate of lang:rust#1; 3 [low] a `LeaseStop` after joining breaks, accepted; 4 [medium] expected net count, duplicate of po#2.
- **lang:rust**, approve-with-changes (38k, 49 s): 1 [high] two closures sharing a local do not borrow-check, accepted; 2 [medium] a pre-join poll-error test, accepted; 3 [low] name `Halt`, accepted; 4 [low] which `LeaseStop` for `Line(Ceiling)`, duplicate of architect#2.
- **architect**, approve-with-changes (40k, 63 s): 1 [medium] closures, duplicate of lang:rust#1; 2 [low] name the ceiling's `LeaseStop`, accepted; 3 [low] the `Stall` changes owner, accepted.
- **po**, approve-with-changes (36k, 50 s): 1 [high] cancel and pre-join tests, duplicate of unix#1 and lang:rust#2; 2 [medium] expected count and a miss rule, accepted; 3 [medium] a 7-day count with a revert number, accepted; 4 [low] a heartbeat observable, accepted.
- **tui**, approve-with-changes (33k, 35 s): 1 [medium] keep `cause` in `Unreadable`, accepted; 2 [low] keep the unusable text, accepted; 3 [low] compare events with `origin/main`, accepted.
- **unix**, approve-with-changes (34k, 43 s): 1 [medium] a cancel test, accepted; 2 [medium] a pre-join test, duplicate of lang:rust#2; 3 [low] `slot run` keeps the exit status, accepted.

## Round 2 (body 7bccd81a8860)

- **architect** approve (31k, 23 s); **unix** approve (29k, 17 s).
- **tui** approve-with-changes (25k, 25 s), **backend** approve-with-changes (36k, 45 s), **lang:rust** approve-with-changes (33k, 42 s): mediums and lows only, carried (below).
- **po** approve-with-changes (33k, 36 s): 1 [high] a 20 s holder cannot prove the heartbeat, applied; 2 [medium] carried.
- Delta on body fd3e01320e94: backend approve (28k, 86 s); architect 1 [high] the verification lease is `Unbound` (300 s `LEASE_GRACE`, not the 120 s agent TTL), applied: the pilot holds 360 s with `GREENHARNESS_SLOT_STALL_S=600`.
- Delta on body 965738b77130 (final): backend approve (28k, 30 s); architect approve-with-changes (41k, 87 s), 1 [medium] carried.

## Carried into implementation

1. relais writes `_` arms for the `#[non_exhaustive]` enums: in `pause`, `_` acts as `Retrying`; in the end mapping, `_` maps as `Line(Ceiling)` does (lang:rust, backend).
2. The `Line(Ceiling)` value is `LeaseStop::SlotLine(SlotLineEnd::Stalled …)`, or a named equivalent, stated in the PR (backend).
3. The end mapping names `Caller(Halt::Stop(stop))` and `Caller(Halt::Unusable)` (architect, backend).
4. `host_slots_off` is called inside `pause` while the `&io::Error` is in scope, before the `Break` (lang:rust).
5. The pre-join poll-error relais test is required, not optional (backend).
6. The after-release count reads `host_slots_off` events containing "a host slot could not be locked" from the run events, not `waits.jsonl`, and has a floor of at least 3 rows, with the before count recorded at close (tui, po, backend, lang:rust).
7. The pilot's admission-wait budget must exceed the 360 s queue (backend).
8. The pilot reads `stale_leases` from `relais coordinator status` every 30 s on a quiet host (0 throughout), since the snapshot carries no per-dispatch heartbeat age for an `Unbound` lease (architect).
