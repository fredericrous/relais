---
canonical: github:fredericrous/decisions/docs/plans/2026-10-29-green-harness-bundle.md
phases: [3]
status: done
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
| skills retired, marker-judged | `install::tests::install_writes_neither_published_skill_and_judges_old_copies_by_the_marker` | the two skills not written; a marked copy removed; an unmarked (grh) copy kept | 1 passed |
| integration from real listings | `doctor_json_describes_the_integration_from_real_listings` (fixtures from Claude Code 2.1.312) | present (user and project scope), partial both ways, absent; `plugin` finding agrees | passed |
| unknown says why | `doctor_json_says_why_the_integration_is_unknown` | malformed listing and missing claude: `unknown` with a reason | passed |
| full suite | `amont rehearse --wait` | pass | rehearsal passed after 11m25s |
| lint | `cargo clippy --all-targets -D warnings` | clean | clean |

## Implementation review

approve-with-changes (52k). Recorded here: the rows above. deliberate: a plugin installed but disabled reports `present` in `integration`, so grh adopts it and never reinstalls over the person's choice; the `plugin` finding still warns. The integration object's contract joins the canonical Phase 3 text in the decisions phase-log update.
