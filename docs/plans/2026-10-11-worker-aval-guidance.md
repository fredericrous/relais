---
status: done
branch: feat/worker-aval-guidance
repos: [relais]
adrs: []
---

# Workers see the repository's rules and relevant decisions

## Review panel

👉 **Decide:** none. Approve if the bet holds: workers get a constraint id index plus aval-ranked guidance, the reviewer gets full statements, and nothing advisory ever blocks a run.
📍 relais · plan reviewed, nothing built · next: worktree, Phase 1 (port, parsers, manifest). Panel: backend (opus-5-5), lang:rust (opus-5-5), tui (sonnet-5-5), unix (sonnet-5-5).
**Changed by review:** guidance never blocks a run and never counts toward SizingProblem (own 16 KiB cap); the reviewer gets full statements and never sees `aval rule`; argv uses the joined `--path=`/`--text=` form with full ids.
📄 Full reviews: [2026-10-11-worker-aval-guidance.reviews.md](2026-10-11-worker-aval-guidance.reviews.md)
**Verdicts:** round 1: backend rework, three approve-with-changes. Round 2: backend approve-with-changes, then approve on the final body. Seven low/medium findings carried into implementation (see Full reviews).

## Context

A relais worker gets architecture context only from decision keys named in
the contract (`architecture.keys`) or mapped in `relais.toml`
(`[architecture] mapping`). Neither is filled in practice: the skill and the
`mcp__relais__run` schema never mention `architecture.keys`, and most
repositories (this one included) have no mapping. Rules (`aval rules`,
69 constraints here) never reach a worker at all. The person's own session
gets the heads table and the constraint index from the aval `SessionStart`
hook; a native subagent does not get that output, and relais's
`SubagentStart` hook only pairs spawns.

So a worker writes code without `errors.never-swallowed`,
`functions.no-flag-arguments` and the rest, and the reviewer judges the
candidate without them, though a review is meant to block on a constraint.

Intended outcome: every worker prompt carries (1) the index of
active constraint ids, the same one-line-per-prefix index the session gets
(ADR-0026 `guidance.session-load`: an index, detail on demand), with an
instruction to run `aval rule <id>` before writing code a constraint
governs, and (2) the few decisions and rules aval ranks as relevant to the
write scope and the objective, in full. The reviewer cannot run aval (its
tools are Read, Grep, Glob, LSP), so it gets the full statement of every
active constraint instead of the index. The planner gets the relevant
block only. Guidance never blocks a run. The
binding key set (contract + mapping, which blocks on
contradiction/undecided) is unchanged.

## Design

### What the worker receives

```
the constraint rules below are in force; review blocks on them.
run `aval rule <id>` for one rule's text before you write code it governs.
--- constraint index ---
errors: errors.explicit-absence, errors.never-swallowed, errors.typed-values
functions: functions.no-flag-arguments, functions.no-hidden-side-effects
…
--- end constraint index ---
the decisions and rules below are ranked by aval as relevant to this task
(advisory ranking; each decision listed is active).
--- relevant guidance ---
`routing.session-model` (ADR-0001): <choice>
rule `errors.never-swallowed` (constraint): <statement>
…
--- end relevant guidance ---
```

Ids are full and copy-pastable, grouped by prefix. The `aval rule <id>`
line appears only in worker prompts that carry the index (the planner gets
the relevant block alone), never
in the reviewer prompt (the reviewer cannot run aval). Both blocks use the existing fenced
`data_list_block` (project text, never instructions — SPEC §16). Keys
already in `constraints` (binding) are not repeated in the relevant block.

The reviewer prompt carries a `--- constraints in force ---` fence with the
full statement of every active constraint (≈21.8 KB on this repo) in place
of the index, plus the relevant block. It never contains `aval rule` and
never falls back to the bare id index.

### Sources (aval's public CLI, `--json`)

- Index: `aval rules --level constraint --json` → active rules (ids,
  statements, `omitted`); relais groups ids by prefix itself (`--index`
  cannot be combined with `--json`). aval already omits rules about traits
  the repository's areas do not name. Measured: 26 ms; rendered index
  ≈ 2.1 KB (69 ids).
- Relevant: `aval relevant --path=<p>… --text=<objective> --top 5 --json`.
  Measured: 165 ms. Arguments go in joined form (`--path=-rf` parses
  correctly); the objective is truncated to 2 KiB on a char boundary before
  it goes into argv. Paths: each write-scope glob reduced to its literal
  directory prefix (`crates/relais/src/**` → `crates/relais/src`; `**`
  alone, a brace glob such as `{a,b}/**`, or any glob character before the
  first `/` → no path), plus `read_hints`. Use `keys[]` with
  `state == "active"` (choice) and `rules[]` (id, level, statement). A
  ranked key that is not active is recorded in the manifest and left out of
  the prompt: the ranking is a suggestion, so it never blocks the run.
