---
canonical: decisions:docs/plans/2026-10-31-board-and-programme-map.md
phases: [4]
status: active
branch: feat/programme-frontier
---
# Board and programme map

Pointer. The plan and its review panel live in decisions, with ADR-0051
(`work.programme-map`, `harness.tool-boundaries`). This repository carries
phase 4: `relais programme list|frontier|show|check` (SPEC §31) and the
`programme` skill, released as 0.43.0 (another session released 0.42.0,
the dataset seed command, while this branch was open; the board showed its
`chore/release-0.42.0` branch).

## Verification

- Integration tests (`crates/relais/tests/it/programme_cli.rs`, user-flow style): the five-ticket fixture (frontier B then D, `cut -f1` ids only, a `prog/<slug>/D` branch removes D and `--all` labels it, closing D lists C), five `check` problems each exiting 2 with `path:line` on stderr and empty stdout, `list` over two repositories, a read-only `RELAIS_STATE_DIR`, a linked worktree behaving like its main checkout, an unknown ticket type reported and left off the frontier → pass in the commit gate (31 checks).
- Real run, branch build, toy programme `object-store` charted in scratch repositories `lake` and `ingest` (5 tickets: one closed, one out of scope, three open of which one blocked, one naming both repositories), `RELAIS_STATE_DIR` a read-only empty directory:
  - `check` → expected exit 0 → actual exit 0, no output.
  - `frontier` → expected `t02-store` and `t04-quota` → actual `t02-store\tgrilling/hitl\tlake\tWhich object store holds the lake` and `t04-quota\ttask/afk\tlake,ingest\tMeasure today's bucket sizes`.
  - `show` → expected counts and the blocked ticket → actual `open 3 (frontier 2, blocked 1, in flight 0), closed 1, out of scope 1` and `How ingest writes a partition (t03-ingest-writer): blocked by t02-store`.
  - `list --root <scratch>` → actual the map path, its destination, `open 3 frontier 2 closed 1`.
  - branch `prog/object-store/t04-quota`, then `frontier --all` → actual `t04-quota … in-flight prog/object-store/t04-quota`.
  - A dangling `blocked_by` and a closed ticket with no Resolution and no map line → expected exit 2, empty stdout → actual exit 2, stdout 0 bytes, three `path:line:` problems, `3 problems`.
  - The read-only state directory → actual still empty (0 entries).
- Trigger rate of the `programme` skill (`skill-trigger-test … --runs 3`, 5 should-prompts in the person's phrasing incl. "what was the session working on X? resume it", 4 near-misses: a typo fix, "plan this feature", a red CI job, a `--json` flag) → expected should ≥ 0.8, near-miss ≤ 0.2 → actual should 15/15, near-miss 0/12, $1.40 on the session's default model.

## Implementation review
