---
canonical: github:fredericrous/decisions/docs/plans/2026-10-28-each-harness-tool-runs-alone.md
phases: [2]
status: done
branch: feat/aval-optional-when-absent
---
# Each harness tool runs alone

Pointer. The plan and its review panel live in decisions, with the ADR
(harness.standalone-tools). This repository carries part 2 of the plan's
Changes: a missing sibling is reported as a gap, never a failure, unless the
repository asks for it explicitly.

## Verification

- `make check`, run without a wall limit: exit 0, plugin tests 332/332. relais's own runner hit `baseline_unrunnable` twice at load average ~570, so the relais-implementation worker made the change as a subagent.
- `cargo test -p relais --lib context::tests::` → 47 passed, including the 13 tests for aval's absence and the gap text.
- Sandbox, PATH = only relais + `/usr/bin:/bin`, a repo with `.adr.yaml` and `amont.conf`:
  - `relais init --detect --write` writes all three integrations `optional` under the two comment lines, with no line over 80 columns.
  - `relais doctor` reports each one as "optional and `X` is not on PATH; its checks are a reported gap, not a pass".
- **Not run:** the end-to-end `relais run` with a mapped key, and the two negative runs. The sandbox repo needs a trust grant, which only the person gives, and a run at that load ends at baseline. The unit tests at the resolver and `assemble` levels cover the same branches.

## Implementation review

approve, after round 1 and two Delta passes (about 179k tokens in total).
- Round 1 had one blocking finding: the plan's tests were missing, although the worker's report claimed them. Fixed in 72f3ffd: 13 tests, and the gap text now matches the plan.
- Delta 1 had one blocking finding: the placeholder name `value`. Fixed in ea43759.
- deliberate: `NotInstalled` is a unit variant (the absence is the whole fact).
- Carried: the end-to-end run and the negative runs, against the released binary.
