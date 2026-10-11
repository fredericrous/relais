---
status: active
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

Intended outcome: every worker and planner prompt carries (1) the index of
active constraint ids, the same one-line-per-prefix index the session gets
(ADR-0026 `guidance.session-load`: an index, detail on demand), with an
instruction to run `aval rule <id>` before writing code a constraint
governs, and (2) the few decisions and rules aval ranks as relevant to the
write scope and the objective, in full. The reviewer cannot run aval (its
tools are Read, Grep, Glob, LSP), so it gets the full statement of every
active constraint instead of the index. Guidance never blocks a run. The
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
line appears only in worker and planner prompts that carry the index, never
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
- Latency: 26 ms + 165 ms per `assemble`, ×(1 + packages) per run. No cache
  needed.

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
and planner blocks. Over the cap, the relevant block is dropped first, then
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

1. Port + parsers + `assemble` + manifest, with tests (below).
2. Prompts (worker, reviewer, planner), receipt gap line, SPEC and skill
   text.
3. `make check` exits 0, implementation review, PR, merge on green, release
   (minor: v0.13.0) so the installed `relais` carries it.
4. Live: run a contract with write_scope `crates/relais/src/context/**` in
   this repo. Expected: the run's `manifest.json` has `guidance.index`
   containing `errors.never-swallowed` and `guidance.relevant` with ≥1
   rule; the dispatched worker prompt contains the `constraint index` and
   `relevant guidance` fences exactly once each; the reviewer prompt
   contains the full statement of `errors.never-swallowed`.

## Verification

- Unit (context): parse fixtures captured from aval 1.9.0 for `rules` and
  `relevant` (active key, undecided key, rules, malformed → gap); index
  grouping, and every rendered id token is exactly an id returned by
  `aval rules`; glob → path reduction (`a/b/**/*.rs` → `a/b`, `**` → none,
  `{a,b}/**` → none, `*/x` → none, literal file kept); undecided ranked key
  never blocks.
- Unit (failure): non-zero exit, exit 2, timeout and malformed JSON each
  give a `GuidanceGap` with the block absent and the run continuing;
  cancelling during `relevant` stops the spawn.
- Unit (argv): objective starting with `--`, path `-rf`, oversize objective
  truncated to 2 KiB on a char boundary.
- Unit (sizing): over `GUIDANCE_BUDGET_BYTES` drops relevant first, then
  index, each recorded as a gap with measured and budget bytes; guidance
  never raises `SizingProblem`; a reviewer budget that fits N of M
  statements gives exactly the first N whole statements and a gap naming
  the omitted ids, also in the receipt line.
- Unit (runner): `build_prompt` contains both fences, each fenced once; the
  `aval rule` line is absent when the index is; fence escapes for an id and
  a `choice` containing the end marker, and a statement with newlines (same
  test shape as the existing constraints-fence test ~9930); the receipt
  shows the gap line only when there is a gap; the reviewer prompt never
  contains `aval rule`, in the full and the partial case.
- Old manifest JSON without `guidance` still deserializes.
- `make check` exits 0 (the CI command).
- Phase 4 live run, as stated there.

<!-- panel: repos=relais reviewers=backend,lang:rust,tui,unix body-sha=5ac21fdc6046 -->
