+++
type = "task"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais", "decisions"]
+++
# `programme next --claim` never crosses programmes

## Question

`relais programme next --from <repo>:<slug> --claim` picks the first
unheld ready ticket of the `--from` programme, "else of a programme with
no ticket held". After a `/clear` relay, the session can therefore resume
on a ticket of another programme without the person choosing it. On
2026-11-24 the person chose: the next ticket never comes from another
programme. When the `--from` programme has no ready ticket, the handoff
parks, and starting another programme is the person's call.

ADR-0083 (`work.programme-map`) describes the current fallback, so it is
amended in the decisions corpus first. Then: what does `next --claim`
print and exit with when it parks, and what do the programme skill's Drive
section and the clear-relay handoff do with that?

## Look at

- `relais programme next` (the `--from` fallback) and its tests.
- `aval resolve work.programme-map` (ADR-0083) in `~/Developer/Perso/decisions`.
- The `programme` skill's Drive and handoff sections (the boundary's clear line).

## Answer looks like

ADR-0083 amended; `next --claim` with no ready ticket in `--from` claims
nothing and says it parked (a distinct exit or field the handoff reads);
a test for it; the skill text says the driver stops there and names the
other programmes with ready tickets for the person.

## Resolution

ADR-0084 (decisions#370) replaces ADR-0083's `work.programme-map`. At a
handoff, the next ticket comes only from the session's own programme, and
starting another programme is the person's call. Plan:
[docs/plans/2026-11-25-next-claim-stays-in-programme.md](../../plans/2026-11-25-next-claim-stays-in-programme.md).

- `relais programme next --from <repo>:<slug> [--claim]` picks only from
  the `--from` programme. A live handoff of this session elsewhere is left
  to expire. With nothing eligible, the command exits 22. Its first line
  stays `parked: next due <date>` (the `--from` programme's own date) or
  `parked: nothing due`. After it comes one
  `ready elsewhere: <repo>:<slug> (<n> ready)` line for each other
  programme with nothing held and some ticket eligible. These are read
  without a lock and never claimed.
- `--json` adds `elsewhere` and `elsewhere_partial`.
- A busy `--from` lock exits 1 (`stayed held for`, then
  `next: <the same command>`) instead of falling through.
- With `--from`, another programme that fails to load, or whose claims
  cannot be read, is left out of the list and flagged partial, and the
  command still parks.
- Without `--from`, the choice is unchanged.
- The programme skill handles each exit: on 22 it parks and names the
  ready-elsewhere programmes, starting none; on a busy lock it retries
  once, then parks with `parked: lock busy`. Work step 6 closes through
  `next`. Drive Stop names the ready-elsewhere programmes.
  greenharness-skills' worktree-task Handoff says the same (branch
  `docs/next-claim-stays-in-programme`).
