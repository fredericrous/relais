---
status: done
branch: feat/review-outcomes
repos: [relais, amont-agent]
adrs:
  - relais ADR-0002 (classifier inputs kept locally)
  - decisions ADR-0022 (plans live in the repo)
---

# Plan review learns which model each reviewer needs

## Review panel

👉 **Decide:** none — approve if a ledger of review outcomes, labels and a per-class report (no cheaper-model trial yet) is how plan review starts to learn.
📍 relais, amont-agent · plan reviewed, narrowed to Phases 0–2 · next: Phase 0 read-only checks. Panel: backend, rust, tui, unix.
**Changed by review:** `record` saves the round-1 body so `miss` can be computed later; thresholds recomputed (19 zero-miss, 30 with one miss); draw, twin and adoption deferred to learning-loop Phase 8 by your D6 choice.
📄 Full reviews: [2026-10-11-plan-review-learns-model.reviews.md](2026-10-11-plan-review-learns-model.reviews.md)
**Verdicts:** 4 approve-with-changes, backend re-read 4 times (all approve-with-changes), no rework; 9 findings carried into implementation.

## Goal

A plan review costs 40–45k tokens per reviewer (77 recorded reviews), 4–10
reviewers per round, all on the session's model. relais already rewrites the
`model` of a subagent spawn from a capability table keyed by task
difficulty; it knows nothing about reviewer roles and gets no outcome for
them (44 of 81 router subagent tasks end `unknown`). This plan gives reviewers an outcome: a label for every reviewer from the
findings ledger the review already writes, and a report of how many labelled
reviews each class has. Trying a cheaper model per role (the draw, shadow
twin and adoption) is deferred to the learning-loop plan's Phase 8, decided
on that data; its design notes stay below.

## Non-goals

- No escalation inside a plan: a cheap reviewer is not retried on a stronger
  model. The existing second looks stay the net: `plan-review-backend`
  re-reads the final body, `implementation-review` reads the diff.
- No change to which roles run (`amont-agent plan-panel` stays deterministic).
- `plan-review-backend` and `plan-review-architect` are never routed down.
- No Laya in this plan (see Phase 5).
- No change to the 3-round bound or the hook's pass rules.
- Not in scope (Phase 8, not built here): the allowlisted draw, the shadow
  twin, per-class adoption and rollback, and the adoption report rows. The
  learning-loop plan's D6 stays untouched.

## Behaviour

1. **Ledger rows.** When the main session finishes the merge step, one
   command, `amont-agent plan-review record`, writes a row per reviewer:
   role, agent id, model, tokens, seconds, round, plan body sha, plan
   features (areas, repos, body lines), verdict, and each finding's severity
   and disposition (`accepted`, `rejected`, `decision`). The dispositions
   come on stdin as JSON (one object per finding: role, severity,
   disposition); everything else is read from the transcript. It is called
   once per round. The agent id is the join key with relais and the dedupe
   key (unique per launch), NOT (plan sha, round). Relais ingest is an
   upsert, so a duplicate line is harmless.
   Write path: `journal::open_private` (O_APPEND|O_CREAT, 0600; directory
   via `private_dir`, 0700), one `write_all` of one serialized buffer per
   row, under an exclusive `flock` on `outcomes.jsonl.lock` around
   read-dedupe → append, so SIGINT mid-write leaves no half row. Exit codes:
   0 rows written or already present (prints `wrote N rows (round R, plan
   <sha12>)` or `already recorded round R of <sha12>; nothing written`);
   1 the ledger cannot be written (reason on stderr; the skill must never
   fail a review because of it), and also a reviewer whose agent id is
   missing from the transcript (named, with the transcript path, on
   stderr); 2 usage.
