+++
type = "task"
mode = "afk"
status = "closed"
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

## Resolution

Closed 2026-11-25 by the plan
`docs/plans/2026-11-25-settle-worker-at-final-answer.md`.

- **The end signal** is `turn.complete` with `reason: 'answer'` (with no
  reason: not aborted and a non-empty answer), not `SubagentStop`. Its
  `background_tasks` lists the session's in-flight work, the agent itself
  included, with no owner field, so it cannot say which shells are one
  worker's own.
- **Stopping.** An answered agent still listed `running` 5 s after the
  answer is `TaskStop`ped by the plugin. Probed: `TaskStop` sends SIGTERM,
  and the whole process tree dies, a child in its own process group
  (`setpgrp`) included. A stopped agent can still be continued, so repair
  works. The stop is confirmed only by a listing asked for after its
  answer (a store sequence number, not the clock), and the dispatch then
  ends **`completed`**: `killed` would drop the answer (`native.rs`), and
  the attempt would end `TerminalResultMissing` with no verify.
- **Bounds.** A refused stop is noted and not retried. A stop with no
  answer in 30 s is noted once, and still counts if it takes late. A stop
  still unconfirmed 30 s later is noted once.
- **Held slots** are `flock`s held by the `slot run` process, so they are
  freed when the shell's tree dies.
- **The answer** keeps the worker's text, then a blank line and
  `relais plugin: the worker ended its turn with background work still
  running; …`. relais never reads `DONE`: verify judges the tree, and
  repair sees the note.
- **Measure** and live checks: [f23](f23-settle-at-answer-week.md), counted
  from the plugin reinstall.
