---
status: active
branch: feat/read-hints-and-widen
repos: [relais]
adrs: [0012]
plan-id: p-01M7HAEFSE5HV9TWTPD5GBK3HD
---
# Refuse unresolvable read hints at the call; widen the scope limits

## Review panel
Panel: backend, lang:rust. Both approved final body 796938831d7a (backend: no blocking or high findings; lang:rust: approve).
Recorded in the review ledger as round 4 against plan 796938831d7a.

reviewers=backend,lang:rust body-sha=796938831d7a

## Picture
```
orchestrator ──► relais native contract (stdin: JSON)
                 │ 1. schema check (exists)
                 │ 2. NEW: fingerprint_hints(base_ref, read_hints)
                 │     ├─ all resolve: write .relais/tasks/<hash>.json
                 │     └─ any not: refuse, list the hints (exit 2)
                 ▼
           run starts (preflight still checks successors)

scope widening (policy/mod.rs):  max_files 1→2,  max_lines 10→20
```
A contract's hints are checked once, at submit; the run then starts as before.

## Goal
Two decisions already made in the time-to-done programme, shipped together:
- **B4 (t08):** a read hint that does not resolve at the contract's `base_ref` is refused when the contract is submitted, not 12 minutes later in preflight. 80 runs ended `read_hint_unresolvable` that way.
- **B6 (t07 option b):** `[execution] widen_scope_small_max_files` 1 → 2 and `widen_scope_small_max_lines` 10 → 20. This amends ADR-0012's 1-file / 10-line choice. In t06's replay this settles 14 more of 96 runs as `scope_widened_small`, each with review raised to Required.

## Non-goals
- A warning-and-drop mode for hints: a hint is a claim about the tree, and a wrong one means the orchestrator described a tree it has not read (t08).
- Changing preflight's hint check: it stays for successors and `from_candidate`, where the tree differs from HEAD.
- Companion lists, reference scans, `WidenRule::Importers` (t07 options a and c, ruled out by t12).
- Anything the protected-path or policy-input refusals decide.

## Code review
- relais: backend, lang:rust (unix, tui left out: no new output, only a refusal path and two constants)

## Behaviour

### B4: refuse at `relais native contract`
- After the schema check (`crates/relais/src/cli/native.rs`, `native_contract_command`, the existing refusal at :211-221), resolve `contract.base_ref` with `workspace::resolve_base` as `relais plan` does (`cli/plan.rs:211-235`), then call `context::fingerprint_hints(&SystemGit, &root, &base_sha, &contract.read_hints)`. The same function preflight (`runner/preflight.rs:522`) uses.
- If `base_ref` does not resolve (not a repository, or a successor's base not yet made), skip the check; preflight keeps it.
- If any hint is unresolvable, print to stderr one line naming each hint and its detail, for example `relais native contract: read hints that do not resolve at the base: crates/foo (no such path); fix the paths and resubmit`, and return `CliOutcome::InvalidInput` (exit 2). Nothing is written to `.relais/tasks/`.
- An empty `read_hints`, or a contract with `from_candidate`, skips the check: its hints resolve in the candidate tree. The plugin already turns exit 2 into `relais refused the contract: …` (`runs.ts:213-216`).
- A missing path makes `git ls-tree` succeed with no output, so the detail reads `no file under it exists at <tree>` (`context/mod.rs:768`). The refusal names the hint, not the detail text.
- Preflight is unchanged.

### B6: widen the small-scope limits
- `DEFAULT_WIDEN_SCOPE_SMALL_MAX_FILES` 1 → 2 and `DEFAULT_WIDEN_SCOPE_SMALL_MAX_LINES` 10 → 20 (`crates/relais/src/policy/mod.rs:565-566`). `docs/SPEC.md` states the defaults too and is updated.
- The per-repository override (`[execution] widen_scope_small_max_files` and `widen_scope_small_max_lines` in `relais.toml`) is unchanged, so a repository can still set its own.
- **Trust grants.** The defaults are left out of the authority hash (`policy/mod.rs:3094-3116`), so repositories that never set these keys keep their grants and now allow 2 files and 20 lines. A repository that sets them explicitly to 1 / 10 or 2 / 20 changes hash and must be re-trusted; any other value hashes as before. Intended; ADR-0012 is amended and the CHANGELOG says so.
- The protected-path and policy-input refusals run before the limits and are unchanged.
- The test that pins the defaults is `policy/mod.rs:3111-3113`; update it to 2 / 20. Add one test with the keys unset: 2 files / 20 lines widened, 3 files not.

## Phases
- [ ] Phase 1 — B4: `crates/relais/src/cli/native.rs` (the hint check after the schema check), `crates/relais/tests/it/portable_scenarios.rs` beside `native_contract_saves_a_contract_and_refuses_prose` (a contract with a missing hint → exit 2, no file written, the hint named on stderr; a resolvable hint → accepted; a `base_ref` that differs from HEAD is checked at the base).
- [ ] Phase 2 — B6: `crates/relais/src/policy/mod.rs` (the two constants), `docs/SPEC.md`, `.adr/records/0012-a-run-settles-a-timeout-and-a-small-overflow.md` (an amendment), and `policy/mod.rs:3111-3113` (to 2 / 20), plus one test with the keys unset.
- [ ] Phase 3 — release relais v0.66.0 (tag-release): `Cargo.toml`, `Cargo.lock`, `claude-plugin/.claude-plugin/plugin.json`, `CHANGELOG.md`. Install it here with brew. The Claude Code plugin update is the person's, since the relais hook refuses it from the agent.

## Decision log
- 2026-11-16 — Both changes ship in one relais release: they are small and independent, and one review covers both.
- 2026-11-16 — B4 checks at the contract's resolved `base_ref`, as `relais plan` does, not HEAD. A base that does not resolve, and a `from_candidate` contract, are left to preflight.
- 2026-11-16 — B6 widens repositories that never set the keys without changing their grants, and re-trusts only those that set the keys to 1 / 10 or 2 / 20. Intended; ADR-0012 is amended.

## Verification
- Phase 1: a contract with `read_hints: ["does/not/exist"]` → `relais native contract` exits 2, stderr names the hint, `.relais/tasks/` gains no file. A contract whose hints exist → accepted, file written.
- Phase 2: a run whose change touches 2 files and 20 lines → `scope_widened_small`; 3 files → not widened. `cargo test -p relais --lib policy` passes, with the pinned tests updated.
- Replay: the t06 replay command, run on its 96 recorded runs → 65 `scope_widened_small` (was 51).
- A contract with `base_ref: "HEAD~1"` whose hint exists only at HEAD → refused at submit, exit 2.
- Production, 7 days after install: `read_hint_unresolvable` preflight blocks on runs without `from_candidate` → 0 (was 80).
- Phase 3: `relais --version` shows 0.66.0 after `brew upgrade`.

## Outcome

<!-- panel: repos=relais -->
