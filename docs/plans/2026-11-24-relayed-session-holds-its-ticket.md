---
status: active
branch: prog/follow-ups/f12-claim-takeover-live-session
repos: [relais]
adrs: [ADR-0083]
plan-id: p-01M85V8ZHQ6WCJSJRC5230XPN9
---
# A relayed session holds its ticket

## Review panel
👉 **Decide:** none — approve if a relayed session adopting its predecessor's handoff closes the 11-21 hole.
📍 relais · 4 phases · 1 step for you · next: Phase 1. Panel: backend, lang:rust, lang:typescript, tui, unix (opus).
**Changed by review:** non-printing claim core for `adopt`; per-ticket skip and retry semantics; `make plugin` as the plugin runner.
**Verdicts:** 5 approve-with-changes; 10 carried; words 1195 → 1535 (×1.28, the excess from high findings); remove 0, merge 0.
📄 Full reviews: [2026-11-24-relayed-session-holds-its-ticket.reviews.md](2026-11-24-relayed-session-holds-its-ticket.reviews.md)

## Picture

```
 today                               after this plan
 -----                               ---------------
 A: next --claim --> handoff(A)      A: next --claim --> handoff(A)
 /clear: A ends, B starts            /clear: A ends, B starts
 B works, claims nothing             plugin: adopt --previous A
         |                                   |
   2 h --+--> t19 reads unheld               v
         |                           work(B), ends at B's SessionEnd
 C: next --claim --> handoff(C)      C: next --claim --> skips t19
 D (C's relay) takes t19 over        D: claim t19 --> exit 3
```

The ticket used to fall free two hours after the handoff while its relayed
session still worked it; after this plan that session holds it as `work`
from its first moment.

## Goal

A session that a `/clear` relay starts holds the ticket its predecessor
handed off as `work`, without its agent having to run
`relais programme claim`. Another session's `next --claim` then never
reserves that ticket, and its `claim` exits 3, for as long as the relayed
session lives (follow-ups f12).

## What happened on 2026-11-21

From `claims/log.jsonl` and the four transcripts:

| UTC | Event |
|---|---|
| 05:21:51 | 943bdf5e reserves blue-factory t19 as `handoff` (`next --claim`), then clears |
| 05:22:21 | 1c6146ea starts on the relayed prompt `resume blue-factory … t19`; it never runs `claim` |
| 07:21:51 | the handoff ends (R = 2 h); t19 reads unheld while 1c6146ea works it in another repository's worktree, with no `prog/` branch |
| 08:36:49 | 22c6b0fc's `next --claim` reserves the unheld t19 as its `handoff` |
| 08:37:46 | 0f2c38ac, its relayed successor, takes the handoff over: two live sessions on t19 |
| 09:30:49 | 0f2c38ac releases t19; 1c6146ea claims it at 09:31:06 |

So the takeover rule did what it says: a live `work` claim already refuses
(exit 3), and a `handoff` is meant to be taken over. The live session held
no claim at all. The rule "a session starting a ticket claims it first"
(ADR-0083) rested on the agent alone, and one skipped it.

## Non-goals

- No change to N (8 h), R (2 h), the takeover rule in
  `claim_work`, or `--steal`.
- No read of `greenbelay board` from relais (Decision log).
- No change to the programme skill's text.

## Code review

- relais: backend, lang:rust, lang:typescript, unix, tui

## Behaviour

1. **`relais programme adopt --previous <old> [--session <new>]`.** For
   each live `handoff` claim of `<old>`, in every programme, it claims that
   ticket for `<new>` (default: the environment's session) exactly as
   `relais programme claim` would, through the same shared-state path, with
   `Contest::Refuse`. A handoff becomes `<new>`'s `work` (event `takeover`,
   `from = <old>`). `<old>`'s `work` claims are never read: they ended with
   it. A ticket another session already took over is left with it. Output,
   stdout empty; one stderr line per ticket, with no `next:` or `--steal` hint: `relais: took over <repo> <slug> <id> from
   <old8>` (the claim command's line), or
   `relais: <repo> <slug> <id> is held by <x8> (<age> old)`, or
   `relais: <repo> <slug> <id> skipped: <why>` (the ticket closed, or its
   repository is not found); with nothing to adopt, no line. Each ticket is
   tried on its own and the loop goes on. Exit 0 when every ticket was
   adopted, skipped or is held by another; exit 2
   on a usage error (no session id, `--previous` empty or equal to the
   session), each naming the flag to fix, e.g. `relais: --previous is this
   session; pass the session the /clear ended`; exit 1 on a lock timeout or I/O failure, after trying every
   remaining ticket (earlier adoptions stay). The exit code is the only
   machine signal; stderr is for people. A second `adopt` for the same
   `--previous` finds nothing and exits 0.
2. **The plugin adopts at the clear.** On `classic.SessionStart` with
   `source === 'clear'` and a `memory.previous` (the session the clear
   ended, as `onProgrammeSessionEnd` records it), the plugin captures
   `<old>` and `<new>` before any await, skips when `memory.isOlder`, and
   runs `relais programme adopt --previous <old> --session <new>` first;
   one `PROGRAMME_BOUND_MS` deadline covers the adopt and the read after it
   (the read gets the remainder; each claim write is atomic under its slug
   lock, so a kill is safe). It then reads the programme view, dropped if
   `memory.session` moved on meanwhile, so the pane shows
   the ticket as the session's own (`holder: self`, `work`). A relais that
   predates `adopt` answers exit 2 `unrecognized subcommand 'adopt'` (matched
   on `adopt`, not on the `session` pattern); the plugin ignores it and reads
   the view as before. Any other exit 2 is recorded as `adopt` `error` with
   the first stderr line. A hook never fails on bookkeeping, but a failed
   adopt (timeout, lock, exit 1) keeps `memory.adoptPending` set: each later
   `readProgramme` of that session retries it until it exits 0, and the
   pane shows `ticket not adopted · run relais programme claim` until then.
   Each outcome is a pane event, a new
   `{ kind: 'adopt'; result: 'ok' | 'older' | 'error' }` variant.
3. **Unchanged:** an agent that does run `claim` after the adoption gets
   `refreshed your claim on …`; the session's `SessionEnd` still deletes the
   work claim; a `work` claim still lapses 8 h after its last refresh.

## Phases

- [ ] Phase 1 — the regression test, red on main:
  `crates/relais/tests/programme_claims.rs`. The 11-21 sequence: A reserves
  t19 as handoff and ends; B is A's relayed successor and runs only
  `adopt`; the clock moves 3 h (a `World::age_claims` helper backdates `claimed_at`/`refreshed_at` in every claim file under the state dir, and any shared-state lease with them); C's `next --claim` does not choose t19;
  D's `claim` on t19 exits 3. Second case: the ordinary relay (B runs
  `claim` itself) still takes the handoff over. Third: a handoff a third
  session already took over stays with it; `adopt` prints nothing and
  exits 0, and so does a second `adopt`. Fourth: two handoffs, one on a
  closed ticket: the open one is adopted, the closed one skipped, exit 0.
  Fifth: a handoff whose recorded `repo` is a removed `-wt-` worktree is
  adopted through the primary checkout.
