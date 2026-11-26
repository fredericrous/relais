---
status: active
branch: docs/relais-modularise-programmes
repos: [relais]
adrs: []
plan-id: p-01M8B3D6GNNDMMM1PJYKV09XT2
---
# Plan: chart the programme "relais-modularise" (programme → epic, with an activity-vision projection)

## Review panel

👉 **Decide:** I split the work into two programmes (modularise, epic-vocabulary) on PO and architect advice; approve, or ask me to merge them.
📍 relais · 1 phase · 2 programmes, 6 stories · next: chart both in one PR. Panel: backend, architect, po, tui, unix, rust (all opus).
**Changed by review:** split into two programmes; verification commands made runnable; t01-vocabulary absorbed the alias, claim and ADR-0051 points.
**Verdicts:** 6 approve-with-changes; 4 carried (round 2: tie between programmes, `--no-fetch`, title, noted); words 1266 → 1710 (×1.35, over ×1.25: mostly fixes and the second map); remove 0, merge 1.
codex: not run (markdown-only plan, no risk named)
📄 Full reviews: [2026-11-26-relais-modularise.reviews.md](2026-11-26-relais-modularise.reviews.md)

## Picture

```
 git (truth)                  activity-vision (view)
 epic.md      ──sync──►      milestone  (one per epic)
 sNN-*.md     ──sync──►      task       (one per story)
    ▲                            ▲
    │ claim, branch              │ evidence: branches, PRs,
    │ prog/<slug>/<id> (t01)     │ sessions attach by branch
    └──── same branch name ──────┘
 ticket-vision: untouched (ITSM tickets)
```

This is the destination of `epic-vocabulary`, not what this plan builds: git holds the epics and stories; activity-vision shows them as milestones and tasks and attaches evidence by branch name. This plan only charts the two programmes; the branch prefix stays `prog/` until vocabulary t01 decides.

## Context

Two things were asked for:

1. **Draft a programme** about splitting relais (one crate, ~229k lines, `crates/relais`) into smaller modules before it is renamed. The rename to greenlift has been on hold since 2026-11-17.
2. **Rename "programme map" to "epic"** (shorter, and says what it is), and **think about integration with activity-vision**.

Facts found while exploring (read-only):

- **Programmes today.** A programme is `docs/programmes/<slug>/map.md` plus one ticket file per decision (TOML front-matter: `type`, `mode`, `status`, `blocked_by`, `repos`, `due`). The skill is `relais/claude-plugin/skills/programme/SKILL.md`, the spec is `relais/docs/SPEC.md` §31, and the CLI is `crates/relais/src/cli/programme.rs` plus `crates/relais/src/programme/`. The decision key is `work.programme-map`, headed by ADR-0084 (`decisions/docs/adr/HEADS.md:89`).
- **How far the word reaches.**
  - 34 maps and 667 files across 13+ repos.
  - The word appears in 159 relais files, 128 decisions files, 55 homelab, 53 greenbelay and 36 amont.
  - It is also in the branch prefix `prog/<slug>/<id>`, in the session-start hook text (`relais/programmes:` in SPEC:835 and `hook/resume.rs`), in the plugin hooks (`claude-plugin/hooks/programme.ts`) and in the handoff contract `relais-handoff/1`.
  - No MCP tool id contains "programme", so the plugin API does not change.
- **activity-vision is not a Jira equivalent today.**
  - It is an evidence inbox (React Router 7 + Effect, Postgres). Only `/inbox` ships. The board is "step 4" and is unbuilt.
  - Its data model has `tasks` (`next | in_progress | blocked | done`), `milestones` and `projects`. It has no epics, sprints or assignees.
  - Its HTTP API is ingest only: `POST /api/v1/events` and a Forgejo webhook. There is no read or write API for tasks, and no MCP server.
  - Its table `external_refs(entity_type, entity_id, system, external_id, ...)` is already there for linking.
  - Its invariants: evidence never creates a task, no rule ever closes a task, and rules never overwrite a field last set by the person.
- **ticket-vision is the ITSM tracker** (tickets, SLAs, changes, a REST API, an MCP server). It is not agile-shaped and it has no epics. Its word "ticket" collides with ticket files inside a programme.

Decisions made with the user:

