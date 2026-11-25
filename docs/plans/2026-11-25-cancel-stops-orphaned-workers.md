---
status: active
branch: prog/follow-ups/f24-stop-an-orphaned-worker
repos: [relais]
adrs: []
plan-id: p-01M887CV4RSX2G5A53FZK7R9H3
---
# Cancel of an ended run stops its orphaned workers

## Review panel
📄 Full reviews: [reference](2026-11-25-cancel-stops-orphaned-workers.reviews.md).
👉 **Decide:** none — approve if cancel stopping an ended run's still-running workers, with the model's `TaskStop` kept denied, is the way out.
📍 relais · 1 phase · 1 step for you · next: Phase 1. Panel: backend, lang:typescript, tui, unix (opus-5-5), codex (gpt-6-astra).
**Changed by review:** the deny names the run to cancel or sends to the person; ownership is re-checked after the list read; the reply says `stop sent`, not `stopped`.
**Verdicts:** r1 4 approve-with-changes → r2 3 approve, unix approve-with-changes; codex approve-with-changes (2 high applied, backend re-approved); 6 carried; words 1004 → 1358 (×1.35, high findings and wrong-statement fixes); remove 0, merge 0.

## Picture

```
 worker runs `pkill -f relais-wt-f21`: its driver dies,
 the run ends `interrupted`, the native worker keeps editing

 today                        after this plan
 cancel: "already ended"      cancel: "already ended", then
   nothing stopped              TaskStop each agent bound to
 TaskStop: denied               this run and still running;
 session waits for the turn     the reply names each one
                              model's TaskStop: denied, the
                                message names cancel
```

Cancel of an ended run now stops the workers the run left running.

## Context

Follow-up ticket `docs/programmes/follow-ups/f24-stop-an-orphaned-worker.md`.
On 2026-11-25 a worker of run `run-6610ef826b9a0-5ea8` ran
`pkill -f "relais-wt-f21"`, which matched its own relais driver's argv. The
driver exited and the run ended `interrupted`; the native worker kept
running. `mcp__relais__cancel` answered "already ended: interrupted" and
stopped nothing; `TaskStop` from the session was refused by the plugin's
guard (`claude-plugin/hooks/register.ts:544`).

Facts checked in this session:
- `relais native cancel` of a terminal run prints `run <id> already ended:
  <state>` and exits 0, for any session (`crates/relais/tests/it/native_cancel.rs:163,184`).
  A driverless live run is reaped and also exits 0; a live driver answers
  `cancel requested …`.
- `cancelRun` (`claude-plugin/hooks/runs.ts:772`) already branches on that:
  exit 0 and not `cancel requested` calls `stopDispatchesOf`, which only
  sets `isStopped` on the run's dispatches. It never stops an agent.
- The store keeps `dispatchChild` (dispatch → child, `child.key` = the run
  id once the first event names it, `runs.ts:469`), `dispatches`
  (dispatch → agent) and `agentDispatch` (agent → its current dispatch)
  after the driver exits: nothing deletes them on child exit.
- `stopAgent` (`agents.ts`) calls `TaskStop` through `fx.tool.call`, whose
  origin is the plugin, so the guard lets it through (`register.ts:545`).
  It returns nothing today; its outcome only reaches the timeline.
- An agent is still running when `fx.agent.list()` lists it with a status
  outside `ENDED` (`completed`, `failed`, `killed`, `dispatches.ts:56`).

## Approach

The ticket's questions, answered:

1. **Option A: `cancel` of an ended run stops its agents.** In `cancelRun`,
   on exit 0 with an answer other than `cancel requested` (an ended run, or
   a driverless one that cancel just reaped), after `stopDispatchesOf`, so
   no late `stopped` goes out while the stops are awaited:
   - collect the agents of every dispatch whose child key is the run,
     stopped or not (a first cancel already marked them stopped while the
     worker ran on);
   - keep an agent only when `store.agentDispatch.get(agent)` is a dispatch
     of this run: a `continue` may have rebound it to a successor run's
     dispatch, which this cancel must not stop. The check runs again after
     the agent list is read, just before each `TaskStop`, since a rebind can
     land while the list is awaited. With no agent kept, one
     line: `no agent of run <run> is known to this plugin; if one still
     runs, ask the person to stop it in Claude Code` (a reload empties the
     store);
   - read `fx.agent.list()` once and keep the agents listed with a status
     outside `ENDED`. A list that cannot be read adds one line to the reply
     and stops nothing; none left adds `no agent of run <run> is still
     running`;
   - `TaskStop` each one in parallel, bounded by `STOP_CONFIRM_MS` as
     today: one bound in all, not one per agent, so the reply comes within
     the cancel process's own 20 s plus 30 s. The CLI's line stays first and
     byte-identical, the agent lines follow one per line, and `exit status`
     stays last: `stop sent to agent <id>` (the `TaskStop` went through;
     its end is not re-listed), `could not stop agent <id>: <why>`,
     or `agent <id>: TaskStop had <why>; it may still take`, where `<why>`
     is built from `STOP_CONFIRM_MS`, never a literal. A failed or late stop
     leaves the cancel a result, not an error: the run has ended, and the
     text is the signal.
   `stopAgent` returns its `StopAnswer` so `cancelRun` can say it; its
   timeline notes stay.
