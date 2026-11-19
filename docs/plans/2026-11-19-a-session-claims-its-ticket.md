---
status: active
branch: feat/sessions-claim-their-work
repos: [relais, greenharness-skills, amont-agent, decisions]
adrs: [ADR-0077]
programme: session-boundaries t26
plan-id: p-01M7SH79WA5FGSCQK2F7K609NZ
---
# A session claims its ticket, and a handoff names only an unheld one

## Review panel

👉 **Decide:** none — approve if a relais claim file, released at `SessionEnd`, and a handoff that names one unheld ticket or parks are worth one new store.
📍 relais, greenharness-skills, amont-agent, decisions · 5 phases · 4 PRs · next: Phase 0, measure N. Panel: architect, backend, lang:rust, po, tui, unix (opus), codex.
**Changed by review:** the park line keeps `safe to /clear` and exits 22; every claim write takes the slug lock; tickets due later and a programme someone else entered are skipped.
**Verdicts:** 6 approve-with-changes, then po and tui approve; 10 carried; words 1834 → 2553 (×1.39, high and blocking exempt); remove 0, merge 0.
📄 Full reviews: [2026-11-19-a-session-claims-its-ticket.reviews.md](2026-11-19-a-session-claims-its-ticket.reviews.md)

## Picture

```
 session A (sre-agent cve-lane)       session B (decisions sess.-boundaries)
   at a boundary                        at a boundary, 3 min later
   relais programme next --claim        relais programme next --claim
     --from sre-agent:cve-lane          --from decisions:session-boundaries
        |                                    |
        v                                    v
   <relais state>/claims/<repo-key>/<slug>/<id>.toml (create_new: 1 wins)
        | kind=handoff, holder=A             | t23 held -> claims t25
        v                                    v
   next: `resume sre-agent cve-lane     next: `resume decisions
          t20: …`                              session-boundaries t25: …`
        |
   /clear -> SessionEnd(A): work claims go, handoff claims stay
        |
   successor A': relais programme claim cve-lane t20 -> work, holder A'
        |
   SessionEnd(A'), ticket closed, or N h unrefreshed -> released

 nothing unheld -> "safe to /clear; parked: next due 2026-11-24", no next:
```

Two sessions handing off minutes apart race on one `create_new`, so they
leave with different tickets; the successor turns the handoff claim into
its own, and the ready lists (`relais hook`, `frontier`) mark or skip
whatever a claim holds.

## Context

The person reported on 2026-11-19 that sessions clear themselves and then
restart on a different programme. Several sessions end up on one programme,
while other programmes are left with nobody. The t26 log study traced this
to open-ended handoffs ("pick from decisions or …") and to ready lists that
show held tickets.

The person chose **claims, and no open-ended handoff**. Asked how to record
a claim, they chose **a relais claim file** (2026-11-19) over a `prog/`
branch alone or board-derived claims.

Facts checked in this session:

- relais already treats a `prog/<slug>/<id>` branch as in flight
  (`programme/load.rs:313`, `analysis.rs:36`), and `frontier` leaves such
  tickets out. Live sessions do not name their branches that way:
  96c8a791 works cve-lane t26 on `docs/cve-lane-t26`, and the planning
  session for this ticket ran on `docs/session-claims`. Both tickets were
  listed as ready.
- greenbelay's activity lines (`~/.claude/greenbelay/sessions/<id>.activity`)
  record a place only when a tool touches a path. A branch names a ticket
  only by convention. Work on one repository's ticket can also happen in
  another repository's worktree (sre-agent t26 is worked on in
  `homelab-wt-standing-critical`).
- `/clear` gives the session a new id (f8f8b580 became 08cf8cdd). A claim
  keyed by session therefore has to pass to the successor.
- relais receives `SessionStart` and `SessionEnd` for every session
  (`~/.claude/settings.json`), and `PreToolUse` only for Agent and Task. It
  can release a session's claims when that session ends. It has no
  per-call heartbeat.
- The clear-relay journal (`~/.local/state/greenbelay/clear-relay/*.jsonl`)
  records `arm`, `clear` and `submit` without the prompt text, so the
  week's re-count has nothing to count yet.

## Goal

