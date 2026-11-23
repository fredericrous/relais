# Full reviews: Owned values in the SharedState trait

All seats ran on opus. Round 1 on body 98a67df41684, round 2 on 906056a8f82f,
backend's final pass on 4e1c775f4278.

## architect

- Round 1 (approve-with-changes, 50k, 99 s): [high] a dead run holds a
  shared intent for good: accepted, in the `replacing` form; [high]
  LocalState's grant-then-release fences out the ticket holder: accepted
  (`NodeDecides` + `grant_ticket`); [medium] `answer_decision` compares
  nothing: accepted (`Granted | AlreadyAnswered`, `release_decision`).
- Round 2 (approve-with-changes, 58k, 271 s): all three resolved; [medium]
  a local `Claimed` from a row this ledger never saw `old` hold must not
  replace: applied with lang:rust's blocking finding A.

## backend

- Round 1 (approve-with-changes, 53k, 122 s): [high] takeover of dead runs'
  intents: accepted; [high] LocalState ticket order: accepted; [medium] no
  promote compensation: accepted; [medium] keep backend in Code review:
  accepted; [medium] two Verification rows not runnable: accepted; [low] no
  merge of Phases 2 and 3: noted.
- Round 2 (approve-with-changes, 64k, 353 s): all resolved; [high] a pending
  local row read as "old is gone": applied.
- Final pass (approve-with-changes, 61k, 20 s): resolved; [low] a refused
  takeover leaves the row pending and a retry loops, refused each time:
  carried into implementation (accepted as fail-closed; each refusal is in
  the intent's refusal note).

## lang:rust (slice: Phase 1)

- Round 1 (approve-with-changes, 52k, 129 s): [high] LocalState refused
  claim moves the epoch: accepted (architect's form, this seat's assertion);
  [high] no takeover: accepted (`replacing`); [medium] promote
  compensation: accepted; [medium] compensation errors: accepted
  (`Compensation { local, release }`); [low] runnable trait grep: accepted.
- Round 2 (approve-with-changes, 60k, 301 s): 1, 3, 4, 5 resolved; [blocking]
  A: the takeover could take an intent from a live run on another node:
  applied; [medium] B: no test of the decision compare-and-set: carried into
  implementation.

## po

- Round 1 (approve-with-changes, 34k, 166 s): [high] bounded trait grep:
  accepted; [high] existing-suite check covers `cli/`: accepted; [medium]
  `node` as `pub(crate)`: rejected (the CLI is a separate binary target that
  reaches the library through `relais::` paths); [medium] post-release
  Outcome: rejected (worktree-task keeps only pre-push observations; the
  Resolution carries the rest); [low] Phase 2 names the doc bullet: accepted.
- Round 2 (approve-with-changes, 42k, 520 s): all resolved, rejections
  stand; [medium] pilot `relais decide`: carried into implementation.

## tui

- Round 1 (approve-with-changes, 35k, 52 s): [medium] what the person sees
  when a release fails: accepted; [medium] pilot by diff of stdout, stderr
  and `$?`: accepted.
- Round 2 (approve-with-changes, 42k, 383 s): resolved; [medium] callers
  match `Compensation`'s `local` so `programme claim` keeps its retry line:
  carried into implementation; pilot `decide`: carried.

## unix

- Round 1 (approve-with-changes, 29k, 67 s): [medium] promote compensation:
  accepted; [medium] pilot exit status and streams: accepted; [low] the
  `.pointer.lock` rule moves verbatim: accepted.
- Round 2 (approve-with-changes, 37k, 418 s): resolved; [medium] both
  binaries on the same scratch path (the lock path is printed): carried
  into implementation; [low] a killed promote leaves the shared pointer
  ahead, the docs say so: carried into implementation.

## Codex

Not run: programme ticket agent.
