+++
type = "research"
mode = "afk"
status = "open"
blocked_by = ["t01-vocabulary"]
repos = ["activity-vision"]
+++
# How epics and stories map onto activity-vision

## Question

How do epics and stories map onto activity-vision? The proposed mapping:
epic to milestone, story to task. An open story with no claim is `next`; one
claimed or on a branch is `in_progress`; one with an open `blocked_by` is
`blocked`; a closed one is `done`; `due` is the task's date. What does that
need: a new provenance ("planned in git"), since "evidence never creates a
task" would otherwise forbid it; a write endpoint, since the API only ingests
events today; `external_refs` rows keyed `<repo>:<slug>/<id>`; and a decision
on whether the board (step B of its plan) must exist first?

## Look at

- `activity-vision/docs/plans/2026-10-08-activity-vision-implementation-plan.md`
  (the invariants, steps A-C).
- `activity-vision/app/lib/db/migrations/pg/0003_planning.ts` (tasks,
  milestones, `external_refs`) and `app/lib/services/Planning.server.ts`.
- `activity-vision/app/routes.ts` and `app/lib/capture/handlers.server.ts`
  (the two endpoints and their auth).
- `blue-factory/docs/programmes/blue-factory/map.md` (the split with
  ticket-vision).

## Answer looks like

A mapping table (epic and story fields to milestone and task fields, status
by status), the API and auth the sync needs, the changes to the invariants it
forces, and an order: what must exist in activity-vision first. One line says
whether the board is a prerequisite.
