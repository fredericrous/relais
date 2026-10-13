---
status: active
branch: feat/task-types
repos: [relais]
adrs:
  - relais ADR-0006 (a change may run below the implementation tier when its declared task type's recipe has the evidence)
---

# Route relais runs by task type: Haiku where it holds, Sonnet elsewhere

## Review panel

👉 **Decide:** the screening cap, about $35 (5 replays per arm; the pilot replaces the estimates); the extension to 15 per continuing arm is funded separately once screening shows which arms continue. Otherwise approve if typed recipes promoted on Haiku replays, with and without a program design, are how relais moves work off Sonnet.
📍 relais · plan reviewed (+ program-design arm, D5) · next: PR 1 (types, export/import, replay under `Admission::Replay`), then classification, pilot and replays. Panel: architect, backend, rust, typescript, po, tui, unix.
**Changed by review:** Haiku was never eligible for a change (eligibility, `Admission`, per-task risk floors); one recipe per type and arm, `-designed` measured as shipped; a mandatory typed-research promotion gate.
📄 Full reviews: [2026-10-12-task-types.reviews.md](2026-10-12-task-types.reviews.md)
**Verdicts:** round 1: 5 approve-with-changes, 2 rework (same blocker); round 2: 3 approve-with-changes; design-arm delta: backend approve after 2 highs fixed; person's review (6 findings, 4 high) applied, backend delta approve after 2 more highs fixed; 11 findings carried into implementation.

## Context

The person (2026-10-12): Sonnet 5.5 is strong enough that "most (should even be all) of our sonnet run will succeed"; "we should use haiku or sonnet depending on the task, maybe we could start classifiying types of tasks", discovering the types from what we know and from the data. Asked which steps, they chose: classify, replay under Haiku, route by type.

The live ledger (2026-09-20 → 2026-10-12, ≈ 260 `relais run`s) supports the premise and shows where the cost is:

- One run in about 260 escalated past Sonnet. Runs that stopped short stopped on infrastructure (27 `blocked_preflight`, 10 `env_missing`, 8 `permission_denied`, wall-clock exhaustion under load, coordinator restarts, a reviewer ending without a verdict) or on review findings that a person or the orchestrator then finished (117 salvaged).
- Cost since 2026-09-25 (≈ $447): Sonnet initial attempts ≈ $322 (72%, ≈ $1.24 a run), reviews on Opus/Fable ≈ $90 (20%), Haiku ≈ $2 (22 inspections, $0.49).
- Haiku has never run a `change`. Three gates keep it out: `kind_floor` puts every change at the implementation tier (`crates/relais/src/route/mod.rs:369`); `eligible_tiers` (`:479`) is computed from that floor, and `covering_recipe_spec` keeps only recipes whose tier is eligible (`:534`); and `validate_candidate` refuses a recipe below the floor (`route/candidate.rs:496`, `TierBelowFloor`). So "can Haiku do this kind of change" is unmeasured, not known to be no.
- relais already has the pieces: deterministic `[[recipes]]` covering a task by kind and write scope (`route::Recipe`, `route/mod.rs:301`; `recipe_covers`, `:553`), where a candidate may only add a new revision of an existing recipe, same name, kind and `scope_within` (`candidate.rs:445-487`); `relais recipe evaluate/promote/rollback` (`main.rs:785`); `relais dataset replay`, which re-runs an accepted task under a candidate recipe and records a `trials` row, and is validated by the same door as promotion on purpose (`main.rs:5388`, `:5413`; `ledger/mod.rs:1289`); and the dataset's counterfactual gap (31 accepted tasks with no cheaper arm, `learn/dataset.rs:297`). What is missing is a task type: `TaskFeatures` has none (`learn/features.rs:49`), the `tasks` table has none (`ledger/mod.rs:1011`), routing never sees one, and recipes cannot match on one.

**Program design (person, 2026-10-12).** "before we implement, should we for some tasks add a program design agent like we have reviewers?" — the shape of the code (call-stack tree diffs, file-tree diffs, key types and signatures) decided before anyone writes it, "since haiku is a small model we might need this step". They chose two linked plans: the general step is a fleet plan of its own (decisions `work.program-design`, the design agent, a plan-template section, the implementation review checking against it); this plan carries a program design into the worker and measures whether it is what makes Haiku viable. Today's review findings were almost all at that level (a refname-sorted `for-each-ref` read as preference, a `bool` flag argument, a `Default` reading the clock, an untyped recipe outranking typed ones).

**Intended outcome:** every relais task carries a task type; replays measure Haiku's acceptance per type on our own past tasks; a type whose replays meet the bar routes to Haiku through a promoted recipe, the rest stay on Sonnet, a Haiku failure still climbs to Sonnet through the existing ladder, and a seeded Sonnet draw keeps the comparison alive.

## The taxonomy (v1)

Eight types, one per task, a closed enum `TaskType` in the library with a doc comment each, versioned as `TASK_TYPE_TAXONOMY_VERSION = 1`:

| type | what it is | examples from the ledger |
|---|---|---|
| `report` | read-only inspection | "report the repository's top-level layout" |
| `mechanical` | a change whose content is fully specified: bumps, renames, a new field through its literals, a dependency or package-manager migration | npm→pnpm migrations; `source: None` through literals |
| `pattern` | a new instance of an existing pattern the contract names | the feedback tool after the decide tool |
| `review-fix` | fix named review findings on a branch | "fix the three review findings against P3" |
| `feature` | a plan phase: new module, CLI surface, schema step | learning-loop Phase 2; revert detection |
| `refactor` | a cross-cutting type or structure change | the typed `Actor` through ~20 sites |
| `ui` | an interface or design-system component | Duro components, editor UI |
| `infra` | manifests, CI, deployment config | homelab Flux changes |

`unclassified` exists only for history the classifier could not place; a new contract never uses it.

## Phase 1: a task type on every task (Rust, plugin, skill)

- **Contract field.** `task_type` becomes an optional contract field (schema stays 1, additive, `#[serde(default, skip_serializing_if = "Option::is_none")]`), validated against the enum; an unknown value is refused naming the eight. A contract without one is routed exactly as today, and its contract hash is unchanged (pinned by a test).
- **Plugin and skill.** The plugin exports `TASK_TYPES = [...] as const` (the `ANSWERS` pattern, `claude-plugin/hooks/decide.ts`), builds the run tool's contract description from it, and a plugin test pins it to the Rust `TaskType` list (the cross-language pin pattern of `tests/guards.test.ts`). The relais skill says the orchestrator picks one when it writes the contract.
- **Stored per task.** One ledger step (the next free one) adds `trials.design_sha256` (null for the Haiku-alone arm; `--by task-type` splits by it), `tasks.task_type`, `tasks.task_type_source` (`declared` | `classified`) and `tasks.taxonomy_version`. A declared type is written at task creation; a classified one never overwrites a declared one.
- **Historical classification, relais staying model-free (SPEC §16).**
  - `relais dataset export-tasks --out <file|->` writes one JSONL row per recorded task: task id, repo, kind, objective, write scope, acceptance; no file contents. A file is written through `fsutil::write_atomic_with_mode` at 0600; `-` writes to stdout, where a closed pipe (`ErrorKind::BrokenPipe`) ends quietly with exit 0; the count summary goes to stderr, as `export-classifier` does (`main.rs:2625-2639`). Exit 0 written, 1 ledger or IO failure.
  - The orchestrator classifies the export once with a Haiku subagent (eight types plus `unclassified`, a one-line reason each), and a second Haiku pass over a 20% sample measures agreement.
  - `relais dataset import-task-types <file>` validates every row first, then writes all labels (`source = classified`) in one SQLite transaction. A file with any unknown task id or type is refused whole and writes nothing: each bad row is printed as `<file>:<line>: <reason>` naming the id or the eight types, exit 2. An IO or ledger failure exits 1. A re-import of the same file is a no-op. Counts by type go to stderr.
  - The agreement rate per type goes in this plan's record; a type below 80% is split or merged before Phase 2 selects from it.
- **Design input.** A contract may name `design = "<repo-relative path>"` (a markdown file of at most 16 KiB holding the three artifacts), `#[serde(default, skip_serializing_if = "Option::is_none")]` like `task_type`. relais reads it at preflight from `base_sha` (a path inside the write scope is refused), puts it verbatim in the worker's context package (`crates/relais/src/context/`), counts its bytes in `package_bytes` against `budget_bytes`, and records its sha-256 as `ContextManifest.design_sha256` (`#[serde(default)]`; the contract hash covers only the path); a missing or oversized file refuses the contract. The implementation review is handed the same design. Absent, nothing changes.
- **Report.** `relais report --by task-type` (a new `Dimension::TaskType`, `report/mod.rs:82`; its help text says it is not `task-class`, which is the verification profile) groups by type: tasks, accepted, salvaged (with containment), cost. Arms seen per tier (live and replay) appear in `--json` only, so the table fits 80 columns. The default report is unchanged.
- Tests: a contract with each type round-trips and an unknown one is refused; a typeless contract's hash is unchanged; a declared type survives an import; import of a file with one bad id exits 2, names `<file>:<line>`, and leaves the `task_type` count unchanged; a re-import is a no-op; export carries no file contents; `export-tasks --out - | head -1` exits 0 with empty stderr; a `--out` file is mode 0600; `--by task-type` on a fixture ledger fits 80 columns.

## Phase 2: replay accepted Sonnet tasks under Haiku (measure only)

- **The type reaches routing.** `route::route` stays pure: `RouteInputs` gains `task_type`, set by the caller from the contract's declared type, else the ledger's classified type. A replay reuses the source run's recorded contract (which carries no type) and keeps the receipt's `contract_hash` for comparison; the type comes in through `RouteInputs`.
- **Recipe match.** `Recipe` gains an optional `task_type = [..]`; `recipe_covers` requires the routed type to be listed. Without it a recipe covers as today. In the exhaustive `RecipeSpec` destructure (`candidate.rs:463`) `task_type` is a knob that may only narrow coverage: adding it is allowed, widening an existing list is refused.
- **Eligibility.** `RouteInputs` carries the admission. Only `replay_command` sets `Replay`; live routing admits a research-tier change only in PR 2, and only for a recipe promoted with trial evidence. `eligible_tiers` (`route/mod.rs:479`) admits the research tier for a `change` only when a covering recipe names the task's type and that admission allows it. A PR 1 test pins that a live route ignores a typed research recipe written into relais.toml by hand. Risk floors stay per task: candidate admission checks a typed research recipe against the kind floor only, never against `recipe_tier_floor`'s risk evaluation over its whole `scope_within` (`route/mod.rs:386`, which would refuse every `["**"]` recipe because of the implementation-tier `[[risk]]` rules); at routing, each task's own scope-derived risk floor (`risk_floor` over its write scope) still applies, so a typed task touching a protected path runs at the implementation tier under the same recipe. Tests: one recipe, a safe-path task routed to research and a protected-path task routed to implementation. Otherwise `covering_recipe_spec` (`:534`) filters the recipe out and the run would silently use Sonnet.
- **Admission.** `validate_candidate` gains an explicit `Admission::{Replay, Promote}` parameter (replay shares the promotion door on purpose today, `main.rs:5413`, so the difference is named, not loosened by accident). `Replay` accepts a research-tier change recipe that names task types; `Promote` refuses it in `recipe evaluate/promote` until Phase 3. A test pins the difference. A replay never changes what live runs do.
- **The candidate.** One recipe name per type and arm: PR 1 seeds `change-<type>` rev 1 in relais.toml (kind `change`, `change-routing`'s `scope_within`, `task_type = [<type>]`, `tier = "implementation"`), so routing is unchanged. The candidate is rev 2 of that name with `tier = "research"` (`validate_new_recipe` refuses unknown names and diffs against the highest revision of the same name, `candidate.rs:445-452`). This gives each type its own revision chain: `relais recipe rollback change-<type>` copies that type's previous revision (`learn/promote.rs:217`) and leaves every other type alone; per-type revisions of the single `change-routing` could not be rolled back one type at a time (`policy/recipe.rs:296-313`). A test with two typed recipes pins that rolling back one returns only that type to implementation. `select_highest_enabled_revision` compares revisions across names with config order breaking ties (`policy/recipe.rs:305-307`), so `covering_recipe_spec` first prefers a recipe that names the task's type over one that does not, and among typed ones a `requires_design` recipe when the contract has a design; a test pins that `change-routing` at rev 5 does not displace `change-<type>` rev 2. For `feature`, `refactor` and `pattern`, PR 1 also seeds `change-<type>-designed` rev 1 (same fields plus `requires_design = true`, at the implementation tier, so routing is unchanged) and ships `requires_design` with `RouteInputs.has_design` (a narrowing knob in the `RecipeSpec` destructure): the Haiku + design arm measures `change-<type>-designed` rev 2, the exact recipe a later promotion would ship, so its recipe id (a hash of the recipe content, `policy/recipe.rs:123`) never changes between measurement and promotion. Promotion provenance (which trials authorised it) is kept in the ledger, outside the recipe content.
- **Each replay checks itself.** Its first attempt's dispatched model must be the research model (Haiku); a replay whose first attempt ran anything else is recorded as invalid, never as a Haiku result, while a Haiku attempt that repaired and escalated to Sonnet is a Haiku failure, not invalid. `relais dataset replay --dry-run` names the research recipe for a typed change before anything is spent.
- **Arm identity.** An arm is its candidate recipe id (`change-<type>` rev 2 or `change-<type>-designed` rev 2), nothing more: each task's design differs, so the design sha-256 is checked per trial (non-null on every `-designed` trial, null on every Haiku-alone one, a mismatch recorded invalid) and never keys the arm; the replay comparison (`learn/comparison.rs:561`, which selects trials by recipe id and keeps one result per task) selects and de-duplicates per task within each arm, never across arms. A test runs the two arms in both orders and pins that neither arm's score changes.
- **Three arms.** For `feature`, `refactor` and `pattern`, each selected task is replayed twice: Haiku alone, and Haiku with a program design (`relais dataset replay --design <file>`, the design added to the context package only and `RouteInputs.has_design` set from the flag, since the recorded contract names none, so the recorded contract and its `contract_hash` stay the source run's; the trial records the design's sha-256). The design is drafted from the task's contract and the plan file as it was at the source run's `base_sha`, never from the landed diff, a later plan revision or the candidate: the drafting agent works in a fresh repository built the way replay builds its checkout (`main.rs:5589`, an object store that excludes the accepted answer, so no later commit is reachable through git history), with a fresh context holding only the historical contract, the plan file at `base_sha` and the base tree, by the fleet plan's design agent, or, until that lands, by a Sonnet subagent producing the same three artifacts; the drafting cost is recorded with the arm. The source run is the Sonnet arm. Other types get the Haiku-alone arm only.
- **Selection and pilot.** First a pilot of one Haiku-alone and one Haiku + design replay (with its drafting) measures cost and wall time per arm, and its cost, drafting included, counts against the screening cap; the plan records it and re-sizes both caps. Then per type (except `report`, already Haiku): 5 of the most recent accepted Sonnet tasks whose repository, base sha and receipt are still reachable and whose write scope fires no implementation-tier `[[risk]]` rule (`risk_floor`, `route/mod.rs:496`; such a task would route to implementation and be recorded invalid) (for `feature`, `refactor` and `pattern`, also a plan file present at that base sha; a task without one is replayed Haiku-alone only and counted); five replays per arm only screen: an arm with at most one reasoning failure in its first 5 continues to 15, otherwise it stops; fifteen decides promotion. An arm whose type has fewer than 15 reachable tasks, or whose extension is not funded, keeps its type on Sonnet.
- **Bounds.** Replays are admitted by the coordinator's heavy slots (two at a time), not by load average (load here runs 100–300, so a load gate might never open), within a 7-day window; what finished by then is the evidence. Two caps, both counting drafting: the screening batch (5 per arm) at about $35 (at about $1.24 a Sonnet draft and about $0.3 a Haiku replay, to be replaced by the pilot's measure: 3 design types × 5 tasks × (2 replays + 1 draft) ≈ $28 plus 20 Haiku-only replays ≈ $6), and the extension (10 more per continuing arm), sized from the pilot and the screening results and funded separately; both are recorded here. A replay that stops on infrastructure is retried once, then counted unknown, never as a Haiku failure.
- **What is recorded.** Each replay logs one start and one end line (type, task id, outcome, dispatched model, cost, wall time, load at start), and the plan's record is copied from those lines. Per type: replays, accepted without escalation, failed on reasoning, unknown, invalid, cost, duration, in `relais report --by task-type` (trials joined to the type) and in this plan's record.

## Phase 3: route by type (Rust, relais.toml)

- **Design-gated routes.** A type whose Haiku + design arm meets the bar and whose Haiku-alone arm does not routes to Haiku only for contracts that name a `design`: the promoted recipe is `change-<type>-designed` (shipped and measured in PR 1), and a contract without a design stays on Sonnet. The design's drafting cost counts in the type's cost per accepted task.
- **The bar.** Evaluated only at n = 15 per arm: an arm passes at 14/15 or better (Wilson 95% lower bound 0.7017) on acceptance without escalation.
- **The route.** A promoted revision of `change-<type>` per qualifying type, `{task_type = [<type>], tier = "research"}`, written through `relais recipe promote --write`. A mandatory typed-research gate sits in the promotion path itself, so the generic gates (`learn/comparison.rs:350`, sample support and comparability, on terminal acceptance) cannot authorise it: for a research-tier change recipe it recomputes, from qualifying trials only, the Wilson 95% lower bound of acceptance without escalation and requires 14/15 or better. A qualifying trial ran that exact candidate recipe id, on a task of that exact type, in that exact arm (design or not), with the research model dispatched, and ended accepted without escalation, failed on reasoning, or escalated (the last two are failures in the denominator; infrastructure stops are unknown and do not count). Tests pin that failed, escalated, mismatched-model, other-type, other-arm and other-recipe trials cannot authorise promotion, through `recipe promote` or any generic promotion path. `Admission::Promote` and `validate_candidate` accept a research-tier change only for a recipe that names task types and passes that gate; `kind_floor` itself is unchanged for every other route.
- **Safety net, already there.** A Haiku attempt that fails repairs once, then escalates to the implementation tier on the existing ladder (`relais.toml` `max_repairs_before_escalation = 1`), so the worst case costs one Haiku attempt, one Haiku repair, then the Sonnet run.
- **A Sonnet arm keeps accruing.** The session router's exploration only sets the session model and never draws above research (`claude-plugin/hooks/router.ts:375-379,678`), and `route::route` has none. So `route::route` gains a seeded draw (`rng::exploration_draw("task-type", task, run)`): for a research-tier change routed by a task-type recipe, 10% of runs take the implementation tier (Sonnet), with draw and propensity logged as the session router logs them. The draw applies only to live routing, never to a replay, so a replay always runs the research tier. Tests pin that a drawn live run routes to implementation, that the draw is reproducible, and that a replay is never drawn.
- **Rollback.** A Haiku type is rolled back with `relais recipe rollback` and said once when, over 10 or more live runs, the Wilson 95% lower bound of its acceptance without escalation falls below 0.50, or its cost per accepted task (Haiku plus any Sonnet escalation) is at or above that type's Sonnet median from the drawn arm.
- **ADR-0006** records: a change may run below the implementation tier only through a recipe that names its task types and was promoted on replay evidence, with a seeded Sonnet draw kept on it; the floor stays for everything else.
- The relais repo's relais.toml is changed first; other repos get the recipe by their own PR once their types have replay evidence.

## Delivery

- **relais PR 1:** Phase 1 and Phase 2's code (contract fields `task_type` and `design`, `requires_design` and `RouteInputs.has_design`, the `-designed` templates, arm identity in the comparison, per-task risk floors, plugin list, ledger step, export/import, `--by task-type`, `RouteInputs.task_type`, the narrowing `task_type` recipe knob, eligibility under `Admission::Replay`, the replay self-check). `make check`, implementation review, merge on green, release through `tag-release` (the plugin's run tool needs the reinstall).
- **Live:** the historical classification, the pilot, then the replays.
- **relais PR 2:** Phase 3 (the typed-research promotion gate, `Admission::Promote`, the seeded Sonnet draw, rollback rule, ADR-0006) plus the first promoted recipes in this repo's relais.toml, once replay evidence exists.

