+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# Settle a worker whose final turn leaves a background shell running

## Question

A native worker that ends its turn with a background shell still running
reads as `running` in `$.agent.list()` until the shell ends. relais settles
the dispatch only on that listing (`observe` in
`claude-plugin/hooks/dispatches.ts`). [f18](f18-timeout-wall-time-waits.md)
measured what this costs:

- 41% of timed-out wall time (33,220 s over 12 runs) passed after the worker
  had already answered;
- 9 of 9 successful workers that stopped with a shell still running settled
  124–1,108 s late.

No worker resumed on a task notification after its stop. How should a
dispatch end at the worker's final turn, so that its shells die before the
snapshot? The design has to answer:

- **The end signal.** `SubagentStop` already arrives at the right moment,
  with `last_assistant_message` and `background_tasks`. It goes through
  `settle_by_agent`, and the native path does not read it today. Is it the
  signal, or should it be a `turn.complete` with `end_turn` plus an agent
  listed `running` with only shells left?
- **Stopping the shells.** Settling must stop the agent, so that its shells
  are killed before the candidate snapshot. How does that sit with the
  in-place fence (`inplace_worker_may_run`)?
- **Held slots.** Can a shell that is still running `greengate slot run` or
  `amont slot run` keep holding a host slot after the agent is stopped?
- **The answer text.** "I'll wait for the completion notice; DONE is not
  yet declared" must reach relais as a not-done answer for verify and
  repair, not as DONE.

## Look at

- `claude-plugin/hooks/agents.ts` (`check`, `onTurn`) and
  `claude-plugin/hooks/dispatches.ts` (`observe`).
- `crates/relais/src/adapter/native.rs` `await_stop`, and the
  `SubagentStop` → `settle_by_agent` path in SPEC §"Release on
  `SubagentStop`".
- f18's Resolution and `f18-assets/`. The replay inputs are the `SubagentStop`
  payloads in `~/.local/state/relais/hook_journal.jsonl*`: their
  `background_tasks` show `type: "shell"` entries still `running`.

## Answer looks like

A plan, then a change, in which a worker that ends its turn with only shells
running settles within seconds, with its shells stopped. A test replays a
`SubagentStop` payload with a running shell and shows the settle. The
measure afterwards: a week of `hook_journal` with no worker stop that
settles more than 60 s late.
