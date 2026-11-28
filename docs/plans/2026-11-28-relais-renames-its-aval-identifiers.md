---
status: active
branch: chore/t28-rename-identifiers
repos: [relais]
adrs: [ADR-0072, ADR-0073, ADR-0012]
plan-id: p-01M8FW733PZEDDXB18BF6R4C9X
---
# relais renames its aval identifiers

## Review panel
👉 **Decide:** none — approve if renaming relais's `Aval*` code and three stored names, old records read as unknown, is the slice.
📍 relais · 3 phases · 1 release · next: Phase 1. Panel: backend, lang:rust, tui, unix (opus), codex.
**Changed by review:** `#[serde(other)] Unknown` catch-all pinned by a test; ledger test proves no `BlockCode` alias; pilot moved to `relais run`.
**Verdicts:** 4 approve-with-changes, then 4 approve; backend approve on the final body; 3 carried; words 751 → 1010 (×1.34, mostly checks); remove 0 (1 rejected), merge 0.
📄 Full reviews: [2026-11-28-relais-renames-its-aval-identifiers.reviews.md](2026-11-28-relais-renames-its-aval-identifiers.reviews.md)

## Picture

```
 relais          0.93.0                    0.94.0
 ------          ------                    ------
 code            AvalCli, AvalVerdict ---> GreentopoCli, GreentopoVerdict
                 run_aval, aval_*     ---> run_greentopo, greentopo_*
 manifest        tool_versions.aval   ---> tool_versions.greentopo
                 guidance.aval_version --> guidance.greentopo_version
                 gap aval_too_old     ---> greentopo_too_old
 blocked: code   aval_tool_failure    ---> greentopo_tool_failure
 old records     kept as written; an old field reads as unknown
 kept            RetiredAvalIntegrationKey, only_aval_on_path_* tests
```

New records carry the new names; old ones stay history, never read by alias.

## Goal
decisions topo-spotter-rename t28, relais slice. t22 kept relais's internal
`Aval*` names and three stored names because three branches were open
(an-t35, f73, f33); all three have merged. This renames them after
greentopo, so relais's code says the name of the program it runs, and new
records carry the new stored names. Released as relais 0.95.0 (0.94.0 was another session's, cut while this ran) and installed
on this Mac.

## Non-goals
- What forbids the old name, kept as it is: `RETIRED_TOPO_KEY`,
  `PolicyError::RetiredAvalIntegrationKey` and its refusal tests in
  `policy/mod.rs`; the `only_aval_on_path_*` tests in `tooling.rs` and
  `context/mod.rs` (an `aval` file on PATH is never spawned);
  `an_aval_key_that_skips_the_toml_entry_point_is_an_unknown_field`.
- Version facts in comments (`aval 1.9.0`, `aval 1.10.0`, `aval 1.11.0`,
  the captured `aval --json rules …` argv), `repo.rs`/`workspace/mod.rs`
  lines that describe the past, `.adr.yaml`'s `aval 1.7`, the
  `checked-by.txt` fixture, the `aval.pack` file name inside recorded rule
  JSON, the dated `docs/AUDIT-*`/`docs/REVIEW-*`, the
  `docs/programmes/follow-ups/*` records, `docs/SPEC.md:576`.
- Removing the old-greentopo fallback (`GapCause::AvalTooOld`, the
  previous-argv retry): it is renamed only; whether it is dead since
  greentopo 2.0.0 is a follow-up.
- No read alias for any old stored name: an alias is a live reader of
  `aval` (ADR-0073 `harness.renames`).
- `greengate.conf`'s vendored `adr-lint.toml` glob (t22: inert until the
  next `greengate add`).

## Code review
- relais: backend, lang:rust, unix, tui

## Behaviour
- **Types and functions**, by exact token, counted before and after:
  `AvalCli` → `GreentopoCli`, `AvalVerdict` → `GreentopoVerdict`,
  `AvalRun` → `GreentopoRun`, `AvalTooOld` → `GreentopoTooOld`,
  `AvalToolFailure` → `GreentopoToolFailure`, `aval_resolver` →
  `greentopo_resolver`, `aval_resolve_in`/`aval_resolve` →
  `greentopo_resolve_in`/`greentopo_resolve`, `run_aval` →
  `run_greentopo`, `parse_aval_output` → `parse_greentopo_output`,
  `aval_launch_hint` → `greentopo_launch_hint`, `aval_mode` →
  `greentopo_mode`, `aval_rule_instruction` → `greentopo_rule_instruction`,
  `aval_version`/`manifest_aval_version` → `greentopo_version`/
  `manifest_greentopo_version`, `aval_missing` → `greentopo_missing`,
  `old_aval` → `old_greentopo`, `fake_aval`/`repo_with_aval` →
  `fake_greentopo`/`repo_with_greentopo`, and the test names built on them.
  Comments that name the current tool say greentopo. No behaviour changes.