## Verification

- `make check` in the worktree exits 0 (fmt, clippy `-D warnings`, module cycles, cargo test, MSRV, audit, plugin).
- Contract: `{"task_type": "mechanical", …}` accepted and stored `declared`; `"task_type": "chore"` refused naming the eight; a typeless contract's hash unchanged.
- Export on a `VACUUM INTO` copy of the live ledger: one row per task, no file content (byte check for a known file's text), `| head -1` exits 0; import of the classifier's output: counts by type on stderr, a declared type untouched; a file with one bad id exits 2 naming the line and writes nothing.
- Classifier: second-pass agreement per type on the 20% sample printed; every type below 80% merged or split before Phase 2 selection; the count left `unclassified` recorded.
- Replay gate: `relais dataset replay --dry-run` on a typed change names the research recipe; the same candidate is refused by `recipe evaluate`/`promote` in PR 1 (`Admission::Promote`); a research-tier change recipe without `task_type` is refused everywhere; a replay whose receipt shows a non-Haiku model is recorded invalid.
- Design input: a contract naming a 10 KiB design puts it verbatim in the worker's context package, counts its bytes in `package_bytes`, and records its sha-256 in `ContextManifest.design_sha256`; a design path inside the write scope is refused; the design is read from `base_sha` even if the working tree differs; `trials.design_sha256` is non-null for a design arm and null for Haiku-alone; a `requires_design` recipe covers only a contract with a design (PR 1, at the implementation tier; after promotion in PR 2, `relais plan` routes a designed contract to Haiku and an undesigned one to Sonnet); a 20 KiB or missing file refuses the contract; `dataset replay --design` leaves the trial's `contract_hash` equal to the source run's and records the design hash.
- Pilot and live replays: per type and arm, replays, accepted, failed, unknown, invalid, cost and duration recorded here; spend within the recorded caps.
- PR 2: a promoted Haiku recipe routes a matching contract to Haiku (`relais plan` shows the route and its reason); a seeded draw routes about 10% to implementation, reproducibly; a failing fixture attempt escalates to implementation; a rollback restores the previous revision, and with two promoted types, rolling back `change-<a>` leaves type b routed to Haiku (`relais plan`).
- Ledger work only against a scratch `RELAIS_STATE_DIR` until release.

## Measure (after release)

- Share of new contracts with a declared type (target 100% within a week; below 80% means the skill line is not holding).
- Per routed type: live Haiku acceptance without escalation, escalations to Sonnet, cost and wall time per accepted task against the drawn Sonnet arm.
- Classifier agreement on the 20% sample, and how many tasks stayed `unclassified`.

## Not in scope

- Moving reviews off Opus/Fable (the 20%): a separate measurement of reviewer findings.
- The session router's own kind/difficulty classifier and Laya: unchanged.
- Other repositories' relais.toml: their own PRs, after evidence.

## Decision log

- **D1** (person, 2026-10-12) Steps 1–3: classify, replay under Haiku, route by type.
- **D2** The type is declared by the orchestrator in the contract and classified once for history; relais never infers it from prose at routing time ("Recipes are never inferred from prose", `route/mod.rs:296`) and links no model (SPEC §16).
- **D3** The implementation floor for `change` relaxes only for a recipe that names task types: under `Admission::Replay` to measure, under `Admission::Promote` only with replay evidence; a replay never routes a live run.
- **D5** (person, 2026-10-12) Program design is planned twice, linked: the general step in a fleet plan, and here the `design` contract input, the Haiku + design replay arm and design-gated routes, so its effect on Haiku is measured rather than assumed.
- **D4** Rollback takes both the backend and product owner rules: a low acceptance bound, or a cost per accepted task no better than Sonnet's, since cost is why the plan exists.
- **D6** (2026-10-13) The seeded `change-<type>` recipes use a field the installed relais (0.17.0) refuses, so from this branch on, relais cannot run against the relais repository until PR 1 is released and installed. The replay half was therefore implemented in the session; the release follows the merge, and the person re-grants trust for the changed `relais.toml`.
- **D7** (2026-10-13) A replay candidate appends its new revision at the end of `[[recipes]]`: a candidate that inserts it beside rev 1 is refused ("a candidate may only append new revisions").

## Record (PR 1)

Phase 1 and Phase 2's code are done (commits a4f9a3e, 63e894e, 6667125, 26ba68a, 00bdb5e). Observed before the push, with the branch binary on a scratch `RELAIS_STATE_DIR` holding a `VACUUM INTO` copy of the live ledger:

| check | expected | actual |
|---|---|---|
| `make check` (amont pre-commit, every commit) | exit 0 | 30/30 checks passed; lib 1,489 tests, plugin 281/281 (plugin-test skipped on a4f9a3e after a standalone 281/281 pass, load 60–170) |
| `plan` on `"task_type": "chore"` | refused naming the eight | exit 2, `unknown task_type \`chore\`: it must be one of report, mechanical, pattern, review-fix, feature, refactor, ui, infra` |
| `plan` route of a typed contract | `change-<type>`, implementation | not shown: blocked on a missing trust grant (the person's step); covered by unit tests |
| `export-tasks --out <file>` | one row per task, 0600, no file contents | 233 rows for 233 tasks, mode 600, keys exactly acceptance/kind/objective/repo/task_id/write_scope |
| `export-tasks --out - \| head -1` | exit 0, empty stderr | exit 0, 0 stderr bytes |
| import, one unknown id and one unknown type | exit 2, `<file>:<line>`, nothing written | exit 2, lines 3 and 4 named, typed count 0 → 0 |
| import of 5 labels, then again | labelled once, re-import no-op | 5 labelled (report 1, feature 4); then 0 labelled, 5 unchanged; source `classified`, taxonomy 1 |
| `dataset replay --dry-run`, task-f1bc7e1662b5eaf3 (`pattern`), candidate `change-pattern` rev 2 research | names the research arm | `arm: change-pattern revision 2, tier research, model haiku`, `design: none`, exit 0 |
| `recipe evaluate` on that candidate | refused (`Admission::Promote`) | exit 3, "puts a `change` at the research tier; only a replay admits that today" |

Next: release, then the live phase (historical classification, pilot, replays), then PR 2.

## Implementation review

- **approve** after one Delta (round 1 approve-with-changes, 112k tokens, 143 s; Delta 119k, 17 s).
- Fixed: unreadable contracts counted on export; a null taxonomy version is corrupt; the no-file-contents test can now fail; the `live_of` holds-until names its limit.
- Carried to Measure: new contracts carry a declared type (target 100% in a week), which the holds-until relies on.

<!-- panel: repos=relais reviewers=architect,backend,lang:rust,lang:typescript,po,tui,unix body-sha=25845c3bb661 -->