- Latency: the index (26 ms) is fetched once per run and shared by every
  package (cached in `AvalCli`); the ranking (165 ms) is asked once per
  `assemble`, so ×(1 + packages) per run. Each guidance spawn has its own
  5 s timeout (`tooling::GUIDANCE_TIMEOUT`), not the 30 s probe timeout.

### Failure policy

Guidance never blocks a run, in any mode. Only binding key resolution
(contract keys + mapping) can fail a run, as today. Any failure of
`aval rules` or `aval relevant` — non-zero exit (including exit 2 from an
older aval without `relevant`), timeout, malformed JSON, or aval optional
and installed but failing — becomes a `GuidanceGap { source: index |
relevant, exit, detail }` in the manifest plus a run Event, and the
affected block is absent. aval optional and not installed → no guidance, one
gap. `GuidanceGap` is its own type; `ContextError::ToolFailure` is not
reused.

### Sizing

Guidance is not counted in the binding `package_bytes` / `SizingProblem`
check. It has its own cap, `GUIDANCE_BUDGET_BYTES` = 16 KiB, for the worker
block and the planner's relevant block. Over the cap, the relevant block is dropped first, then
the index; each drop is a `GuidanceGap` with measured and budget bytes.
Expected on this repo: index 2.1 KB + top-5 ≈ 3 KB, well inside.

The reviewer's full constraint text is counted against the reviewer prompt
alongside `REVIEW_PATCH_BUDGET_BYTES` (192 KiB, runner/mod.rs:4158). If all of
it would not fit, the reviewer gets the whole statements, in index order,
that do fit (never cut mid-statement); the omitted ids are a `GuidanceGap`
with measured and budget bytes, shown in the receipt gap line.

### Code

- `crates/relais/src/context/mod.rs`
  - Extend the `DecisionResolver` port with
    `fn constraint_rules(&self, cancel) -> Result<Rules, GuidanceGap>` and
    `fn relevant(&self, query: &RelevantQuery, cancel) -> Result<Relevant, GuidanceGap>`.
    Both have default bodies returning empty guidance, so the closure impl
    and existing tests compile unchanged. `AvalCli` overrides them through
    the same bounded `procs::run_with_timeout` spawn `aval_resolve` uses,
    taking the run's cancel flag (factor the spawn into one helper; `AvalCli`
    stays the one place aval is spawned).
  - Pure parsers `parse_rules_output` / `parse_relevant_output` beside
    `parse_aval_output`, typed; unrecognised shape → `GuidanceGap`.
  - Pure `index_lines(&[RuleRef]) -> Vec<String>` (full ids, grouped by
    prefix, sorted).
  - `assemble` adds `guidance: Guidance` to `ContextManifest`
    (`#[serde(default)]`, old manifests still parse). It stores the index
    ids, `omitted` from `aval rules`, the exact ranked keys (with state) and
    rule ids used, the gaps, and the aval version.
- `crates/relais/src/runner/mod.rs`
  - `build_prompt` (worker, ~4470) renders the index and relevant blocks
    after the binding constraints; the reviewer prompt (~3815) renders the
    full constraint statements and the relevant block.
  - The run outcome/receipt gets one line only when there is a gap, e.g.
    `guidance: relevant missing (aval exit 2)`; nothing when all is well.
- `crates/relais/src/runner/scheduler.rs` (~1132) — planner prompt gets
  the relevant block only (it splits work; the index adds nothing there).
- `docs/SPEC.md` §7 (line ~271): state that relais passes the constraint
  index (full statements to the reviewer) and aval's advisory relevance
  ranking, that guidance never blocks, and that path-to-decision mappings
  stay the binding source.
- `claude-plugin/skills/relais/SKILL.md`: one line — name
  `architecture.keys` in the contract when the task depends on a decision
  (binding), since the relevant block is advisory.

### Rejected in review

- No index cache: the measured cost (26 ms + 165 ms) is low.
- Manifest hash left as is: guidance is recorded as evidence, never
  compared.
- Non-UTF-8 paths are not a case: contract paths are already `String`.

## Phases

1. [x] Port + parsers + `assemble` + manifest, with tests (below).
2. [x] Prompts (worker, reviewer, planner), receipt gap line, SPEC and skill
   text.
3. [x] `make check` exits 0, implementation review, PR, merge on green, release
   (minor: v0.13.0) so the installed `relais` carries it.
4. [x] Live: run a contract with write_scope `crates/relais/src/context/**` in
   this repo. Expected: the run's `manifest.json` has `guidance.index`
   containing `errors.never-swallowed` and `guidance.relevant` with ≥1
   rule; the dispatched worker prompt contains the `constraint index` and
   `relevant guidance` fences exactly once each; the reviewer prompt
   contains the full statement of `errors.never-swallowed`.

## Verification

Observed on `eca07d0` (gate `make check`: 1347 unit tests and every integration suite passed; relais run `run-65d8e596b8948-144e1` accepted, `make check` exit 0).

