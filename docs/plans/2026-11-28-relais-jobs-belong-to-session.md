---
status: active
branch: fix/hsa-t24-relais-job-session
repos: [relais, amont]
adrs: []
plan-id: p-01M8H30RWXW4Q61YH0FMPZ17C5
---
# Relais host-slot jobs belong to the session

## Review panel

👉 **Decide:** none — approve if the dispatching session, not the run, owns a relais host-slot job.
📍 relais, amont · 2 phases · 1 release · next: Phase 1. Panel: backend, lang:rust, tui, unix (opus).
**Changed by review:** unattributed sessions keep the run id; relais release, install and live check added; test ordering spelled out.
**Verdicts:** round 1 4 approve-with-changes, round 2 4 approve; 3 carried; words 715 → 944 (×1.32, highs exempt); remove 0, merge 0.
📄 Full reviews: [2026-11-28-relais-jobs-belong-to-session.reviews.md](2026-11-28-relais-jobs-belong-to-session.reviews.md)

## Picture

```
 Claude session S ──dispatches──▶ relais run R ──queues──▶ host-slot ticket
                                                       holder.session
   before:  R  (no caller is ever R: promote refused own-session)
   after:   S  (greengate slot promote <id> from S: moved; from T: refused)

 queue:  a(T) b(T) relais(S)  ──promote from S──▶  relais(S) a(T) b(T)
```

The holder's session changes from the run to the dispatching session, so
that session, and only it, can move its relais job to the head.

## Goal

An agent can move its own relais job ahead in the host slot queue with
`greengate slot promote <id> --reason …`, under the limits the person set in
amont host-slot-admission t06 (own session only). Today relais tags every
host-slot holder with the run id (`verify/heavy.rs`, `slots::holder`), which
no caller of `slot promote` ever is, so every relais job is unmovable by
construction. The holder's session becomes the Claude session that
dispatched the run.

## Non-goals

- No change to relais's coordinator queue (t06: only the host-slot stage is
  reordered).
- No change to either tool's session-resolution order. relais reads
  `RELAIS_SESSION_ID`, then `CLAUDE_CODE_SESSION_ID`; the crate's
  `session_id()` reads them the other way round. The plugin sets
  `RELAIS_SESSION_ID` to the Claude session id (`claude-plugin/hooks/runs.ts`,
  `fx.session.id()`), the same id Bash sees, so they agree in practice. A
  caller who sets `RELAIS_SESSION_ID` to something else gets an unmovable
  job, refused `own-session`; that is the known limit, written in the ticket.
- No crate bump: relais already links `greenharness-slots` 5.5 (t15), which
  has promote and inode-tracked tickets.
- No pane change: the pane already shows each `waiting_for_slot` event's
  `position`, so a move shows as a smaller place on the next event.
- relais printing the job id and the promote command when it starts to wait
  (as `slot run` does) is a follow-up, not this plan.

## Code review

- relais: backend, lang:rust, tui (no relais CLI text change, but the session column of `greengate slots` and the `released` rows now show the dispatching session for relais jobs, noted in CHANGELOG; unix left out: no flag, stream or exit code changes)
- amont: none — the ticket file and one Decisions line only

## Behaviour

- A relais host-slot holder record carries `session` = the run's
  `session_id` (the dispatching Claude session; `RELAIS_SESSION_ID` from the
  plugin, else `CLAUDE_CODE_SESSION_ID`, else the `unattributed-ppid-N`
  fallback). When `session_id` is empty (only tests and unmanaged paths)
  or starts with `unattributed-`, it stays the run id: `short_session()`
  cuts to 8 characters, so every unattributed job would read `unattrib` in
  `greengate slots` and in the `own-session` refusal.
- The waiter's ticket, the holder file and the `released` journal row all
  carry that session, so `greengate slots` shows the session column a
  person can match against their own.
- `greengate slot promote <id>` from the dispatching session moves the
  relais job; from any other session it is refused `own-session`.
- An unattributed job keeps the run id as its session and cannot be moved:
  no caller's crate `session_id()` ever equals it, so the promote is refused
  `own-session` (or `no-session` with no id at all).

## Phases

- [ ] Phase 1 — holder session is the run's session, with a test that moves
  a relais waiter ahead from its owning session and is refused from another:
  `crates/relais/src/verify/heavy.rs`, `crates/relais/src/verify/heavy/tests.rs`
  (the pinned `"session":"run-w2"` at tests.rs:1279-1282 becomes
  `"session":"tab"` with its comment), `CHANGELOG.md`; then release relais
  (minor) and install it on this machine
