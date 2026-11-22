---
status: done
plan-id: p-01M7ZD5ZMFEF3XH7HYATDANGVA
branch: feat/phase8-entry
repos: [relais]
adrs: []
---

# relais learning loop: Phase 8 entry

## Review panel

👉 **Decide:** none — approve if the bet is "a question answered and never corrected before the next task is evidence enough to learn cheaper routing for questions".
📍 relais · 4 phases · 1 PR · next: Phase 1 in a worktree. Panel: backend, lang:rust, tui, unix (opus-5-5); codex (gpt-6-astra).
**Changed by review:** no v49 ledger column (0 stop rows, so 0.75.0 rollback stays possible); the cost figure and full model ids get their own report lines; the savings gate is inconclusive with any unpriced session (Codex).
**Verdicts:** round 1: 4 approve-with-changes; round 2: 1 approve, 3 approve-with-changes; final backend approve-with-changes, no blocker; codex approve-with-changes; 5 carried; words 2071 → 2454 (×1.18); remove 1, merge 0.
📄 Full reviews: [2026-11-21-learning-loop-phase-8-entry.reviews.md](2026-11-21-learning-loop-phase-8-entry.reviews.md)

## Picture

```
 today                               after this plan
 -----                               ---------------
 question --answered--> (dropped)    question --answered, own--> strata (+)
                                     question --answered, v45--> (dropped)

 <=4 unpriced sessions --> arm cost  <=4 unpriced sessions --> "excluded N"
   unknown for all 148 sessions      >=144 priced sessions --> cost per task
```

An answered question used to fall out of the exploration evidence and an unpriced session used to blank its arm's cost; after this plan a question's own `answered` counts as a positive, and only the unpriced sessions (up to 4 of 148) are left out of the headline.

## Context

The 2026-11-21 handoff on Phase 8 of `docs/plans/2026-10-10-learning-loop.md` asked a planner to choose between a Haiku-cloning Laya, a Sonnet/Opus teacher pass, and the plan's data gate. Reading the gate code and the ledger corrected the handoff on four points:

1. **The gate is met.** `relais report --since 2026-11-01 --no-import` renders the session-routing section (at the end of a 2,500-line report; a `head`-truncated pipe hid it). Two subagent strata have reached 20 drawn tasks with strong outcomes (`sub review d3 module` 30 drawn, `sub review d4-5 cross-cutting` 24), a third is projected for 11-23. Subagent draws are legitimate since D14 (2026-10-11) reversed D6. The ceiling clause (bands only at day 14 if > 90 days out) does not fire. Phase 8 starts on the plan's own terms.
2. **Main-session question strata cannot accumulate evidence.** `is_strong` excludes `answered`; questions are 60% of main-session tasks and 71% of them end `answered` or `unknown`. The busiest main stratum (`main question d1-2 unknown`) is at 5 of 20, projected 2027-03-09. Main-session routing is where the money is: since 11-01, 65% of main `new_task` decisions went to Opus.
3. **The cost headline is blank.** Two sessions used `gpt-6.1-sol` and two used `qwen3.6:35b-a3b`; one unpriced model voids the whole arm's cost, so all three arms read "cost per completed task unknown" over 148 sessions. Phase 8's savings gate (session-router plan §5 gate 3) needs that figure per stratum.
4. **Laya replacing Haiku is not a cost project.** The classifier cost $5.51 for 3,885 calls since 11-01 (about $8/month). Laya's ceiling against single-draw Haiku labels is about 89% (D33), under the 95% bar; on CPU it answers in 28 s against a 3 s timeout, on GPU it competes with chat. A teacher pass at Sonnet/Opus prices ($0.004–0.008 a label) cannot net a saving against that bill.

This plan does three small things in relais so Phase 8 can be charted on true numbers: makes `answered` questions count, makes the cost headline honest, and records the decisions. It does NOT build Phase 8: learner, four gates, `router evaluate`/`rollback`, `router_activations`, served `adjustments`, plugin application are all absent from source (`router/mod.rs:12-13` "R1 has no learner"; `RouterCommand` at `cli/mod.rs:702-720` has only `ExportClassifier`). Phase 8 is charted as a programme at the end.

## Decisions (person, 2026-11-21, "let's follow your suggestions")