2. **Labels.** relais joins a row to the router's subagent task by agent id.
   Outcome of a reviewer task: `miss` when a later reviewer (backend on the
   final body) raises a
   `high` finding that falls in this role's remit and the earlier one did
   not; `clean` otherwise; `unknown` when the plan never reaches a push.
   "Remit" is a declared role→area table in amont-agent (checked in,
   tested), not a judgment. A `miss` also requires that the later `high`
   finding's section exists unchanged in the round-1 body (diff against the
   round-1 body `record` saved, found by the sha stored in the row; the
   skill's scratch copy is session-local and gone by the time `relais learn
   cycle` runs); otherwise the label is `unknown`.
   Adding implementation-review as a later reviewer is Phase 8 work.
   `miss|clean` are not router outcomes (`Outcome::rank`,
   router/outcome.rs; `RouterTaskRow.outcome`, ledger/router.rs): they live
   in a separate ledger table `review_outcomes` keyed by agent_id
   (idempotent upsert), with a new `LabelSource::Review`
   (learn/features.rs ~l.371) and a bumped `DATASET_VERSION`
   (learn/dataset.rs:49).
3. **Draw (Phase 8, not built here).** For spawns of an allowlisted set of read-only reviewer types, the
   router draws, seeded and with the propensity recorded, between the table's
   tier (control) and one tier cheaper. Exploration stops per stratum once
   its decision is made. Strata start as ONE pooled class, "checklist" (all
   routable roles: language, unix, tui, ui-design, react, game-ux, po,
   platform, ux-research), not role × size bucket. Backend and architect are
   never routed. A role is split out of the pool only when it has 19 of its
   own labelled reviews.
4. **Shadow twin (Phase 8, not built here).** For about 1 in 6 cheap draws, relais queues a native
   agent run of the same stored prompt at the control tier, during idle, and
   discards its text. The twin must review the same text as the live
   reviewer: the plan body (and brief) is snapshotted at spawn into the
   stored prompt file, the plan sha both arms ran on is recorded, and a pair
   is scored only when both shas match. It is scored against the live
   review: twin-only `high` findings count as potential misses, matched by
   section heading. The stored prompt lives in the relais ADR-0002 day
   files: same 0600/0700 modes, 90-day deletion, same off switch
   (`keep_classifier_inputs=false` disables twins).
5. **Decision (Phase 8, not built here).** Constant: `ONE_SIDED_95 = 0.10` (relais learn/cycle.rs:44),
   passed to `clopper_pearson_upper(k, n, alpha)` (router/stats.rs:57, which
   halves alpha). Rule: adopt the cheap tier for a class when the upper
   bound of its miss rate is below 15% (threshold read from config, never a
   literal in the report). With 0 misses the one-sided 95% bound is
   1−0.05^(1/n), so adoption below a floor f needs n ≥ ln(0.05)/ln(1−f):
   at 15% that is 19 zero-miss reviews (bound 14.6%); at 10%, 29; at 20%,
   14. With 1 miss at the 15% floor the bound clears at n=30. Only `clean`
   and `miss` reviews count toward n; an `unknown` review (its plan never
   reaches a push, or no later reviewer ran) does not. A `miss` in an
   adopted class rolls it back. Time to decision: plans needed = 19 ÷
   (cheap draws per plan × labelled fraction); with roughly 2–3 cheap draws
   per plan and a labelled fraction near 50%, the pooled class decides
   after roughly 16 plans (restated from the Phase 0 measurement); per
   role it would take far longer. The tier in force is stored per stratum
   as its own ledger row, with a `learning_cycles` row as the audit;
   rollback is per stratum. `Registry::promote` is not used (it holds one
   global active/previous pointer, learn/registry.rs:439-448).
6. **Ingest.** `relais native review-outcome` has flags `--ledger <path>`
   (default the amont-agent path) and `--dry-run` (prints the labels it
   would assign); it prints `labelled N: clean a, miss b, unknown c`. The
   learn cycle runs it at the start of each `relais learn cycle` (Phase 2 of
   the learning-loop plan); a ledger row relais never reads just waits for
   the next cycle. Trust boundary: it refuses (exit 3, README:62) a ledger
   that is a symlink, not owned by the running uid, or wider than 0600; rows
   are validated with `deny_unknown_fields`; the file never alters
   machine.toml. Its exit codes go into the README table and the
   `exit_codes_follow_the_documented_table` test
   (crates/relais/tests/portable_scenarios.rs ~l.1048) is updated.
7. **Report.** `relais report` has a per-class section (class = reviewer
   role for now), rows within 80 columns, top 5 classes then `+N more`, also
   present in `relais report --json`. Per row: class; n (labelled reviews);
   clean; miss; unknown; tokens by model, printed with a unit and a column
   header; and "still learning" wording until a class is decided. Empty
   state: "no reviewer outcomes yet: relais learn cycle ingests them".
   (Phase 8, not built here: per-class adoption rows with the state as a
   word (`adopted`, `exploring`), tier in force, tokens saved since
   adoption, and the rollback explained in words: date, class, plan, and
   which later reviewer raised the high finding.)

## Phases

- [x] Phase 0 — read-only checks, before any code: (a) router `mode` and
      envelope on this machine, (b) whether `plan-review-*` spawns reach
      `routeSpawn` as routed or abstained today (`state.pins`), (c) that the
      subagent agent id seen by relais (relais routing.ts:412
      `result.agentId`) equals the one amont-agent reads from the transcript
      (`completed_agents`); fallback join if the ids differ: session id +
      subagent type + launch order, (d) the fraction of the 77 recorded
      reviews (docs/plans/*.reviews.md across the repos) whose plan reached
      a push; the time-to-decision estimate is restated from it. Results
      recorded in the Decision log.
- [x] Phase 1 — amont-agent: lift the `toolUseResult.agentId` capture in
      `completed_agents` (src/plan_review.rs ~l.813-816) out of the
      `resumable` branch and extend `Completed` (~l.685) with agent_id,
      model, tokens, duration. `amont-agent plan-review record` (stdin JSON
      dispositions, exit codes and locked append as in Behaviour 1) writes
      rows to `~/.claude/amont-agent/plan-review/outcomes.jsonl`; it also
      saves the round-1 body (0600, in a `bodies/` directory beside
      `outcomes.jsonl`, named by its sha, written once) and stores that sha
      in each row, for the `miss` label; the
      plan-review skill's step 4 calls it once per round, and the skill
      writes each reviewer's model into the `## Review panel` line so the
      person sees which ran cheap (one-line skill change). Tests: row shape
      and file modes (as tests/plan_review.rs:784-789), idempotence by
      agent_id, and two concurrent `record` processes on the same transcript
      yield one row per reviewer; the saved body is written once and is mode
      0600.