- [ ] Phase 2 — close the ticket, noting that until relais prints the job
  id an agent finds it in `greengate slots` by tool `relais`, its session
  prefix and the repo/command: `docs/programmes/host-slot-admission/t24-relais-jobs-can-be-moved.md`,
  `docs/programmes/host-slot-admission/map.md` (amont, its own branch
  `fix/hsa-t24`)

## Decision log

- 2026-11-28 — the job belongs to the dispatching Claude session, not the
  run, because t06 lets a session advance its own work, a run is that
  session's work, and a run id is never the identity of a `slot promote`
  caller.
- 2026-11-28 — empty `session_id` falls back to the run id, so a holder is
  never written with an empty session (the crate shows `-` for one and the
  journal would drop the field).
- 2026-11-28 — the test releases the holder three waits after the move,
  not on reading place 0 from events.jsonl: that file is not flushed while
  the wait runs, so it is read only after the grant (found when the
  mid-wait read never saw place 2).
- 2026-11-28 — the relais release and install move to the programme driver
  (its instruction to this ticket agent), so Phase 1 stays open on them and
  on the live check; this branch carries the code, tests and CHANGELOG.
- 2026-11-28 — the unattributed prefix is one constant,
  `verify::UNATTRIBUTED_SESSION_PREFIX`, used by `resolve_session_id` and
  `slots::holder` (review: lang:rust, backend), declared in `verify` because
  `coordinator` already depends on it.

## Verification

- Phase 1: a unit test in `verify/heavy/tests.rs` holds the one slot with
  another holder, queues two other-session waiters, then admits a relais
  command (session `tab`). From inside the relais wait it promotes the relais
  job (found in `queue_in` by tool `relais`) first as session `session-other`
  → refused `own-session`; then as `tab` → `Moved { from: 2, to: 0 }` (a
  refusal's `limit()` is printed in the assertion message, so an `age-floor`
  on a slow host explains itself); then
  releases the holder three sleeper calls after the move lands, and reads
  events.jsonl only after the grant (the two other waiters are kept alive
  under a tool other than `relais`, in a fresh slot dir) →
  the relais command is granted while both other
  waiters still wait, and its `waiting_for_slot` events read position 2
  before the move and 0 after. The `released` row names `"session":"tab"`.
- Phase 1: a unit test builds `slots::holder` with `session_id` empty, then
  `unattributed-ppid-7`, with run id `run-e`, and asserts the session is
  `run-e` both times.
- Phase 1, piloted: `cargo test -p relais --lib verify::heavy` through
  `greengate slot run`.
- Phase 1, live after install: while a relais command waits for a host
  slot, `greengate slots` shows its row with a session whose first 8
  characters equal `$CLAUDE_CODE_SESSION_ID` in the dispatching session's
  Bash → `greengate slot promote <id> --reason …` from that session prints a
  move; from another session → refused `own-session`.

### Observed (2026-11-28, before the push)

- Promote test → expected refused `own-session` from `session-other`,
  `Moved { from: 2, to: 0 }` from `tab`, granted ahead of `a` and `b`, places
  2s then only 0s, `released` row `"session":"tab"` → observed: passes.
- Fallback test → expected `run-e` for empty and `unattributed-ppid-7`, `tab`
  for `tab` → observed: passes.
- Piloted: `cargo test -p relais --lib verify::heavy` through
  `greengate slot run --max-wait 3000` → 63 passed, 0 failed; the
  `host_slots` tests run 5 more times → 5 of 5 pass (10 of 10 on the earlier
  tree).
- Live check after install: owed to the driver's release.

## Implementation review

- implementation-review: approve-with-changes, then Delta approve-with-changes (plan text only, fixed); fixed: Verification text, observed results, place-order assert, scratch dir. 58k + 70k.
- backend: approve-with-changes, Delta approve; fixed: one prefix constant, promote errors recorded, no-migration reason in the PR body; deliberate: `resolve_session` doc name is right (the public wrapper exists). 64k + 72k.
- lang:rust: approve-with-changes, Delta approve; fixed: prefix constant (verify side), `session_id` doc; deliberate: owned strings in the test table, a nit. 49k + 56k.
- tui: approve-with-changes, Delta approve; fixed: CHANGELOG names `$CLAUDE_CODE_SESSION_ID`, plain wording, where the run id lives. 43k + 50k.

<!-- panel: repos=relais,amont reviewers=backend,lang:rust,tui,unix body-sha=1adeb5ace647 -->