- **Stored names, new records only** (t06: readers switch in one release,
  old rows stay history and read as unknown):
  - the context manifest's `tool_versions.aval` → `tool_versions.greentopo`;
    an old manifest holding `"aval": "1.12.0"` still parses, with
    `greentopo: None`;
  - `guidance.aval_version` → `guidance.greentopo_version`, the same way;
  - the guidance gap cause `aval_too_old` → `greentopo_too_old`; an old
    manifest whose gap says `aval_too_old` still parses, its cause read as
    `GapCause::Unknown`: a `#[serde(other)]` unit variant, the catch-all
    for any bare-string tag, never written by relais and not an alias.
    serde documents `other` for internally tagged enums only, so a test
    pins it; if serde refuses it on this externally tagged enum, a
    hand-written `Deserialize` maps unknown tags to `Unknown` instead.
    Production reads no manifest back; the back-compat tests do;
  - the block code `aval_tool_failure` → `greentopo_tool_failure` in
    `blocked:` output and new ledger rows. An old row's
    `{"code":"aval_tool_failure"}` still reads back through `status` and
    `explain` as the string it is.
- The baseline cache key hashes `ToolVersions`; it moves anyway with every
  relais version, so the rename costs no extra baseline run.

## Phases
- [x] Phase 1 — rename the identifiers and stored names:
  `crates/relais/src/context/mod.rs`, `crates/relais/src/context/guidance.rs`,
  `crates/relais/src/context/candidates.rs`, `crates/relais/src/policy/mod.rs`,
  `crates/relais/src/cli/run.rs`, `crates/relais/src/cli/replay.rs`,
  `crates/relais/src/runner/config.rs`, `crates/relais/src/runner/preflight.rs`,
  `crates/relais/src/runner/prompt.rs`, `crates/relais/src/runner/scheduler.rs`,
  `crates/relais/src/runner/review/engine.rs`,
  `crates/relais/src/runner/review/mod.rs`, `crates/relais/src/verify/mod.rs`,
  `crates/relais/src/runner/tests/mod.rs`,
  `crates/relais/src/runner/tests/candidates.rs`,
  `crates/relais/src/runner/tests/budgets.rs`,
  `crates/relais/src/runner/tests/exhaustion.rs`,
  `crates/relais/src/runner/tests/verify_kind.rs`,
  `crates/relais/tests/it/release_scenarios.rs`
- [x] Phase 2 — back-compat tests: an old manifest with `tool_versions.aval`,
  `guidance.aval_version` and an `aval_too_old` gap parses, with
  `greentopo: None` and the cause `GapCause::Unknown`, as does an arbitrary
  `"unknown_x"` (context tests); a ledger row with
  `{"code":"aval_tool_failure"}` reads back through `run_status` and
  `transitions()` (what `explain` prints) as that string, while
  `serde_json::from_str::<BlockCode>` of it is `Err` (no alias), beside
  `a_run_with_sandbox_era_rows_still_reads_back` without its round-trip:
  `crates/relais/src/context/mod.rs`, `crates/relais/src/ledger/mod.rs`
- [ ] Phase 3 — release 0.95.0 through the `tag-release` skill, install it,
  print `relais --version`. The CHANGELOG entry names each old → new stored
  name (`aval_tool_failure`, `tool_versions.aval`, `guidance.aval_version`,
  `aval_too_old`) and says old rows keep the old string: `CHANGELOG.md`,
  `Cargo.toml`, `Cargo.lock`

## Decision log
- 2026-11-28 — block code `greentopo_tool_failure`, following t06's pattern
  (`<new tool name>` in place of the old prefix); recorded in t28's
  Resolution.
- 2026-11-28 — a catch-all for the old gap cause rather than an alias, so an
  old manifest stays readable without a reader of `aval_too_old` (kept
  against backend's remove finding: five context tests hold that a stored
  manifest stays parseable; the cost is one unwritten variant).
- 2026-11-28 — check `greenbelay board --repo relais` before Phase 1 and
  before the push; this branch or hsa-t15, whichever lands second, rebases
  (hsa-t15 adds no aval-named code, so its conflicts are mechanical).
- 2026-11-28 — minor release: the `blocked:` code and manifest fields are
  output a reader can see, renamed for new records.
- 2026-11-28 — the fleet pack was refreshed on this branch in its own
  commit (ab130b51a), because worktree-task's Start step found it behind
  decisions; it changes no code.