- [x] Phase 2 — relais: `relais native review-outcome` (Behaviour 6), join
      by agent id, label `miss|clean|unknown` into the `review_outcomes`
      table, `LabelSource::Review`, `DATASET_VERSION` bump, README exit-code
      table and `exit_codes_follow_the_documented_table` update. Tests for
      the three labels, the `unknown` case when the section is not in the
      round-1 body, and the exit 3 refusals (symlink, other uid, mode wider
      than 0600). The per-class report section (Behaviour 7) ships here too;
      check `relais report | awk 'length>80'` is empty.
      Exit criterion: the labelled data answers "how many reviews per class,
      and what fraction are labelled", so Phase 8 starts from numbers.
- [ ] Deferred to the learning-loop plan's Phase 8 (subagent exploration,
      decided on data). Design notes for that phase, not built here: reviewer
      allowlist, seeded draw with propensity, the shadow twin queue
      (plan-body snapshot at spawn, sha-matched scoring, ADR-0002 day-file
      retention) and scoring. Exploration stays main-session only until then.
      Checks for that phase: spawn an allowlisted reviewer 30 times against a
      fake clock and seed → arm split within the seeded draw, propensity
      recorded; a twin whose plan sha differs from the live arm is not scored
      → observed.
- [ ] Deferred to Phase 8 — design notes for that phase, not built here:
      per-stratum adoption and rollback stored as a tier row per stratum plus
      a `learning_cycles` audit row (no `Registry::promote`), and the adoption
      report rows. Checks for that phase: seed a class with `n_min` clean rows
      from config → adoption tier row and `learning_cycles` row; add one miss
      → rollback row, explained in words in `relais report`; the report width
      check stays empty → observed.
