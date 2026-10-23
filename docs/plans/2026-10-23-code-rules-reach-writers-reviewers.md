---
canonical: github:fredericrous/aval/docs/plans/2026-10-23-code-rules-reach-writers-reviewers.md
phases: [3]
status: active
branch: feat/code-rules-full-text
---
# Code rules reach the agent that writes and the agent that reviews

Pointer. The plan and its review panel live in aval (merged in aval#42); the
ADR is decisions ADR-0043 (decisions#86). This repository carries Phase 3:
relais workers and its reviewer get the full text of the `governs: code`
constraints and the applicable decisions, the reviewer blocks on a violation
the diff introduces, and workers get the read-only aval MCP tools.

Carried into implementation from the plan review:
- `fit_statements` measures the rendered text (statement + body) and takes the
  gap source (worker or reviewer); a test feeds a body larger than the budget.
- Only exit 2 and the stale-pack case keep every id in the index; exit 1/3 and
  a timeout lose the index as today. The "aval lacks `--full`" gap is recorded
  only when the retry succeeds.
- Decisions: `aval relevant --top 20`; an empty diff skips the call (zero
  `--path` is exit 2, not an old aval).
- Review is conditional (routing or changed verification inputs): say so, and
  count the unreviewed share over 20 runs.
- Measured on this branch: relais has 55 code constraints, 59 KB as `--full`
  text (the plan estimated 52 and 55 KB).