2. **Option B is not taken.** The guard stays closed. Whether a run is over
   is judged from the store, which a plugin reload empties; a guard that
   opened on "no live run known" would let the model stop a live run's
   worker. The deny for `TaskStop` gets its own text, in both branches of
   the guard (its `.catch` fallback included), naming the way out. With the
   run known (`agentDispatch`, then `dispatchChild`): `This agent belongs
   to relais run <run>; relais drives it. To stop it, call
   mcp__relais__cancel with run <run>: cancelling a run that has already
   ended also stops its agents still running.` With none known: `This
   agent belongs to a relais run this plugin no longer knows. Ask the
   person to stop it in Claude Code.` `SendMessage` keeps today's text.
3. **The `pkill -f` question.** No guard here: judging a `pkill -f` pattern
   against the driver's argv means parsing shell and regexes, and the
   cancel above now recovers from it. A follow-up ticket goes to the inbox:
   worker guidance (or the runner) keeps a worker's process kills off its
   own driver, e.g. kill by pid, never `pkill -f` on a path.

The cancel tool's description gains one clause: an ended run's agents
still running are stopped, where this plugin still knows them.

## Phases

- [ ] Phase 1 — cancel stops orphaned agents. `claude-plugin/hooks/runs.ts`
  (`cancelRun`, `stopDispatchesOf` unchanged), `claude-plugin/hooks/agents.ts`
  (`stopAgent` returns its answer; a `stopAgentsOf(fx, store, run)` helper),
  `claude-plugin/hooks/guards.ts` (`TASK_STOP_DENY_MESSAGE`),
  `claude-plugin/hooks/register.ts` (guard picks the message by tool, `.catch` branch included; cancel
  description), tests below, `CHANGELOG.md` (Unreleased).
  Done by hand in this session: the change is one helper and its tests.

### 🧑 Your steps
- Reinstall the relais plugin with relais's own install command for Claude — the cancel stops agents only once the installed plugin carries it (installed: 0.72.0). The follow-ups handoff already lists this step.

## Non-goals
- Opening the `TaskStop` guard to the model (Option B).
- A guard on a worker's `pkill`/`kill` commands (follow-up ticket instead).
- Stopping agents after a plugin reload emptied the store: no record ties
  an agent to the run then.
- Any Rust or wire change.

## Code review
- relais: lang:typescript (backend left out: no engine, ledger or wire change, one plugin helper over existing store maps; unix and tui left out: no command, flag or pane change, only the cancel tool's reply lines)

## Delivery
One worktree and one PR from `origin/main`, merged on green. The plugin
change ships with the next relais release and takes effect after the
person's plugin reinstall. Rollback is a reinstall of the prior plugin;
a stop already sent is not undone, and a worker stopped mid-edit leaves
partial writes in its tree.

## Verification
- `make plugin` passes, read by its counts; each new test of the new
  behaviour fails on `origin/main`.
- A new test file `claude-plugin/tests/cancel-orphans.test.ts`, with a fake
  `relais native cancel` and a fake `TaskStop` that answers at once:
  - an ended run (`run R already ended: interrupted`, exit 0) whose agent is
    listed `running` → exactly one `TaskStop` with that `task_id`, and the
    reply holds `stop sent to agent <id>`;
  - the agent listed `completed` → no `TaskStop`;
  - a second cancel after the first one marked the dispatches stopped,
    agent still `running` → it is stopped;
  - the agent rebound to another run's dispatch, before the cancel or
    while its agent list is awaited → no `TaskStop`;
  - a live run (`cancel requested …`) → no `TaskStop` from the plugin;
  - a refused `TaskStop` → the reply says `could not stop agent <id>`;
  - two agents, one `TaskStop` unanswered: the scripted clock moves
    `STOP_CONFIRM_MS` (the 5 s test limit rules out real time) → both
    lines, the late one holding `${STOP_CONFIRM_MS / 1000} s`, the CLI's
    line first;
  - `fx.agent.list()` throws → one line, no `TaskStop`;
  - a reaped driverless run (`… cancelled`, exit 0) → its agent is stopped;
  - an ended run with no dispatch in the store → the `is known to this
    plugin` line, no `TaskStop`.
- `guards.test.ts`: the model's `TaskStop` of a relais agent is denied with
  the run id and `mcp__relais__cancel` when the store binds it, with `stop
  it in Claude Code` when only the agent list names it, and with the new
  text on the `.catch` path; `SendMessage` keeps its text.

<!-- panel: repos=relais reviewers=backend,lang:typescript,tui,unix,codex body-sha=d1c9bfbea897 -->
