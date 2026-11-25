+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# Do the plugin's tool.call .catch answers reach the caller?

## Question

f25 measured, in `claude plugin test` on Claude Code 2.1.331, that when the
`SendMessage`/`TaskStop` guard's `tool.call` hook threw, the call reached the
tool whatever the hook's `.catch` answered (`{ deny }` and `{ result }` alike).
The plugin has other `tool.call` hooks whose `.catch` is their only failure
answer, and no test covers any of them. If the engine ignores those answers
too, a thrown error in them runs the tool or answers something else than the
plugin meant.

## Look at

- `claude-plugin/hooks/register.ts`: the `.catch` of the `run` tool, of
  onboard, trust and trust_holdouts (`CONSENT_FAILED`), of decide and of
  feedback.
- f25's Resolution, and its probe: a hook body that throws, with a `.catch`
  that returns a marker, asserted through `$.tool.call`.
- Whether a registered MCP tool (`$.tool.register`) behaves unlike a built-in
  tool such as `TaskStop`.

## Answer looks like

One test per `.catch` that throws in its hook and asserts the caller gets the
`.catch` answer. Where the answer does not arrive, the hook catches its own
failure inside its body instead, as f25 did, and the test goes green.