- **Git is the source of truth.** activity-vision *projects* epics (one-way first). Claims, `prog/` branches, handoffs and the ADR-0080 driver stay as they are.
- **Vocabulary:** programme → **epic**, ticket → **story**. `type = task` stays a story kind.

## What this plan produces (when approved)

Two programme directories in the **relais** repo, charted in today's vocabulary because the tooling only reads `docs/programmes/` until the rename ships. The review panel (PO, architect) found that the split and the vocabulary rename share no blocker, so they are two programmes. Work happens in a per-task worktree from `origin/main` (`worktree-task` skill), as one branch and one PR, with one commit per file group.

Locations: `relais/docs/programmes/relais-modularise/` (the split) and `relais/docs/programmes/epic-vocabulary/` (the rename and the activity-vision projection).

### `relais-modularise/map.md` (sections as `MAP_SECTIONS` requires)

- **Destination.** relais (today a workspace with one member, `crates/relais`) is a workspace of several member crates with enforced boundaries. Each piece that is a separate tool has its own name, chosen once, and the rename to the family name happens once. Done when two targets, set at t01 from the baseline below, are met.
- **Notes.** relais is 229,504 lines in one crate. Baseline to record when charting: the build time after a one-line edit, and the largest file's line count. The module groups are routing/learning, execution, verification, host scheduling (overlaps greengate's slot queue, ADR-0014), the programme module, plugin/hook/install/doctor and the ledger. The family name is greenlift (ADR-0076, `harness.bundle-names`); a stray memory note saying greenguide is wrong, since decisions#174 rejected it. The rename of the tool and the vocabulary migration of `epic-vocabulary` ship in one window, so hook text and branch prefix change once.
- **Decisions so far.** Git is truth and activity-vision projects it (decided in `epic-vocabulary`). Epic and story are the new words.
- **Not yet specified.** Workspace-split implementation stories (one per crate, after t02). Their order against the rename implementation, since both rewrite `crates/relais/src/programme/` and `cli/programme.rs`: one lands before the other starts.
- **Out of scope.** Splitting relais into separate repositories before the workspace split proves the seams. Moving any state out of git.

### `epic-vocabulary/map.md`

- **Destination.** "Epic" and "story" replace "programme" and "ticket" across the harness with a compatibility window, and activity-vision shows epics and stories as milestones and tasks.
- **Notes.** The word reaches 34 maps, 667 files and 13+ repos. activity-vision's plan (`docs/plans/2026-10-08-...`) has the board unbuilt and no write API; ticket-vision is the ITSM tracker and stays untouched.
- **Decisions so far.** Git is truth; activity-vision projects one-way. Epic/story.
- **Not yet specified.** The rename implementation (CLI alias, dual directory read, hook text, migration of 34 maps), the activity-vision sync build, and the activity-vision board in its own epic in that repo.
- **Out of scope.** Two-way sync. Moving any state out of git.

### Stories of `relais-modularise` (ids keep today's scheme until the rename migrates them)

| id | type / mode | blocked_by | question |
|---|---|---|---|
| t01-coupling-map | research / afk | none | What is the module-to-module dependency graph of `crates/relais`? Who owns each ledger table, what state is shared, and which of the 5–7k-line `mod.rs` files (`verify`, `admission`, `coordinator`, `report`) hide several jobs, counted in non-test lines (inline `#[cfg(test)]` modules excluded, method stated)? Which ties block a crate split: `include_str!` paths (13 sites), the `conformance` feature, the pinned skill-text and policy-hash tests, and the `pub(crate)` surface? Record the baseline. |
| t02-seams | grilling / hitl | t01 | Which crates, in what order, and which of them (if any) become separate tools later? |
| t03-slot-overlap | research / afk | none | Do `admission`/`coordinator`/`procs` duplicate greengate's (was amont) host slot queue (ADR-0014), and can one of them go? |
| t04-names | grilling / hitl | t02 | What are the names of the pieces and of the family, once? Greenlift is settled by ADR-0076. Includes the 37 repos' `relais.toml`, and shipping with `epic-vocabulary`'s migration in one window. |

### Stories of `epic-vocabulary`