| Check | Input → expected | Observed |
|---|---|---|
| Unit (context) | aval 1.9.0 fixtures, index, globs, undecided key → as stated | pass: `rules_json_parses_to_the_active_rules_and_the_omitted_counts`, `relevant_json_keeps_the_active_key_the_undecided_key_and_the_rules`, `malformed_output_is_a_gap_naming_its_source`, `index_lines_carry_full_ids_grouped_by_prefix`, `a_glob_is_reduced_to_its_literal_directory`, `guidance_is_recorded_and_an_undecided_ranked_key_never_blocks`, `an_unknown_rank_state_is_kept_as_aval_wrote_it` |
| Unit (failure) | exit, exit 2, timeout, bad JSON, cancel → gap, run continues | pass: `every_way_a_spawn_fails_is_a_gap`, `each_guidance_failure_is_a_gap_and_the_run_continues`, `malformed_json_from_the_spawn_layer_becomes_a_gap_too`, `an_aval_that_will_not_launch_is_one_gap`, `a_cancelled_spawn_returns_a_gap_well_before_the_timeout` |
| Unit (argv) | `--`-objective, `-rf` path, 2 KiB cut → joined form | pass: `argv_uses_the_joined_form_for_flag_looking_values` |
| Unit (sizing) | over budget, never SizingProblem, reviewer N of M | pass: `over_budget_drops_relevant_first_then_the_index`, `over_the_guidance_budget_the_relevant_block_goes_first`, `guidance_never_raises_a_sizing_problem`, `a_reviewer_budget_that_fits_n_of_m_gives_n_whole_statements_and_a_gap` |
| Unit (runner) | fences once, no `aval rule` without index, escapes, planner, reviewer, receipt | pass: `the_worker_prompt_fences_the_index_and_the_relevant_block_once_each`, `without_an_index_there_is_no_aval_rule_line_and_no_empty_fence`, `an_id_and_a_choice_carrying_the_end_marker_cannot_close_their_fence`, `the_planner_gets_the_relevant_block_and_never_the_index`, `the_reviewer_reads_full_statements_and_never_aval_rule`, `the_reviewer_does_not_read_a_rule_twice`, `a_binding_key_is_not_repeated_among_the_relevant_ones`, `the_receipt_shows_a_guidance_line_only_when_there_is_a_gap`, `guidance_note_is_empty_without_a_gap_and_names_a_manifest_gap_or_a_reviewer_gap` |
| Old manifest | JSON without `guidance` → parses | pass: `a_manifest_written_before_guidance_still_parses`, `a_guidance_less_manifest_value_reads_as_empty` |
| `make check` | exit 0 | exit 0 (pre-commit gate on `eca07d0`) |
| Packages | each package run emits its own gap events | by reading: package runs go through `RunEngine::run` → `preflight` → the one `assemble` call (`runner/mod.rs:1601`) |
| Phase 4 live run | relais 0.13.0, write_scope `crates/relais/src/context/**`, review required → guidance in manifest and prompts | pass: run `run-65d8fdb6e6f86-11113` accepted. The manifest has 69 index ids including `errors.never-swallowed`, 5 ranked keys, 5 ranked rules and no gaps. The worker prompt has the `constraint index` and `relevant guidance` fences once each, plus the `aval rule` line. The reviewer prompt has `constraints in force` with the full `errors.never-swallowed` statement and no `aval rule` line. The worker did not run `aval rule`; a one-line doc comment did not call for it. |

## Decision log

- 2026-10-11, Phases 1–2: built by relais run `run-65d8d338c6051-dc00` (sonnet@medium). The run ran out of wall clock in verification; its single red check was `release_scenarios::a_run_awaiting_a_person_is_answered_by_decide_and_drops_off_the_open_list`, which passes alone (13 s). The base was red on two other tests in the same suite under the same load.
- `AvalCli` passes its own cancel flag to guidance spawns, but `main.rs` builds `AvalCli::new` without one, so CLI runs are bounded by the 5 s `GUIDANCE_TIMEOUT` only (as `aval resolve` is not cancellable from the CLI either). Wiring a run cancel flag into `AvalCli` is its own change.
- deliberate: the receipt gap line goes into `VerificationReport.notes`, not a new receipt field (receipt types live in `verify/`).
- deliberate: when aval cannot be launched, the relevant query is skipped, giving one gap, not two.

## Implementation review

approve-with-changes, then approve-with-changes on the Delta with all six findings resolved (no new finding).
Fixed: GapCause enum, RankState parse, exhaustive Ended match, cancel test, guidance_note tests, reviewer-gap reset, observed verification above.
Shipped: #203, released in v0.13.0 (#204); Phase 4 observed live.
Tokens: 74k + 85k; 75 s + 19 s.

<!-- panel: repos=relais reviewers=backend,lang:rust,tui,unix body-sha=5ac21fdc6046 -->
