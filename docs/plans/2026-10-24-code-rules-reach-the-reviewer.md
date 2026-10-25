---
canonical: decisions:docs/plans/2026-10-24-code-rules-reach-the-reviewer.md
phases: [4]
status: active
branch: feat/candidates-in-review
---
# Code rules reach the reviewer (pointer)

Canonical plan: [decisions/docs/plans/2026-10-24-code-rules-reach-the-reviewer.md](https://github.com/fredericrous/decisions/blob/main/docs/plans/2026-10-24-code-rules-reach-the-reviewer.md) (ADR-0044).

This repository carries relais's half of Phase 4 (behaviour 3):
- the reviewer prompt lists adr-lint's candidates for the candidate commit (`adr-lint candidates --base <base> --head <candidate> --json`), as a new `GuidanceSource`;
- a missing or failing adr-lint is a receipt gap, `candidates missing (…)`, and never a silent drop;
- the receipt counts the dispositions (violation, fine, undisposed);
- each ruling is written as an `adr-lint.ruling/1` record with `tool: relais`.

## Verification

(filled at finish)
