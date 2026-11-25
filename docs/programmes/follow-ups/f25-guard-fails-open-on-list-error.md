+++
type = "task"
mode = "afk"
status = "open"
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
