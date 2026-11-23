---
canonical: greenharness-skills:docs/plans/2026-11-23-reviewers-get-preferred-model.md
phases: [3]
status: active
---
Part of [Reviewers get their preferred model wherever they are launched](https://github.com/greenharness/skills/blob/main/docs/plans/2026-11-23-reviewers-get-preferred-model.md): the `relais-review` template says `model: inherit` and `preferred-model: opus`.

## Code review

- relais: backend, lang:rust (unix, tui, architect left out: one template string and its test change)

## Observed (phase 3)

- `cargo test -p relais --lib install::`: 86 passed. New:
  `the_review_agent_inherits_with_opus_preferred` (the template's literal
  `model: inherit` + `preferred-model: opus` lines, no `model: opus` line) and
  `a_pinned_review_agent_upgrades_to_the_inheriting_one` (an untouched
  pinned v0.77.0-style block plans as `Update` and is rewritten).
- Pending until after the release: the user-scope install, and the installed
  `~/.claude/agents/relais-review.md` reading `inherit` + `preferred-model`.
- Open: with `model: inherit`, `router/pins.rs` no longer serves a pin for
  `relais-review`. With session routing on, the router may override an
  explicit `model: "opus"` on any unpinned agent, the 20 workflow reviewers
  included. That predates this change and is left to its own decision.
