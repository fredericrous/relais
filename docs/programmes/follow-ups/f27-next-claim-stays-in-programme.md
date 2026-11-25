+++
type = "task"
mode = "afk"
status = "open"
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
