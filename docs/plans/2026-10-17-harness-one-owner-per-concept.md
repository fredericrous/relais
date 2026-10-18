---
canonical: decisions:docs/plans/2026-10-17-harness-one-owner-per-concept.md
phases: [2]
status: done
---
# The harness has one owner per concept (pointer)

The plan lives in the decisions repository, which owns ADR-0039. This
repository carries Phase 2's relais slice: `docs/quiet-runs.md` keeps only
the watcher protocol and links to amont's document for the active entry,
and every relais pointer to the entry, `relais --help` included, names
amont's document.

## Outcome

| Check | Expected | Actual |
|---|---|---|
| Bare `docs/quiet-runs.md` hits outside `CHANGELOG.md`, `docs/plans/` and `.adr/` | about the watcher protocol; every entry reference says "amont `docs/quiet-runs.md`" | `quiet.ts:5` and `quiet.rs:4` name the protocol and give the entry as amont's; `quiet.rs:64`, `main.rs:131` and `docs/quiet-runs.md:11` say amont's |
| `relais --help` | names amont's page for the entry | the `Quiet` doc comment (`main.rs:130-131`) does; it reaches users with the next release |
| `aval relevant --text "quiet run active entry" --top 3`, from relais | `harness.cross-tool-reads` in the top 3, `active` | ranked 2nd, `active`, after the pack refresh |
| `aval check` | exit 0 | 49 decision records, no findings |
| relais pre-commit | pass | 29 checks passed; `plugin-test` warned for this one commit (below) |

## Decision log

- 2026-10-18: `pre-commit-plugin-test` was set to `warn` for one commit only, on the person's explicit choice. The setting was unset straight after. It had failed three times on 5000 ms timeouts at load 114–340; run alone at load 19 it passed in 39 s. CI runs the plugin tests on the pull request.
- 2026-10-18: the fleet pack refresh rides this branch (`guidance.generated-block-rides-the-change`).
- 2026-10-18: two passages in this page touch the writer's side on purpose. Both say what relais itself does and do not specify the writer:
  - step 1 notes that a writer sweep may also remove an entry;
  - step 2 describes the reader's own liveness check, including `OpenProcess` on Windows.

## Implementation review

approve. One low finding was kept as deliberate: the liveness line is relais's own check. 19k tokens in 16 s.