- A ticket is **held** while any of these is true:
  - a `prog/<slug>/<id>` branch works it (as today);
  - it has an unexpired claim file.
- `relais programme next --claim` picks and reserves the first unheld ready
  ticket in one atomic step. The Handoff step prompts with only that ticket,
  and parks when there is none.
- The ready list at `startup` and `clear` marks held tickets, and so does
  `frontier`. At a projects root, the expanded repository is the one with
  the most unheld ready tickets.

## Non-goals

- **Agent pods on k8s** do not see this machine's claims: an accepted gap,
  because relayed clears are local.
- **greenbelay `board`** does not read claims in this plan; that is a
  follow-up if wanted.
- **The clear-relay plugin** does not refuse a `next:` without a ticket. It
  only journals what was named, and the re-count decides whether to
  enforce (the same staging as ADR-0077).
- **t27** (the root scan cutting `decisions` off) stays its own ticket.
  `next` is a CLI with no 2.5 s cutoff, and claims are read from one
  directory, so this plan does not depend on t27.

## Phases

### Phase 0 — measure N (decisions; read-only)

- From `~/.claude/projects/*/*.jsonl` over the last 14 days, take the
  lifetime of each session that ran at least 20 tool calls: its first to
  last timestamp.
- Separately, take the share of those sessions with no `SessionEnd` in
  relais's hook records. These are the claims only expiry can free.
- Set `N` to the p95 lifetime rounded up to whole hours, at least 4 and at
  most 24.
- Record the numbers, the query and N in t26. The query goes in a fenced
  block so the count can be re-run.

### Phase 1 — the claim store and `next` (relais)

**Store.** One file per claim:
`<relais state dir>/claims/<repo-key>/<slug>/<id>.toml`.

- `repo-key` is the first 12 hex characters of the `sha2` SHA-256 of the map repository's git common dir; the
  file also records the path, for display.
- Every write (create, takeover, refresh, steal, sweep) holds
  `procs::LockFile` on `claims/<repo-key>/<slug>/.lock` (retried for up to
  2 s), re-reads the claim under the lock, and writes with
  `fsutil::write_atomic_with_mode`, so the first writer wins, a takeover
  has one winner, and a killed writer never leaves a partial claim.
  Read-only renders (`resume.rs`, `frontier`) never write or delete.
- Fields:
  - `session`, `kind` (`work` or `handoff`);
  - `claimed_at` and `refreshed_at` (Unix seconds);
  - `from` (the session that handed it off, if any).
- The state dir comes from `paths.rs`, which honours `RELAIS_STATE_DIR`.
  Tests use a scratch state dir and never touch the real one
  ([[feedback_branch_binary_migrates_real_ledger]]).

**Rules.** These live in a new `programme/claim.rs`; `held()` is a pure
function over the claim, the clock and N.

| Claim                  | Held until                                                            |
|------------------------|-----------------------------------------------------------------------|
| `work`                 | the holder's `SessionEnd`, the ticket closes, or `refreshed_at + N`    |
| `handoff`              | taken over, the ticket closes, or `claimed_at + R`, R = 2 h            |
| a `prog/` branch, no file | as today                                                           |

- A claim file that does not parse counts as unheld once it is older
  than 60 s.
- A `handoff` claim survives its holder's `SessionEnd`; that end is the
  `/clear` it exists for.
- R covers the relay, which delivers within seconds, plus a person who
  clears by hand later the same session.
- A closed ticket's file is swept by the next `claim` or `next` (a write,
  under the lock).
- Every create, takeover, refresh, steal, release, sweep and expiry appends
  one line (`ts, event, repo-key, slug, id, session, kind`) to
  `<relais state dir>/claims/log.jsonl`: the claims log Phase 4 re-counts.
- `load.rs` reads the claims and passes them to `analysis::standings` as
  values, so analysis stays pure. `Standing::InFlight` gets a holder (a
  branch, or a claim with session and kind); the JSON keeps `in_flight`
  and adds `held_by`.

**Commands** (`cli/programme.rs`). The session id comes from
`--session <id>` (scripts and tests), else `coordinator::resolve_session()`
(`RELAIS_SESSION_ID`, then `CLAUDE_CODE_SESSION_ID`); on its parent-PID
fallback, every writing command refuses with exit 2 and names both.

