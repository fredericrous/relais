+++
type = "task"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais"]
+++
# The SendMessage/TaskStop guard may fail open when the agent list throws

## Question

While writing f24's tests (2026-11-25), a plugin test set `agent.list` to
reject. A model `TaskStop` of a relais agent the store holds then reached
the tool (`{ result: 'ok' }`). The same probe on origin/main (84513ff94)
gives the same result, so this predates f24. The engine reported:

```
test's agent.list hook was skipped: test: list down
hook failed closed: relais: errorKind=unknown errorChars=44 (tool.call; its .catch answered)
relais's tool.call hook: relais: HooksError: no implementation for agent.list
```

The `.catch` is said to have answered, and its code returns `{ deny }` for a
store-bound agent, yet the call went through. Whether the handler ran, and
what it returned, is not known.

Why does the guard let the call through when `$.agent.list()` throws?

## Look at

- `claude-plugin/hooks/register.ts`: the `tool.call` guard on
  `SendMessage` and `TaskStop`, and its `.catch`.
- `claude-plugin/tests/guards.test.ts`: the f24 deny tests. The red test:
  spawn `d1`, set `listHold` to a rejected promise, call the model's
  `TaskStop` of `agent-1`, and assert a `deny` (it gets `{result:'ok'}`
  on origin/main today).
- The mod engine's `.catch` contract for `tool.call` (the plugin-authoring
  skill).

## Answer looks like

A red test on origin/main, then a fix that denies a store-bound agent when
the list cannot be read (for example, read the store before the list), and
the test green.

## Resolution

Measured 2026-11-25 in `claude plugin test` on Claude Code 2.1.331 (CI pins
2.1.291; not checked live), on this guard only: when the hook threw, the
engine ran its `.catch` ("its .catch answered") but the call still reached
the tool. With the `HooksError` from `$.agent.list()`, a `.catch` answer of
`{ deny }` and one of `{ result }` both came back as `{ result: 'ok' }`; with a
plain `throw`, a `{ result }` answer did too (a `{ deny }` one was not tried).
The guard's `.catch` was not what decided.

Fix: the guard reads the store first (a store-bound agent is denied without
the list) and catches a failed `$.agent.list()` inside the hook, failing open
only for an agent the store does not hold, as before. The test
`a model TaskStop or SendMessage of a store-bound relais agent is denied when
the agent list throws` (tests/guards.test.ts) went red before the fix
(`stop.deny` undefined) and is green after it. A second test covers the caught
list: an agent the store does not hold still goes through. 591 of 591.