- **D34 (label policy)** For `class_kind = question`, an `answered` outcome with `outcome_source = own` counts as a strong positive for the exploration strata, stop rule and Phase 8 learner. A question's natural terminal state carries no verification; `corrected` already captures the negative; "answered and not corrected before the next task started" is the only success signal a question gives. Caveats recorded: the label is final once the next task starts, so a correction noticed later never reaches it; the missed-failure check (file overlap) cannot match an `answered` task; backfilled `answered` rows (v45) are excluded because they were inferred, not observed. `ROUTER_LABEL_POLICY_VERSION` 2; `is_completed` and `rank` unchanged; `LABEL_POLICY_VERSION` (run-level learner) untouched. Amends session-router plan §5 gate 1's strong set for the question kind.
- **D35 (Phase 7 closed)** No run 3. Laya replacing Haiku is dropped as a cost project (D17 superseded): bill ≈ $8/month, ceiling ≈ 89% < 95% bar, no serving path. Trained models in `s3://laya-models` stay until the person deletes them. Laya's "second job" (Phase 8 research comparison on exported outcomes) remains; SPEC §16 unchanged.
- **D36 (Phase 8 gate met; charted as a programme)** On 2026-11-21 the busiest strata are subagent review strata with ≥ 20 drawn strong outcomes; the ceiling clause does not fire; Phase 8 starts with full strata keys, review-subagent strata first. Phase 8 is a multi-session build, charted as a relais programme, not a phase of this plan. The handoff's main-only reading (18 drawn in "question/unknown") counted kind × scope without the band and ignored subagent strata.
- **Watch item** `main question d1-2 unknown`: 2 of 5 drawn strong outcomes corrected (40%) against 7% undrawn. Small n; the pooled stop rule has not fired (0 rows in `router_exploration_stops`). Re-read a week after D34 lands.
- **Not changed** Exploration ε stays 0.1 (`[session_routing] epsilon` and `envelope.epsilon_max` in machine.toml are person-configurable; no code change). Revisit only if main strata still lag after D34.

## Phases

relais, one PR, one commit per phase. Verification per `work.verification-follows-risk`: one real-input check per observable behaviour, run once on the final tree (the two checks under Verification).

### Phase 1: `question` + `answered` + `outcome_source = own` is a positive strong outcome, versioned