- [ ] Phase 5 (data-gated, not built here) — Laya as a feature model on plan
      text ("will this role find a high finding?"), allowed only when the
      strata learner is saturated and a stratum has thousands of rows; joins
      the learning-loop plan's Phase 8.

## Decision log

- 2026-10-11 — start with strata, not Laya: ten roles by a few size buckets
  is a lookup table; Laya needs thousands of labelled rows.
- 2026-10-11 — the model switch is relais's existing `routeSpawn`; this plan
  adds labels and exploration, not a second mechanism.
- 2026-10-11 — pooled class first: per-role strata would take far longer
  than the data allows (77 reviews over history; `unknown` labels do not
  count toward n, so the pooled class decides after roughly 16 plans at a
  ~50% labelled fraction).
- Phase 0 (d): labelled fraction of the 77 recorded reviews — to be
  measured; the time-to-decision estimate is restated from it.
- 2026-10-11 — Phase 0 results (read-only, this machine):
  (a) router mode `on` for every decision through 2026-10-11T08:06Z; of 72
  subagent decisions 17 were holdout (applied=0), none explored.
  (b) `plan-review-*` spawns are routed by the table, not pinned (reason
  `table`, applied=1): this plan's own panel ran backend, rust and unix at
  `escalation` (claude-fable-5-1) and two backend re-reads at
  `implementation` (claude-sonnet-5-5). So the table already routes backend
  down by difficulty; the "never routed down" non-goal binds Phase 8's
  draw, and Phase 8 must pin backend/architect if it is to hold. 3 of the
  8 spawns (tui, two backend re-reads) have no router row at all.
  (c) the ids agree: `toolUseResult.agentId` in the transcript equals
  `router_tasks.agent_id` for every reviewer that has a row; no fallback
  join is needed. Model, tokens and seconds are in the transcript too:
  `resolvedModel` on the launch result; `totalTokens`/`totalDurationMs` on a
  foreground result, `<subagent_tokens>`/`<duration_ms>` in a background
  notification.
  (d) of 43 reviewed plans in `~/.claude/plans`, 35 landed in a repository
  (body sha found in a `docs/plans/` file), carrying 74 of their 85
  reviews: a pushed fraction near 0.87. Restated: 19 ÷ (2–3 × 0.87) gives
  roughly 7–11 plans for the pooled class, before the unchanged-section
  condition turns some of those labels `unknown`.
- 2026-10-11 — per-stratum tier rows, not `Registry::promote`: it has a
  single global active/previous pointer.
- 2026-10-11 — D6 decision: wait for Phase 8. This plan builds Phases 0–2
  only; the allowlisted draw, shadow twin, adoption and rollback are deferred
  to the learning-loop plan's Phase 8 (subagent exploration, decided on
  data). learning-loop D6 stays untouched.
- 2026-10-11 — miss floor 15%: 19 zero-miss reviews, or 1 miss at 30; taken
  as the Phase 8 default. D6 is left for Phase 8.

- 2026-10-11 — implementation: `LabelSource::Review` and the
  `DATASET_VERSION` bump move to Phase 8 (carried finding): the
  `review_outcomes` table feeds no dataset yet, so nothing needs a
  rebuild. "Reaches a push" is read as: a `docs/plans/*.md` in one of the
  row's repos (under amont-agent's plan root, `AMONT_AGENT_PLAN_ROOT` or
  `~/Developer/Perso`) carries `body-sha=` of one of the plan's rounds. The
  remit table ships in every ledger row (`remit`), so relais never calls
  amont-agent to read it.
