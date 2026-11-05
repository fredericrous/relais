---
status: active
branch: fix/routing-rule-claude-only
repos: [relais]
adrs: []
---
# The routing rule is for Claude sessions only

## Review panel
⚠ unreviewed: a small, tightly scoped fix the person asked for directly after a session failed on it; no panel was run.
📍 relais · plugin hook · next: implement and verify.

## Context
The plugin adds one line to the system prompt of every session: "Route each
bounded implementation or inspection task through relais instead of doing it
yourself". A session running a non-Claude model through a proxy (observed on
2026-11-04 with a ChatGPT model) read that line as a developer instruction
that a user message cannot override. It refused to edit directly when the
person said "don't use relais", and stayed blocked. relais routes for Claude
sessions; its router and run ladder are Claude's.

## Approach
`prompt.compose` reads the session model (`$.session.model()`) and adds the
`relais-routing` section only when the model is a Claude model or is unknown.
A model name that holds none of `claude`, `opus`, `sonnet`, `haiku`, `fable`
is not Claude. An unknown model keeps today's behaviour: the line is added.

- `hooks/consent.ts`: `isClaudeModel(model)` and an optional `model` argument
  on `withRoutingSection`; a non-Claude model returns the composed prompt
  unchanged.
- `hooks/register.ts`: the compose hook reads the model, and a failed read is
  an unknown model.
- Tests: Claude ids and aliases get the section; a proxy id (`gpt-5`,
  `qwen`) does not; unknown and failed reads keep it; once-only still holds.

## Non-goals
No change to the routing tools, the skill text, the router, or the person's
own `~/.claude/CLAUDE.md`, which carries its own relais line.

## Verification
- `claude-plugin/tests/consent.test.ts` covers each case above.
- `make plugin` passes.
