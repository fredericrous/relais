+++
type = "task"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais"]
+++
# Let a session stop a relais worker whose run has ended

## Question

On 2026-11-25, run `run-6610ef826b9a0-5ea8` (f21, in place in
`relais-wt-f21`) had a worker that ran `pkill -f "relais-wt-f21"`. That
pattern matched its own relais driver, whose argv holds the worktree path.
relais exited, and the run ended `interrupted`. The native worker kept
running, and kept editing the worktree in place.

- `mcp__relais__cancel` answered "already ended: interrupted" and stopped
  nothing.
- `TaskStop` from the session was refused: "This agent belongs to a relais
  run; relais drives it."

So nothing the session could do would stop it. The session waited for the
worker to end its own turn before taking over the tree.

How should an ended run's still-listed agent be stopped?

- Option A: `cancel` of an ended run stops any agent still bound to its
  dispatches.
- Option B: the `TaskStop` guard lets a stop through once the agent's run
  is over.
- Separately: should worker guidance, or the guards, refuse a `pkill -f`
  whose pattern matches the worker's own driver?

## Look at

- `claude-plugin/hooks/register.ts`: the `tool.call` guard on `SendMessage`
  and `TaskStop`.
- `claude-plugin/hooks/agents.ts` (`handleStop`) and the cancel path
  (`relais native cancel`).
- The in-place fence (`inplace_worker_may_run`, SPEC §"A lost worker
  fences the tree"). It covers a lost plugin, not a lost driver.

## Answer looks like

A plan, then a change: a session can stop an ended run's agent through
relais, and a test shows `cancel` (or the guard) stopping it.

## Resolution

Closed 2026-11-25 by the plan
`docs/plans/2026-11-25-cancel-stops-orphaned-workers.md`.

- **Option A.** `mcp__relais__cancel` of a run that has ended, or that
  cancel just reaped, `TaskStop`s each agent the run's dispatches bound
  that `agent.list()` shows still running. It does this even when an
  earlier cancel had already marked those dispatches stopped. An agent
  rebound to another run is skipped. That ownership is checked again after
  the list is read. The reply keeps relais's line first, then one line per
  agent: `stop sent to agent …`, `could not stop agent …: …`, a late line
  naming the 30 s window, or a line saying no agent is known or still
  running. The list read and the stops share one 30 s window.
- **Option B is not taken.** The model's `TaskStop` stays denied: a guard
  that opens once a run "is over" would judge from a store that a plugin
  reload empties. The deny now names the run to cancel, or, for a run
  that is still starting, `mcp__relais__status`. When the plugin no longer
  knows the run, it sends the model to the person.
- **`pkill -f`.** There is no guard on a worker's kill commands. Worker
  guidance or a runner change is f26.
- Found on the way: the guard may fail open when `agent.list()` throws.
  That is f25.