- `crates/relais/src/ledger/router.rs`: `ExplorationTask` (`:208-221`) gains `outcome_source: Option<String>`; `router_exploration_tasks` (`:853-891`) selects `t.outcome_source` as column 10 in both SELECT arms. `outcome_source` is never NULL (v23 backfilled `own`, upsert writes `unwrap_or("own")` at `:671`), so `Some("own")` is an exact match.
- No ledger migration (`crates/relais/src/ledger/schema/migrations.rs` and `schema/mod.rs` stay at v48): no stop row exists (0 in `router_exploration_stops`), so a stop's label policy is not stored; a later policy v3 adds the column with `DEFAULT 2`, and the installed binary keeps opening the ledger.
- `crates/relais/src/router/explore.rs`: new `pub const ROUTER_LABEL_POLICY_VERSION: u32 = 2` after `STOP_MARGIN` (`:21`), doc comment in the `learn/dataset.rs:55-76` style (version 1 = every outcome but unknown/answered; version 2 = also a question's own `answered`, as a positive). `strong` (`:32-36`) takes `&ExplorationTask`: `is_strong()` first, else `Answered && kind == "question" && outcome_source.as_deref() == Some("own")`. `strata` (`:95`) calls `strong(task)`; `bad = Corrected` (`:101`) unchanged.
- `crates/relais/src/router/outcome.rs`: doc comments at `:18-19` and `:64-65` say the kind-aware rule lives in `router::explore`. `rank`, `is_completed`, tests unchanged.
- `crates/relais/src/router/report.rs`: `ExplorationReport` (`:125-132`) gains `label_policy_version: u32`, set in `exploration()` (`:153-157`), `Default` derive kept. `render` (`:167-195`) header gains a third line `   an answered question counts as a positive unless backfilled; policy v2):` (second line ends with `;`). `SessionRoutingReport.answered` doc (`:82-83`) and the rendered string at `:510-513` become `  answered (no file edited; questions count unless backfilled): N` (test at `:743-746` follows). `parent_buckets` (`:423-428`) stays: a diagnosis bucket, one comment line.
- `crates/relais/src/report/mod.rs:82`: `REPORT_SCHEMA_VERSION` 20, doc line "20: `session_routing.exploration.label_policy_version`, `session_routing.arms[].excluded_sessions`, `session_routing.arms[].priced_completed`". `runner/tests/holdouts.rs:1781-1782` and `tests/it/holdout_report.rs:139` → 20 (pre-commit runs the full `cargo test`).
- Tests: `explore.rs` fixture `task()` (`:246`) gains `outcome_source: Some("own")`; new `answered(drawn, kind, source)` fixture; rename `an_answered_outcome_is_no_evidence_for_the_stop` (`:303`) to `…_edit_…`; add `an_answered_question_from_its_own_record_is_a_positive` (19 drawn + 1 drawn answered question → stop fires at 20 drawn, 4 bad; an undrawn one reads `undrawn 1, undrawn_bad 0`), `a_backfilled_answered_question_is_no_evidence`, `an_unknown_question_is_no_evidence`, `the_router_label_policy_is_version_two`. `report.rs`: `spawned` fixture (`:887-896`) gains the source; new `the_exploration_section_names_its_label_policy_and_the_stops` (header third line, JSON field, every line ≤ 80); `:955` already measures the header. `ledger/router.rs` tests (first coverage of these methods): `exploration_tasks_carry_their_outcome_source`.
- Docs: `docs/router-protocol.md:79` (strong set) and `:147` (`answered` definition); `docs/SPEC.md:1185` ("neither completed nor a learning label" → not completed; a question's own `answered` is a positive strong outcome, label policy version 2; every other `answered` is no label); `CHANGELOG.md` Unreleased bullet naming `ROUTER_LABEL_POLICY_VERSION` 2, report schema 20, `is_completed`/rank unchanged.

### Phase 2: unpriced sessions are excluded per arm, not the whole arm

- `crates/relais/src/router/report.rs`: `Arm` (`:28-43`) gains `excluded_sessions: usize` (doc: left out of `cost`, the ratio's denominator and the bootstrap clusters; `sessions`/`tasks`/`completed` still count them); `unpriced_models` kept as the union over excluded sessions. The arm closure (`:291-336`) computes `total()` per session once, splits priced from unpriced, sums priced cost, uses the priced sessions' completed count as the ratio denominator (exposed as `Arm.priced_completed: usize`), feeds only priced sessions to `cluster_bootstrap_ratio` (`router/stats.rs:91`); `cost` is `None` only when the arm has sessions and none prices (an empty arm keeps `Some(ZERO)`). `completed` (`:287-290`) stays the full count so it reconciles with the `outcomes:` line. The arm's `cost per completed task …` figure moves to its own line under the arm line, indented four spaces, because the fixed text in front of it already fills 80 columns; `every_session_routing_line_fits_eighty_columns_with_four_digit_counts` drops its filter (`:762`) and checks every line. Render (`:480-502`): the `(None, Some(_))` figure reads `none (no completed task among priced sessions)` when `excluded_sessions > 0` (tested); when `excluded_sessions > 0` and the arm has a priced cost, a line `    excluded N session(s) from cost, no [pricing.models] entry for:` and then one line `      <model id>` per unpriced model; an arm no session of which prices reads `cost per completed task unknown, no [pricing.models] entry for:` followed by the same one-id-per-line list and no `excluded` line; ids are printed in full, never cut, so they can be pasted into machine.toml (implementation review #1, code review). Module doc `:4-6` gains the rule.
- Tests in `crates/relais/src/router/report.rs`: extend `a_priced_arm_has_a_cost_per_completed_task_and_an_unpriced_one_is_unknown` (`:660`) with `excluded_sessions == 1` and, the arm being fully unpriced, the id list without an `excluded` line; new `an_unpriced_session_is_left_out_of_its_arms_cost_and_denominator` (routed arm with one priced and one unpriced session: `sessions 2, tasks 3, completed 2, excluded_sessions 1, cost Some(2_000_000), cost_per_completed_task Some(2_000_000)`, `interval == Some([2_000_000, 2_000_000])` (one priced cluster)); `every_session_routing_line_fits_eighty_columns_with_four_digit_counts` (`:751`) covers the new lines with `excluded_sessions = 9999` and two model ids, and asserts that every id appears in full.
- Docs: `docs/SPEC.md:1191` Measurement paragraph (exclusion rule, fully unpriced arm reads unknown); `docs/plans/2026-10-07-session-router.md` §5 gate 3: one sentence distinguishing descriptive reporting from activation: the headline excludes unpriced sessions, but the savings gate is inconclusive whenever either compared arm contains an unpriced session (a session-count threshold cannot bound omitted cost: gpt-6.1-sol's 989 usage rows sit in 2 sessions); pricing the model in machine.toml is what makes the arm readable again; `CHANGELOG.md` bullet naming the blank headline this fixes.

### Phase 3: plan records

- `docs/plans/2026-10-10-learning-loop.md`: append D34, D35, D36 and the watch item after D33 (`:168`, format `- **D<n>** (date, who) text`); adjust the stop-rule and report wording at `:80-81` and the Phase 8 gate text at `:131` for the question-kind rule; mark Phase 7 closed in its section.
- `docs/plans/2026-10-07-session-router.md:198`: gate 1's strong set gains "(and, D34 of the learning-loop plan, a question's own `answered`)".
- `AGENTS.md`: run `amont agents-md` in the worktree; stage it if the generated block changes.

### Phase 4: chart the Phase 8 programme (after the PR merges and releases)

Invoke the `relais:programme` skill in the relais checkout to chart "Phase 8: the learner that chooses the model" under `docs/programmes/` with decision tickets: per-stratum `router evaluate [--json]` (read-only first slice over the two ready strata; reads `explore::strata`, `stats::cluster_bootstrap_ratio`, `stats::wilson`), the four activation gates, `router rollback`, `router_activations` (schema 48 → 49), `router-state` `adjustments` (`state.rs:125,309` stub), plugin application (the plugin router hook's state parser and exploration draw), `docs/router-protocol.md`, the missed-failure measure, Laya's second job as a comparison, and the relais ADR (next free number under `.adr/records/`; 0015 is referenced at learning-loop `:36`, check before taking it).

## Non-goals

- Building Phase 8 (learner, gates, `router evaluate`/`rollback`, `router_activations`, served `adjustments`, plugin application): charted as a programme in Phase 4, built under it.
- Changing `is_completed`, `rank`, the plugin's `outcomeOf`, or `LABEL_POLICY_VERSION` (run-level learner).
- Raising exploration ε, or any Laya training run.
- Pricing `gpt-6.1-sol` or `qwen3.6:35b-a3b` in the binary (prices live in machine.toml).

## Code review

- relais: backend, lang:rust, tui (unix left out: no flag, exit code or path changes; architect left out: no new module or contract, Phase 4 charts the Phase 8 build separately)

## Verification

```
cargo fmt --all -- --check
cargo clippy -p relais --all-targets -- -D warnings
cargo test -p relais --lib router::
cargo test -p relais --lib ledger::
cargo test -p relais --lib report::
cargo test -p relais --lib runner::tests::holdouts
cargo test -p relais --test it holdout_report
cargo build -p relais
```

Manual check on a SCRATCH copy only, so the live ledger is never opened by the branch binary (the report opens the ledger read-write; a future ledger step would migrate it):

```
set -eo pipefail   # stop at the first failing step; a failing relais must fail its pipeline, not hide behind awk/jq
scratch=$(mktemp -d)
sqlite3 ~/.local/state/relais/ledger.sqlite ".backup '$scratch/ledger.sqlite'"
sqlite3 -readonly -header "$scratch/ledger.sqlite" "WITH eligible AS (SELECT CASE WHEN t.difficulty<=2 THEN 'd1-2' WHEN t.difficulty=3 THEN 'd3' ELSE 'd4-5' END AS band, t.scope, t.outcome IN ('completed_verified','completed_accepted','corrected') AS old_strong, d.explored AND d.applied AS drawn, NOT d.explored AND d.propensity IS NOT NULL AS undrawn FROM router_tasks t JOIN router_decisions d ON d.session=t.session AND d.task_id=t.task_id AND d.agent_id='' AND d.relation='new_task' WHERE t.agent_id='' AND t.class_kind='question' AND t.scope IS NOT NULL AND t.difficulty IS NOT NULL AND NOT d.holdout AND (t.outcome IN ('completed_verified','completed_accepted','corrected') OR (t.outcome='answered' AND t.outcome_source='own'))) SELECT band, scope, SUM(drawn AND old_strong) AS v1_drawn, SUM(drawn) AS v2_drawn, SUM(undrawn AND old_strong) AS v1_undrawn, SUM(undrawn) AS v2_undrawn FROM eligible WHERE drawn OR undrawn GROUP BY band, scope"   # whole-ledger expected counts by stratum and policy; its drawn/undrawn flags must match router_exploration_tasks (ledger/router.rs:861-887)
command -v relais   # the installed 0.75.0: baseline on the same copy, before the branch binary
RELAIS_STATE_DIR="$scratch" relais report --since 2026-11-01 --no-import \
     | awk '/^session routing \(/{p=1} /^learning:/{p=0} p'
RELAIS_STATE_DIR="$scratch" ./target/debug/relais report --since 2026-11-01 --no-import \
     | awk '/^session routing \(/{p=1} /^learning:/{p=0} p'
RELAIS_STATE_DIR="$scratch" ./target/debug/relais report --since 2026-11-01 --no-import --json \
  | jq '.schema_version, .session_routing.exploration.label_policy_version, [.session_routing.arms[].excluded_sessions]'
rm -rf "$scratch"
```

Expected: arm lines carry a priced `cost per completed task $…` followed by `    excluded N session(s) from cost, no [pricing.models] entry for:` and one full-id line per model where it applies; the exploration header's third line reads `   an answered question counts as a positive unless backfilled; policy v2):`; baseline first: compare every displayed `main question …` row from installed 0.75.0 against the SQL oracle's v1 counts, and every displayed branch row against its v2 counts, keyed by band and scope. Exploration uses the whole ledger despite `--since`; both text and JSON retain only five strata, so do not compare displayed sums or require identical membership. Confirm backfilled answers contribute nothing (the 4 `backfill_v45` rows); the review strata remain at 30 and 24 drawn wherever displayed; JSON gives `20`, `2`, and three `excluded_sessions` summing to the distinct sessions with gpt-6.1-sol or qwen3.6:35b-a3b usage on the copy (≤ 4). The cargo block is the gate, not a check. `--no-import` keeps the report from importing transcripts; the live ledger is never opened by the branch binary.

Release: `tag-release` skill after merge (report schema 20 is user-visible; no ledger step, so rollback is reinstalling the previous release, 0.76.0 after the rebase; neither 0.75.0 nor 0.76.0 adds a ledger step); plugin reinstall is the person's step as before. Then Phase 4.

### Results (2026-11-21, tree 1813525b)

Gate: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test -p relais` green on each phase's tree (full suite 350 tests, 936 s on the busy host); the pre-commit hook deferred clippy and the tests to CI twice because no host slot freed, so they were run by hand each time. Manual check on a `.backup` copy of the live ledger (integrity `ok`), baseline with installed 0.75.0 first, then `target/debug/relais` at 1813525b, both with `RELAIS_STATE_DIR` on the copy; the branch binary never opened the live ledger. The scratch copy's `-readonly` open fails until SQLite has written its `-shm`; the oracle ran without the flag.

| input | expected | actual |
|---|---|---|
| SQL oracle, `main question d1-2 unknown` | v1 = baseline row; v2 = branch row | v1 drawn 5, undrawn 56 = baseline `drawn 5 (40%), undrawn 56 (7%)`; v2 drawn 12, undrawn 94 = branch `drawn 12 (17%), undrawn 94 (4%)`, projection 2027-03-10 → 2026-12-15 |
| oracle, all question strata | drawn delta = 11 own `answered` explored questions; backfilled rows add nothing | drawn 5→12, 1→2, 0→2, 0→1 across four strata = +11; 4 `backfill_v45` rows absent from both policies |
| `sub review d3 module`, `sub review d4-5 cross-cutting` | 30 / 24 unchanged | 30 (10%) / 24 (4%), `20 drawn reached` |
| exploration header | third line `   an answered question counts as a positive unless backfilled; policy v2):`, second ends `;` | verbatim |
| arm lines | own cost line; `excluded N session(s) …` + full ids; excluded sums to the unpriced sessions (≤ 4) | routed `$19.841499 (95% interval $14.560013 to $28.461619)`, excluded 1 (`gpt-6.1-sol`); held out `$21.387365`, excluded 1; shadow `none (no completed task among priced sessions)`, excluded 2 (`qwen3.6:35b-a3b`); 1+1+2 = 4 = distinct unpriced sessions on the copy |
| section width | 0 lines over 80 columns | 0 |
| `--json` | `schema_version` 20, `label_policy_version` 2, three `excluded_sessions`, `priced_completed` per arm | 20, 2, [1, 1, 2]; priced_completed 955 / 114 / 0 against completed 1012 / 119 / 0 |

Relais: Phase 1's worker (sonnet) produced a complete candidate but its focused test run returned no result in its budget (`blocked env_missing`); a `kind: verify` run on it answered `candidate_missing` although `candidate-final.patch` was on disk (filed in the inbox), so the patch was applied by `git apply` and checked by hand. Phase 2's first run exhausted the 1800 s admission wait with nothing launched (host slots held by other worktrees' test runs); the second produced a kept candidate whose verification lease also exhausted; the candidate was applied by hand and checked. One pre-existing integration test (`tests/it/session_router.rs`) asserted the single-line arm format and was updated.

## Implementation review

- **approve** (implementation-review, round 1 88k/73 s, delta 28k/31 s, re-bind 32k/23 s on tree f1e48ef0): round-1 plan-text mismatch fixed; delta's blocking `drawn: bool` flag on the `answered` fixture fixed; optional `debug_assert` on the list guard kept as `deliberate: the comment states the invariant, and `cost` is computed three lines above it`.
- **approve** (code-review backend, 69k/60 s, delta 25k/27 s): shared `OWN_SOURCE`, `holds-until:` on the deferred stop column and the per-session loop, parent-source test all fixed; keeping a version-pin test kept as `deliberate: the Rust seat asked to delete the tautology; the holds-until note names the change`.
- **approve** (code-review lang:rust, 66k/44 s, delta 28k/53 s): `OWN_SOURCE`, byte-slice removed, one-id-per-line list for a fully unpriced arm, tautological test deleted, stray comma, guard simplified: all fixed; the `OutcomeSource` enum kept as `deliberate: the shared constant was the seat's stated minimum`.
- **approve** (code-review tui, 49k/20 s, delta 28k/49 s): one arm-figure form and one unpriced phrasing fixed; "add one to machine.toml" and the header wording kept as `deliberate: 80-column budget and the setting's own name; wording settled by the plan panel's tui seat` (the seat accepted both).
- Rebased onto v0.76.0 (#534) before the push: the vendored pack refresh dropped out as empty and the CHANGELOG bullets moved above `## v0.76.0`; the rule pack changed, so a fresh round ran on tree b8712a32: implementation-review **approve** (57k/24 s), backend **approve** (68k/36 s; rollback target corrected to 0.76.0), lang:rust **approve** (40k/15 s), tui **approve** (76k/22 s; CHANGELOG wording for `none (no completed task)` kept as `deliberate: the JSON is the machine interface, and no plugin, script or doc greps the old line`).
- Phase 4 done 2026-11-22: the programme `docs/programmes/phase8-learner/` is charted (8 tickets: two research, two grilling, one on the activations ledger, two dated checks, one person's step), so `status: done`.

## Outcome

2026-11-22. Phases 1–3 merged in relais#533 (squash 49c5091f) and released as v0.77.0; Phase 4 charted the programme **relais learns which model a stratum needs** (`docs/programmes/phase8-learner/`), whose frontier starts with the two research tickets on the gate inputs and the missed-failure measure. The relais follow-up (a blocked run's unregistered candidate) is `docs/programmes/follow-ups/f10`. The person's step below is also the programme's ticket **Price the two foreign models so their sessions rejoin the headline**.

## Person's optional step

Price the two foreign models so their sessions rejoin the headline, in `~/.config/relais/machine.toml` (shape in `docs/INTEGRATIONS.md:286`); the local qwen can be priced at 0:

```toml
[[pricing.models]]
ids = ["gpt-6.1-sol"]
input = ...        # micro-USD per million input tokens
output = ...
[[pricing.models]]
ids = ["qwen3.6:35b-a3b"]
input = 0
output = 0
```

<!-- panel: repos=relais reviewers=backend,lang:rust,tui,unix,codex body-sha=d612f8ce0395 -->