- 2026-11-28 — checks ran with `PATH=$HOME/.cargo/bin:$PATH`: Homebrew
  `rust` 1.100.0 (reinstalled 11-27) shadows the rustup 1.94.1 pin, and its
  clippy fails pre-existing code (`verify/heavy/tests.rs` `fetch_update`).

## Verification
- Phase 1: `git grep -cw <token> -- crates` per name, before → after: each
  old token 0 outside the Non-goals' list; each new one = old count + its
  own count before (`greentopo_resolve`: +2, MCP tool names) → every
  renamed token 0, but 4 back-compat test inputs (`aval_tool_failure` ×3
  in the ledger test, `aval_version` ×1 in the manifest test); new counts
  equal the old (`GreentopoVerdict` 95, `greentopo_resolver` 25,
  `parse_greentopo_output` 13, `GreentopoCli` 13, `fake_greentopo` 12,
  `run_greentopo` 10; `greentopo_resolve` 2 + 2 MCP names)
- Phase 1–2: `cargo test -p relais` through `greengate slot run` → green,
  including the new back-compat tests → `cargo test -p relais` green
  before the review fixes (lib 2803, it 368, others 99/26/16/25/1; 5
  ignored); after them, on the pinned 1.94.1, `cargo clippy -p relais
  --all-targets -- -D warnings` clean, `--lib context:: ledger::` 284
  passed, the integration test 1 passed. `#[serde(other)]` works on this
  externally tagged enum (the test pins it)
- Phase 1: pilot `relais run` using the existing CLI integration-test
  setup (isolated state, trust, supported backend and session prerequisites),
  a clean scratch repository and a contract with one binding architecture
  key. Declare `[integrations] greentopo = { mode = "required", bin =
  "<absolute fixture path>" }`. First use an executable script that exits 3:
  expect `blocked: greentopo_tool_failure` and process exit 3, without
  requiring a launch hint. Then, on macOS, use an executable script whose
  shebang names a nonexistent interpreter: it passes file discovery but
  fails to launch; expect the same code and exit, with the declared `bin`
  and relais.toml named in the hint. Record stdout and stderr separately
  and assert neither case launches a worker → done as the integration test
  `a_failing_greentopo_blocks_the_run_as_greentopo_tool_failure` (the
  scenario World is the CLI harness): both bins exit 3, stderr carries
  `blocked (greentopo_tool_failure): \`code.paradigm\`: unparseable
  greentopo output: ""` (relais prints `blocked (<code>): <detail>`, not
  the `blocked: <code>` this line first expected), only the launch failure
  names the `bin` and relais.toml, no `aval` on either stream, and the fake
  worker's `invocations.log` is never written
- Phase 3 (next, the release): `relais --version` → `relais 0.95.0`
- Census, on `HEAD` before the push and on `origin/main` after the merge,
  outside CHANGELOG, `docs/plans`, `docs/programmes`, `.adr/packs` and
  `HEADS.md`: `git grep -nIiE '(^|[^[:alpha:]])aval|adr-lint'` plus
  case-sensitive `git grep -nE '[a-z]Aval'` → on HEAD 59d75c9e9 only the
  Non-goals' list: `RETIRED_TOPO_KEY`/`RetiredAvalIntegrationKey` and their
  refusal tests (`policy/mod.rs`), the `only_aval_on_path_*` tests
  (`tooling.rs`, `context/mod.rs`), version facts (`guidance.rs`,
  `context/mod.rs:48`, `review/mod.rs:730`), `aval.pack` inside recorded
  rule JSON, the old `adr-lint.candidates/1` refusal input, the
  back-compat test inputs, `checked-by.txt`, `.adr.yaml`'s `aval 1.7`,
  `greengate.conf`'s vendored glob, dated `docs/AUDIT-*`/`docs/REVIEW-*`
  and `docs/SPEC.md` refusal/history text

## Implementation review
- implementation-review (opus): approve-with-changes ×3; fixed: `.ok()` reason, `m`→`written`, the integration test's no-dispatch/stdout/real-format checks; ticks, results, pack line here. ~230k tok, 120 s.
- backend: approve-with-changes → approve; deliberate: no negative bin-name check for the exit-3 case (tui asked to leave room to name the program). 123k, 96 s.
- lang:rust: approve-with-changes → approve; `Unknown` comment narrowed, its limits pinned. 88k, 61 s.
- unix: approve-with-changes → approve. 79k, 62 s. tui: approve; follow-up: name the program and exit code in the unparseable-output detail. 56k, 44 s.

## Outcome
<!-- panel: repos=relais reviewers=backend,lang:rust,tui,unix,codex body-sha=40115fecb59c -->