| id | type / mode | blocked_by | question |
|---|---|---|---|
| t01-vocabulary | grilling / hitl | none | What exactly renames, what stays, and how long do both spellings work? Covers `docs/epics/`, `epic.md`, the `sNN` ids, the branch prefix (both `prog/` and `epic/` count as a claim for the whole window, so a branch in flight never shows as ready again; `frontier` treats both as in flight), which directory wins when a slug is in both `docs/programmes` and `docs/epics`, the session-start text (header, every noun in it such as "ticket", and the printed commands `relais programme check/list` switch in one release; list every consumer matching the header string), the `relais epic` command with `programme` as an alias (its deprecation note goes to stderr only, never `--json` or hook output, and names `relais epic <same args>`; exit codes and `--json` keys stay the same), the `relais-handoff/1` contract, and migration of 34 maps. Output: a decisions ADR superseding the `work.programme-map` head (ADR-0084) and the `harness.skills-source` head (ADR-0051), which names the `programme` skill. |
| t02-activity-projection | research / afk | t01 | How do epics and stories map onto activity-vision? Proposed mapping: epic → milestone, story → task; open with no claim → `next`, claimed or on a branch → `in_progress`, `blocked_by` open → `blocked`, closed → `done`, `due` → the task's date. Needs a new provenance ("planned in git") because "evidence never creates a task" would otherwise forbid it, a write endpoint, `external_refs` keyed `<repo>:<slug>/<id>`, and a decision on whether the board (step B) must exist first. |
Each story has `## Question` (required); each afk story also has `## Look at` and `## Answer looks like`, so `check` gives no warnings.

Frontier at the start: `relais-modularise` t01, t03; `epic-vocabulary` t01 (hitl, so the ADR-0080 driver does not take it). Blocked by `blocked_by`: modularise t02, t04 and vocabulary t02, which is afk and becomes ready for the driver once t01 closes.

`repos`: vocabulary t01 is `["relais","decisions"]`, vocabulary t02 is `["activity-vision"]`, the rest default to relais.

## Critical files

- Create: `relais/docs/programmes/relais-modularise/map.md` and its four stories, and `relais/docs/programmes/epic-vocabulary/map.md` and its two stories.
- Read for the ticket bodies (no edits): `relais/claude-plugin/skills/programme/SKILL.md`, `relais/docs/SPEC.md` §31, `relais/crates/relais/src/programme/{model,parse}.rs`, `activity-vision/docs/plans/2026-10-08-activity-vision-implementation-plan.md`, `activity-vision/app/lib/db/migrations/pg/0003_planning.ts`, `decisions/docs/adr/HEADS.md:89`.
- Not touched by this plan: any Rust or TypeScript, any ADR, any other repo.

## Verification

1. `relais programme check docs/programmes/relais-modularise; echo $?` and the same for `epic-vocabulary`: exit 0 and empty stderr (problems exit 2, warnings print to stderr; unknown front-matter keys are errors).
2. `relais programme frontier docs/programmes/relais-modularise --no-fetch` exits 0 and lists exactly t01 and t03; the same for `epic-vocabulary` lists exactly t01. (`frontier` needs the directory argument.)
3. `relais programme list --no-fetch --root ~/Developer/Perso` shows both programmes.
4. The project checks run locally before the push (`greengate`), the push goes through `amont land`, and the PR merges on green with `merge-when-green`. After the merge, `relais programme frontier` on origin/main shows the same lists.

## After this plan

The next sessions run modularise t01 and t03 (the ADR-0080 driver can take them) and vocabulary t01 (the person). The rename implementation and the activity-vision sync are charted after vocabulary t01 and t02 close.

## Phases

### Phase 1: chart both programmes and land them

Files: `docs/programmes/relais-modularise/map.md`, `docs/programmes/relais-modularise/t01-coupling-map.md`, `docs/programmes/relais-modularise/t02-seams.md`, `docs/programmes/relais-modularise/t03-slot-overlap.md`, `docs/programmes/relais-modularise/t04-names.md`, `docs/programmes/epic-vocabulary/map.md`, `docs/programmes/epic-vocabulary/t01-vocabulary.md`, `docs/programmes/epic-vocabulary/t02-activity-projection.md`.

Then check with `relais programme check`, push through `amont land`, merge on green (the same PR).

## Non-goals

No Rust or TypeScript change, no ADR, no rename, no activity-vision code in this plan. It only charts the epic that decides them.

## Code review

- relais: none — markdown only
- decisions: none — no file edited
- activity-vision: none — no file edited

<!-- panel: repos=relais adds=contract reviewers=architect,backend,lang:rust,po,tui,unix body-sha=b2a9c0bccdd0 -->