- `relais programme claim <slug> <id> [--dir <map dir>]` (the skill always
  passes `--dir`: the cwd may be another repository's worktree):
  - takes the ticket for this session as `work`;
  - may take over a `handoff` claim, or refresh one's own claim;
  - is refused with exit 3 when another live session holds a `work`
    claim. The refusal names the holder's first 8 characters and the claim's
    age, then `next: relais programme next --from <repo>:<slug> --claim`,
    and the full `relais programme claim <slug> <id> --steal` for when the
    person directs it, all on stderr; stdout stays empty.
- `relais programme release <slug> <id>`: deletes the claim only when this
  session holds it.
- `relais programme next [--from <repo>:<slug>] [--claim] [--root <dir>] [--json]`:
  1. the first unheld ready ticket of the `--from` programme, in frontier
     order (due first);
  2. otherwise, the first unheld ready ticket of a programme in which no
     ticket is held, ordered like the root summary (soonest due, then most
     ready, then name). For a step-2 reservation, hold that programme's
     lock while re-reading all its claims and branch holdings and checking
     that no ticket is held, then write the chosen claim before unlocking.
     If another writer has occupied the programme, skip the whole programme,
     not merely the chosen ticket. Test two simultaneous callers with
     different exhausted source programmes and one empty destination
     programme containing two ready tickets: at most one caller may reserve
     a ticket there;
  3. otherwise, exit 22, a new `CliOutcome::Parked` in `cli/outcome.rs`
     (1 stays `OperationalFailure`), with `parked: next due <date>` on
     stdout (`{"parked":…}` with `--json`), the soonest `due` of
     an open ticket that is not ready yet, or `parked: nothing due`.
  - Tickets whose `mode` needs the person are skipped, like a held ticket.
    Phase 1 reads the mode values in `model.rs` first.
  - Automatic handoff eligibility additionally requires `due` to be absent
    or on/before today in the machine's local timezone. Apply this filter
    in both selection steps; frontier retains its existing listing behavior.
    When parked, report the earliest future due date among otherwise-ready,
    unheld afk tickets; use `parked: nothing due` when none exists. A blocked
    ticket's date is not a promised wake-up date. Record this automatic
    scheduling policy in ADR-0078. Inject today in tests: a future follow-up
    is not selected before its due date and becomes eligible on that date.
  - With `--claim`, the choice is written as a `handoff` claim. When
    `create_new` loses a race, `next` moves to the following candidate. This
    is what keeps two near-simultaneous handoffs apart.
  - The answer is one line on stdout, `<repo> <slug> <id>\t<name>`, or
    JSON; refusals and errors go to stderr.

**Hook.**

- `SessionEnd` deletes the ending session's `work` claims (under the lock); it reads only
  the claims directory.
- `SessionStart` (`resume`/`compact`) and every `claim`/`next` call refresh
  that session's `work` claims' `refreshed_at`; a test shows a refreshed claim outliving N.
- `resume.rs`:
  - `render` adds `(held by <s8>, <age>)` after `cut_name`, so the mark is
    never cut; held rows sort after unheld ones before the
    `TICKET_LINE_CAP` split, and the header counts unheld ready plus
    `M held`;
  - the root expansion ranks by unheld ready count, then the latest
    Handoff;
  - `summary_line` shows `N ready, M held`.
- `frontier` keeps leaving held tickets out, now including claims. `show`
  lists them under in-flight with the holder. Under `--all` a claimed row's
  fifth field is `claimed <s8> <age>` (five fields kept); JSON gains `claim`.

**Record.** ADR-0078 is drafted as `proposed` in a decisions PR that
lands with this phase, so the new meaning of `frontier` is never
undecided; Phase 4 accepts it.

**Skill text.** relais's own `claude-plugin/skills/programme/SKILL.md`
resume step:
1. run `relais programme claim <slug> <id>` first, and again at each commit
   on the ticket (this refreshes `refreshed_at`);
2. on an exit-3 ownership refusal, run `relais programme next --from … --claim`.
   Resolve the returned ticket's map directory and run
   `relais programme claim <slug> <id> --dir <map dir>` successfully before
   starting any work: `next --claim` reserves a handoff, not active work.
   If that takeover returns exit 3, select again; on exit 22 park; on any
   other failure report stderr and stop recovery. Do not redirect on a
   configuration or I/O failure from the original claim. Test that successful
   refusal recovery produces a work claim owned by the current session,
   refuses another session's claim, and is released by SessionEnd.

The skill text is pinned by a test: run
`cargo test -p relais --lib install::`
([[relais_skill_text_is_pinned_by_a_rust_test]]).

### Phase 2 — the Handoff names one unheld ticket (greenharness-skills)

In `parts/workflow/skills/worktree-task/SKILL.md` § Handoff step 4, and
its mirrors in `parts/gate/skills/merge-when-green/SKILL.md` step 7 and
`parts/gate/skills/tag-release/SKILL.md` (line 101). The `programme`
skill is relais's (harness.skills-source), so Phase 1 edits it:

- In a programme, run `relais programme next --from <repo>:<slug> --claim`.
  - The prompt becomes `resume <repo> <programme> <ticket>: <gist>`, and
    nothing open-ended.
  - On exit 22, end with `safe to /clear; parked: next due <date>`. It keeps
    greenbelay's `clear-at-boundary` marker (at `deny`), and the relay's
    `next:` pattern does not match it, so nothing is submitted.
  - On any other non-zero exit, report relais's stderr line, print no
    clear line, and never fall back to an open-ended prompt.
- A next step that is the person's parks too. The map's Handoff already
  carries the person's steps.
- Without relais, name the map's own next ticket, or park. Never name
  another programme from memory ([[feedback_green_harness_parts_optional]]).
- A session starting on a relayed `resume` claims first (relais's skill,
  Phase 1).
- Release with grh as usual.

### Phase 3 — the relay journals what it named (amont-agent / greenbelay)

- `plugin/clear-relay/hooks/register.ts` adds to the `submit` event:
  `named: {repo, programme, ticket}` parsed from
  `^resume (\S+) (\S+) (\S+):`, or `named: null`.
- No refusal.
- A unit test covers both shapes.

### Phase 4 — record and prove (decisions)

- Write **ADR-0078 "A session claims its ticket"**, amending ADR-0077's
  handoff content. It records:
  - the claim;
  - the `next` order;
  - park instead of an open-ended handoff, as
    `safe to /clear; parked: next due <date>`;
  - N with its source.
- Close t26 with the decision and the proof.
- File a due ticket, about 7 days after install: re-count the
  relay journal's `named` triples against the claims log. The bar is 0
  relayed prompts naming a ticket held by another live session, and 0
  open-ended `next:` prompts, 0 step-2 picks naming a programme with a held
  ticket, and the daily count of programmes with ready tickets and no
  session, compared with the 2026-11-19 baseline (duro-app, relais).
- Roll back to advisory if more than 10% of claims end by expiry rather
  than release, or a ticket stays held for more than N h with no live holder
  session.

## Code review

- relais: backend, lang:rust, unix, tui, architect
- amont-agent: backend, lang:typescript (unix and tui left out: no CLI output)
- greenharness-skills: none — no language area (skill text)
- decisions: none — no language area (ADR and ticket text)

## Verification

- **relais unit tests:**
  - `held()` over each row of the table, at the boundaries N and R (a handoff claim aged R → held; aged R + 1 s → not held);
  - `next` ordering, covering own programme, an empty programme and park;
  - takeover and steal;
  - `SessionEnd` keeping `handoff` claims and deleting `work` ones;
  - `render` marks and the root ranking.
- **relais integration test (the ticket's proof, in a scratch state dir):**
  two processes run `next --claim` at the same moment from the same
  programme, which has one ready ticket.
  - The first gets it; the second gets another programme's ticket, or
    parks.
  - Repeat with two ready tickets: they get different ones.
  - Deterministic: a pre-created claim on the first candidate makes
    `next --claim` take the second.
  - 8 threads behind a `std::sync::Barrier`, 50 rounds: no ticket handed
    to two; two concurrent takeovers of one handoff claim: exactly one
    exits 0, the other exits 3.
- **Gates:** `relais programme check` on the fixtures, `cargo test -p relais`
  (including `install::`), and the repository's greengate gates.
- **Live, after install:** two sessions end at a boundary within minutes of
  each other, and their prompts name different tickets. Read them in the
  clear-relay journal's `named` field.
  - A `startup` at `~/Developer/Perso` shows `held by 96c8a791` on a
    ticket a live session claimed.
- **Week:** the due ticket from Phase 4.

## Decision log

- 2026-11-19, the person: claims recorded in a relais claim file (over a
  `prog/` branch alone, or board-derived claims, which fail the proof
  during the gap right after a handoff).
- 2026-11-19, the agent (afk ticket):
  - every ticket held → park with `safe to /clear; parked: next due <date>`;
  - a person's next step parks;
  - the plugin journals rather than refuses;
  - N is measured in Phase 0.

- 2026-11-19, the agent (Phase 0): N is **8 h**, the p99 gap between two
  commits in one session over 14 days (7.51 h; p95 2.01 h; 2,223 gaps in 90
  sessions), not the p95 session lifetime the plan named. Lifetime p95 is
  65 h of mostly idle time and would have clamped N to the 24 h ceiling,
  while a claim is refreshed at each commit. 27 of 770 sessions (3.5%) sent
  no `SessionEnd`. Recorded with its query in decisions t26.
- 2026-11-19, the agent (implementation review): the step-2 lookup trait the
  architect suggested was not added; the ordering tests run against a
  scratch store.

## Verification record (Phase 1, 2026-11-19)

Real binary (`target/debug/relais` at 6e4859b4), a scratch git repository
with programmes `alpha` (t01 ready, t02 due 2026-12-30) and `beta` (t01
ready), `RELAIS_STATE_DIR` a scratch directory.

| Check | Input | Expected | Actual |
|---|---|---|---|
| own programme first | A: `next --from repo:alpha --claim` | alpha t01 | `repo alpha t01-one`, exit 0 |
| idempotent | A again | the same ticket | `repo alpha t01-one`, exit 0 |
| a programme no session is on | B, same `--from` | beta t01 | `repo beta t01-one`, exit 0 |
| due later is skipped, park | C, `--json` | exit 22, next due 2026-12-30 | `{"next_due":"2026-12-30","parked":true}`, exit 22 |
| takeover | D: `claim alpha t01-one --dir …` | work claim, names A | `took over repo alpha t01-one from aaaaaaaa`, exit 0 |
| refusal | E: same claim | exit 3, `next:` and `--steal` | as expected, exit 3, stdout empty |
| release refused | E: `release` | exit 3, a next step | `next: nothing to release; it ends …`, exit 3 |
| ready list marks | `SessionStart` startup | held rows marked | `1 ready, 2 held`, `(held by dddddddd, 0m)` |
| frontier | `frontier docs/programmes/alpha` | held left out | only t02 listed |
| release at end | `SessionEnd` of D | work claim released | `log.jsonl`: `release … dddddddd-4` |
| tests | `cargo test -p relais` (gate) | pass | pass; `programme::` 38, `programme_claims` 16 |

Phases 2–4 are in their repositories (pointer plans); the live proof and
the week's count follow the release.

## Implementation review

- implementation-review: approve-with-changes ×3 (round 1, delta, final delta); 6 + 1 blocking fixed; own-handoff `from` doc gap deliberate: no other holder to name. 94k + 105k + 44k, 245 + 53 + 96 s.
- backend: approve after the delta (5 fixed). 85k + 101k, 180 + 78 s.
- lang:rust: approve-with-changes ×3; 7 fixed; deliberate: the unreachable `(Stole, None)` arm keeps the match total. 82k + 97k + 29k.
- unix, architect: approve after the delta (5 + 4 fixed; architect's lookup trait deliberate: scratch-store tests). tui: approve-with-changes ×3, 7 fixed; deliberate: the unreadable-claim release line (83 columns), a rare path.

<!-- panel: repos=relais,greenharness-skills,amont-agent,decisions adds= reviewers=architect,backend,lang:rust,po,tui,unix,codex body-sha=2ac1da8868e3 -->
