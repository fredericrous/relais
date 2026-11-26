# The harness says epic and story, and activity-vision shows them

## Destination

"Epic" and "story" replace "programme" and "ticket" across the harness with
a compatibility window, and activity-vision shows epics and stories as
milestones and tasks. Git stays the source of truth.

## Notes

- **Origin:** session of 2026-11-26. "Programme map" is long, and "epic" says
  what it is. "Ticket" collides with ticket-vision's ITSM tickets.
- **How far the word reaches:** 34 maps and 667 files across 13+ repos
  (relais 159 files, decisions 128, homelab 55, greenbelay 53, amont 36).
  It is also the CLI subcommand `relais programme`, the directory
  `docs/programmes/`, the branch prefix `prog/<slug>/<id>`, the session-start
  text (`relais/programmes:`, `hook/resume.rs`, `SPEC.md` section 31), the
  plugin hooks, and the handoff contract `relais-handoff/1`. No MCP tool id
  contains it.
- **activity-vision** has tasks and milestones but no epics, no board (step 4
  of its plan is unbuilt) and no write API. Its tables include
  `external_refs`. Its rules: evidence never creates a task, no rule closes a
  task, and no rule overwrites a field the person set. ticket-vision is the
  ITSM tracker and stays untouched.
- **Governing decisions:** `work.programme-map` (ADR-0084),
  `harness.skills-source` (ADR-0051, names the `programme` skill),
  `work.programme-driver` (ADR-0080), `harness.cross-tool-reads`.
- **Neighbouring programme:** relais `docs/programmes/relais-modularise`. Its
  t04 asks whether the tool rename ships in one window with this migration.

## Decisions so far

- Git is the source of truth. activity-vision projects epics one-way, and
  claims, `prog/` branches, handoffs and the ADR-0080 driver stay as they are
  (the person, 2026-11-26).
- Programme becomes epic and ticket becomes story. `type = task` stays a
  story kind (the person, 2026-11-26).

## Not yet specified

- The rename implementation: the CLI alias, the dual directory read, the hook
  text, and the migration of 34 maps.
- The activity-vision sync build, and the activity-vision board in its own
  epic in that repository.

## Out of scope

- Two-way sync between git and activity-vision.
- Moving any state out of git.
