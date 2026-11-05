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
`prompt.compose` adds the `relais-routing` section only when the model the
prompt is composed for (`e.promptModel`, a field the engine always sets) is a
Claude model. A model name that holds none of `claude`, `opus`, `sonnet`,
`haiku`, `fable` is not Claude. No name keeps today's behaviour: the line is
added. Reading the event, not `$.session.model()`, avoids an effect call
inside the hook and its failure modes.

- `hooks/consent.ts`: `isClaudeModel(model)` and an optional `model` argument
  on `withRoutingSection`; a non-Claude model returns the composed prompt
  unchanged.
- `hooks/register.ts`: the compose hook passes `e.promptModel`.
- Tests: helper cases in `consent.test.ts`; the hook itself, run by the
  engine, in `compose.test.ts`.

## Non-goals
No change to the routing tools, the skill text, the router, or the person's
own `~/.claude/CLAUDE.md`, which carries its own relais line.

## Verification
Observed (359 + 2 tests, `claude plugin validate claude-plugin` passed):
- `consent.test.ts`: `claude-opus-5-5`, `opus`, `Haiku`, `fable-5-1` → section added; `gpt-5`, `qwen`, `gemini-3-pro`, `o4-mini` → prompt returned unchanged; no name, `''`, `'  '`, `42` → section added.
- `compose.test.ts`, through the engine: `promptModel: 'claude-sonnet-5-5'` → `relais-routing` present; `promptModel: 'gpt-5'` → absent.
- The 2026-11-04 session recorded `gpt-6.1-sol` on all 438 of its messages (session transcript), a name that holds none of the Claude words.
- Not run: a live non-Claude session through a proxy. `holds-until:` the next session on a proxy model shows no `# relais` section; the person's own `~/.claude/CLAUDE.md` line is not touched here.
- `make plugin`, CI conclusion and the merged SHA: pending, next phase (land).
