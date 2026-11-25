---
status: active
branch: prog/follow-ups/f27-next-claim-stays-in-programme
repos: [relais, decisions, greenharness-skills]
adrs: [ADR-0084]
plan-id: p-01M88SMXS48G1ECGJS98AFPFQQ
---
# Next claim stays in its programme

## Review panel
📄 Full reviews: [reference](2026-11-25-next-claim-stays-in-programme.reviews.md).
👉 **Decide:** none — approve if a handoff that finds its own programme empty should park and list the others, never start one.
📍 relais, decisions, greenharness-skills · 3 phases · next: Phase 1 (ADR-0084). Panel: backend, lang:rust, tui, unix (opus-5-5), codex (gpt-6-astra).
**Changed by review:** `ready elsewhere: <repo>:<slug>` pastes into `--from`; a busy lock retries once then parks; Work step 6 closes through `next`.
**Verdicts:** r1 4 approve-with-changes → r2 4 approve; codex approve-with-changes (1 high applied, delta 3 approve, rust approve-with-changes); 8 carried; words 1029 → 1275 (×1.24); remove 0, merge 0.

## Picture

```
 next --from relais:follow-ups --claim

 today                             after this plan
 1 own handoff, any programme      1 own handoff in follow-ups only
 2 first unheld ready, follow-ups  2 first unheld ready, follow-ups
 3 else first of a programme       3 else park, exit 22, stdout:
   with nothing held (crosses)       parked: next due <its own date>
 4 else park                         ready elsewhere: relais:p8 (2 ready)
 from's lock busy -> step 3        from's lock busy -> exit 1, retry line
 other failed load -> no park      other failed load -> park, partial note
```

The handoff choice stops at the programme it left; a park lists the rest.

## Goal

After a `/clear` relay, a session resumes only on a ticket of the programme
it left: `relais programme next --from <repo>:<slug> --claim` never reserves
a ticket of another programme. When the `--from` programme has nothing
ready, the handoff parks and names the programmes that do have ready
tickets, so starting one is the person's call (chosen by the person on
2026-11-24, ticket `docs/programmes/follow-ups/f27-next-claim-stays-in-programme.md`).

## Non-goals

- `next` without `--from` keeps today's choice (first eligible ticket of a
  programme with nothing held): no programme is being left, so nothing
  crosses. Nothing in the relay path calls it without `--from`; the
  plugin's test only names it as a trigger to re-read the pane.
- No new exit code: 22 stays the park, read by both skills already.
- The clear-relay plugin is unchanged: it arms only on
  ``safe to /clear; next: `…` `` and ignores a `parked:` line
  (`greenbelay/plugin/clear-relay/hooks/register.test.ts:185`).
