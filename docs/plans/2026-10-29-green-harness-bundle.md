---
canonical: github:fredericrous/decisions/docs/plans/2026-10-29-green-harness-bundle.md
phases: [3]
status: active
branch: feat/grh-skills-move
---
# Green Harness bundle

Pointer. The plan and its review panel live in decisions, with ADR-0050
(harness.skills-source). This repository carries phase 3: relais stops
writing `relais-verified-push` and `relais-architecture-conflict`, which
now ship from greenharness/skills, and `relais doctor --json` reports the
Claude Code integration grh reads.

## Verification

| Check | Input | Expected | Actual |
|---|---|---|---|