- [ ] Phase 2 — `adopt` in relais: `crates/relais/src/cli/mod.rs` (the
  subcommand), `crates/relais/src/cli/programme.rs` (`adopt_command`, which
  lists `<old>`'s live handoffs with `Store::claims_of` and calls the claim
  path per ticket; that path, from `forget_ended` through `claim_ticket`
  and its compensation, is first extracted from `claim_command` into a
  non-printing function returning `claim::Taken`, so `claim`'s output is
  unchanged; the `--dir` is the claim's `repo` when it exists, else the
  primary checkout beside a `-wt-` path, else a `skipped` line),
  `crates/relais/src/programme/claim/store.rs` only if a helper is missing.
  `CHANGELOG.md` entry.
- [ ] Phase 3 — the plugin: `claude-plugin/hooks/programme.ts`
  (`adoptPrevious`; `adopt` added to `PROGRAMME_COMMAND`), `claude-plugin/hooks/register.ts` (call it before the
  read on a clear), `claude-plugin/tests/programme.test.ts` (argv, order
  adopt → read, an older relais's exit 2 ignored, no adopt without a
  previous session or on resume/fork; `reads()` narrowed to
  `argv[2] === 'session'`, a separate `adopts()` helper, and fakes that answer
  `adopt` and `session` separately; a second clear during a pending adopt);
  `claude-plugin/hooks/events.ts` (an `adopt` event variant).
- [ ] Phase 4 — close f12: `docs/programmes/follow-ups/f12-claim-takeover-live-session.md`
  (Resolution, `status = "closed"`), `docs/programmes/follow-ups/map.md`
  (Decisions so far, Handoff), and a dated follow-up ticket: over 7 days of
  `claims/log.jsonl` after the reinstall, every relayed handoff logs a
  `takeover` within 10 s of its successor's start, and no handoff expires
  while its successor is alive.

### 🧑 Your steps
- `relais install --claude` — the plugin change applies only once the relais plugin is reinstalled (it is at 0.72.0 here), from the next session

## Decision log

- 2026-11-24 — Adoption keys on the exact predecessor id the plugin already
  holds (`memory.previous`, set at `SessionEnd(reason=clear)`), not on a
  process or a cwd match in the Rust hook, because Claude Code's
  `SessionStart` payload names no predecessor and a cwd match would mix two
  tabs cleared in the same directory.
- 2026-11-24 — The adopted claim is `work`, not a renewed `handoff`: ADR-0083
  says the next session "takes it over as `work`", and a `work` claim ends
  at its session's `SessionEnd`: the liveness test the ticket asks for.
  Cost accepted: a tab cleared for other work holds the ticket until that
  session ends or 8 h pass.
- 2026-11-24 — The ticket's board clause ("a ticket whose worktree shows a
  live session on the board refuses") is not built: it would not have
  caught 11-21 (another repository, a branch naming no ticket), and relais
  reading greenbelay crosses a tool boundary (`harness.cross-tool-reads`).
  Accepted gap: a live session that never claims and was not relayed.

## Verification

| Check | Input | Expected |
|---|---|---|
| regression test | `cargo test -p relais --test programme_claims` | the five cases pass; case 1 fails on main (no `adopt`) |
| piloted run | a scratch `RELAIS_STATE_DIR`: `next --claim` as A, `adopt --previous A --session B`, `claim` as D | B holds `work` from A; D exits 3 with `is held by B…` |
| plugin tests | `make plugin` (`claude plugin validate` + `scripts/plugin-test.sh`) | the adopt-then-read order, older-relais exit 2 ignored, no adopt on resume/fork, and retry-after-failure cases pass |
| full gate | the commit gate | green |

<!-- panel: repos=relais reviewers=backend,lang:rust,lang:typescript,tui,unix body-sha=0f4eb3921e8b -->