- Closed tickets and superseded ADRs (0078, 0080, 0083's history) keep
  their text; CHANGELOG's old entry stays.
- The drive loop does not call `next`; it reads `frontier`, which already
  stays in one map.

## Code review

- relais: backend, lang:rust, unix (tui left out: no terminal UI drawn, two stdout lines)
- decisions: none — prose ADR, no code
- greenharness-skills: none — two sentences of skill text

## Behaviour

`relais programme next --from <repo>:<slug> [--claim] [--json]`:

1. With `--claim`, a live `handoff` claim this session holds is returned
   first only when it is in the `--from` programme; one in another programme
   is left to expire with its lease (not released, not returned).
2. The first unheld, eligible (`afk`, due today or earlier) ready ticket of
   the `--from` programme, reserved as `handoff` with `--claim`; a lost race
   moves to the next ticket of the same programme.
3. Otherwise it parks: exit 22, stdout line 1 `parked: next due <date>`
   (the soonest future due of an unheld ready `afk` ticket of the `--from`
   programme only) or `parked: nothing due`, then one line per other
   programme in which no ticket is held and at least one is eligible:
   `ready elsewhere: <repo>:<slug> (<n> ready)` (the `--from` form, so it
   pastes), ordered like the root
   summary (soonest due, most ready, name). Listing is a read: no lock, no
   claim. `--json`:
   `{"parked":true,"next_due":"YYYY-MM-DD"|null,"elsewhere":[{"repo":"…","slug":"…","ready":n}],"elsewhere_partial":false}`
   (`elsewhere_partial` is `true` when another programme failed to load, step 5)
   (`elsewhere` is `[]` without `--from` too).
4. The `--from` lock not taken within 2 s exits 1 (`stayed held for`, then
   `next: <the same command>`), as `claim` does, instead of falling through.
5. A programme other than `--from` that failed to load no longer blocks the
   park: it parks and stderr says `relais: the ready-elsewhere list may be
   partial: a programme failed to load (see above)`. The `--from` programme
   failing to load still exits 1. Without `--from`, a failed load still
   refuses to park (unchanged).

Skill text:

- programme skill, Work step 2, exit 22: park with line 1 only in the clear
  line; when `next` printed `ready elsewhere:` lines, name those programmes
  for the person just above it, and start none of them.
- programme skill, Work step 6: the closing line comes from
  `relais programme next --from <repo>:<slug> --claim` with step 2's exit
  handling, also when worktree-task is absent: a `next:` prompt only on
  exit 0, the parked line on exit 22; the skill pin covers it.
- programme skill, Work step 2: exit 1 from `next` with `stayed held for`
  on stderr: retry once, then park with `safe to /clear; parked: lock busy`.
  worktree-task Handoff says the same.
- programme skill, Drive step 7 Stop: the summary names the programmes
  `relais programme next --from <repo>:<slug>` lists under
  `ready elsewhere:`; starting one is the person's call.
- worktree-task Handoff: `next` chooses the first unheld ready ticket of
  this programme only; exit 22 names the ready-elsewhere programmes above
  the parked line.

## Phases

- [ ] Phase 1 — ADR-0084 replaces ADR-0083's `work.programme-map`
  (restated whole, the `next --claim` sentence changed), in decisions:
  `docs/adr/0084-the-next-ticket-stays-in-its-programme.md`,
  `docs/adr/HEADS.md`, pointer `docs/plans/2026-11-25-next-claim-stays-in-programme.md`.
  Merged before Phase 2 so relais's vendored pack resolves it.
- [ ] Phase 2 — relais: refresh the pack (`.adr/packs/decisions.pack`,
  `.adr/records/HEADS.md`) as its own commit; then
  `crates/relais/src/programme/claim.rs`, `crates/relais/src/programme/claim/rules.rs`,
  `crates/relais/src/cli/programme.rs`, `crates/relais/src/cli/mod.rs`,
  `crates/relais/src/cli/outcome.rs`, `crates/relais/tests/programme_claims.rs`,
  `claude-plugin/skills/programme/SKILL.md`, `crates/relais/src/install/mod.rs`
  (skill pin), `docs/SPEC.md`, `CHANGELOG.md`, and the ticket
  `docs/programmes/follow-ups/f27-next-claim-stays-in-programme.md` and
  `docs/programmes/follow-ups/map.md` (Resolution, Decisions, Handoff).
- [ ] Phase 3 — greenharness-skills: `parts/workflow/skills/worktree-task/SKILL.md`
  Handoff step 4 text, pointer `docs/plans/2026-11-25-next-claim-stays-in-programme.md`.

## Decision log

- 2026-11-25 — A new ADR (0084) replaces 0083's key rather than editing
  0083 in place, because the corpus restates a changed choice whole in a
  new record (0083 replaced 0080 the same way).
- 2026-11-25 — Exit 22 kept, the elsewhere list as extra stdout lines and a
  JSON field, because both skills and the relay already treat 22 as the
  park and read only line 1.
- 2026-11-25 — The `--from` lock timeout becomes exit 1, because with no
  fallback a busy lock would otherwise park on a transient state.
- 2026-11-25 — worktree-task is in scope though the ticket names only
  relais and decisions, because every session's handoff follows its text,
  which would contradict the fix.

## Verification

- Phase 1: `aval resolve work.programme-map` in decisions → ADR-0084 active,
  choice says the next ticket comes only from the session's own programme → <result>
- Phase 2: unit tests in `claim.rs`: from with nothing eligible parks and
  writes no claim in another programme; `next_due` ignores another
  programme's sooner date; `elsewhere` lists it; an own handoff in another
  programme is not returned under `--from`; a busy `--from` lock is an
  error. Integration (`programme_claims.rs`): CLI `next --from a:p --claim`
  with `a:p` exhausted and `b:q` ready → exit 22, stdout line 1
  `parked: nothing due`, line 2 `ready elsewhere: b:q (1 ready)`, no claim
  file under `b`, stderr empty, stdout only those two lines; `--json`
  carries `elsewhere`, and `elsewhere_partial:true` when another programme
  fails to load; a busy `--from` lock → exit 1 with the retry line; three
  programmes → the `ready elsewhere:` lines in the order soonest due, most
  ready, name. Existing tests rewritten, not loosened:
  `two_next_claim_processes_started_together_print_different_tickets` (one
  racer exits 0 with `a1`, the other exits 22 naming `beta:beta` under
  `ready elsewhere:`), the second half of
  `a_from_programme_that_fails_to_load_exits_1_and_a_failed_load_never_parks`
  (now exit 22 plus the partial-list stderr line; without `--from` still
  exit 1), and
  `two_callers_from_exhausted_programmes_reserve_at_most_one_ticket_in_one_empty_one`
  (no claim outside `--from`). Checked to fail on origin/main → <result>
- Phase 2: piloted, the real CLI on this machine:
  `relais programme next --from relais:follow-ups` (look) → a follow-ups
  ticket or a park with ready-elsewhere lines, never another programme's
  ticket; then the same with `--claim` (released after): claim files under
  other programmes' keys unchanged, before against after → <result>
- Phase 2: `cargo test -p relais --lib install::` → the skill pin passes,
  its phrase list extended with `ready elsewhere:`, `start none of them`
  and `parked: lock busy` (and the Drive test with the Stop sentence),
  checked to fail on the old SKILL.md → <result>
- Phase 3: `grep -n "else one of a" parts/workflow/skills/worktree-task/SKILL.md` → no match → <result>

## Outcome

<!-- panel: repos=relais,decisions,greenharness-skills adds=cli reviewers=backend,lang:rust,tui,unix,codex body-sha=009568a3d007 -->