- 2026-10-11 — the plan-review skill is `~/.claude/skills/plan-review/SKILL.md`,
  a local file in no repository; it was changed in the implementing session
  (step 4 runs `plan-review record` before the plan is edited, step 7's 📍
  line names each role's model). It calls a command only amont-agent >
  2.33.1 has, so the amont-agent release carrying Phase 1 must ship before
  the next plan review records anything.

- 2026-10-11 — implementation: the report states "still learning" once, in
  the section header, not on every row: no class can be decided before
  Phase 8, and the 80 columns are spent on tokens by model (whole models,
  then `+N`). Phase 8 brings a per-row state back when states can differ.
  A later in-remit `high` finding whose section was absent from round 1,
  changed since, or not named labels the reviewer `unknown`
  (`section_changed`), as Behaviour 2 says; a missing body is
  `missing_body`.
- 2026-10-11 — implementation: the Phase 2 relais run was interrupted
  (its plugin heartbeat lapsed while this session blocked on a CI poll);
  its preserved candidate was applied and finished in-session, recorded
  as `salvaged`.

## Verification

- Phase 0: read-only commands above → facts recorded → observed.
- Phase 1: run a real plan-review round, call the record command → one row
  per reviewer in `outcomes.jsonl` (mode 0600), rerun writes nothing new
  (exit 0, "already recorded"), two concurrent runs yield one row per
  reviewer → observed 2026-10-11, branch binary on this plan's own review
  transcript, scratch config dir: `wrote 8 rows (round 5, plan
  1a0350da061a)`, 8 reviewers with agent id, model, tokens and seconds;
  rerun exit 0 `already recorded round 5 of 1a0350da061a; nothing
  written`; two concurrent runs from an empty dir → 8 rows; dirs 0700,
  ledger 0600, the round-5 body saved.
- Phase 2: replay the ledger of a past plan → labels match a hand reading of
  its reviews file; `--dry-run` prints them without writing; a symlinked
  ledger exits 3; `relais report` shows n, clean, miss, unknown and tokens by
  model per class with "still learning" wording, and `relais report |
  awk 'length>80'` is empty → observed 2026-10-11, branch binary, scratch
  `RELAIS_STATE_DIR`, the Phase 1 ledger of this plan's own review (8 rows,
  one backend `high` finding on `Behaviour` with no area, only the round-5
  body saved): `labelled 8: clean 3, miss 0, unknown 5 (not_pushed 0,
  no_later_review 1, missing_body 4, section_changed 0)`, which matches the hand reading:
  rust, tui and unix clean (the finding names no area in their remit),
  backend rounds 1–4 `missing_body` (the round-1 body was never saved),
  round 5 `no_later_review`. Re-ingest gave the same (also after the review fixes); a symlinked ledger
  exited 3 (`… is a symlink`). The report's Reviewers section lists the four
  classes with tokens by model (`opus-5-5 56k, sonnet-5-5 55k, +1`) under
  "every class still learning"; `awk 'length>80'` printed 0 lines.
  Phase 3–4 checks sit under their deferred notes in Phases.

## Implementation review

- relais: approve on the Delta (100k, 34 s); round 1 approve-with-changes (85k, 63 s): 6 findings, all fixed in 5ecb907 (section_changed unknown, unreadable report line, cycle settings as input, preview/ingest split, exhaustive match, exit 1).
- amont-agent #94: approve on its final round, after 14 findings over four rounds, all fixed; recorded in its pointer plan.
- relais, after CI lint (`check-module-cycles.py`: one trailing test module) moved two tests in ed6e0ed: approve, fresh round (102k, 17 s).
- deliberate: the other-uid refusal is code-only (chown needs root); "still learning" is a header line (Decision log).

## Outcome

Reviewers now have an outcome. amont-agent ≥ the release after 2.33.1
records each panel round (`plan-review record`, amont-agent #94); relais
labels them `clean|miss|unknown` (`native review-outcome`, run at the start
of every `learn cycle`) and `relais report` shows n, clean, miss, unknown
and tokens by model per reviewer role. On this plan's own panel: 3 clean,
5 unknown, because the round-1 body predates `record`. The draw, twin,
adoption and rollback stay with the learning-loop plan's Phase 8, which
starts from these counts.

<!-- panel: repos=relais,amont-agent reviewers=backend,lang:rust,tui,unix body-sha=1a0350da061a -->
