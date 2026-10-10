// relais's requests to spawn, continue and stop agents, and what comes back:
// each turn's usage, and the one `stopped` a dispatch ends with.

import type { Fx } from './fx.ts'
import { type Dispatch, observe, openDispatch, stoppedPayload, withTurn } from './dispatches.ts'
import { sendBound, sendStopped } from './callbacks.ts'
import { detach, every, type Child, type Store } from './store.ts'
import { applyEvent, isLive } from './timeline.ts'

const POLL_MS = 2000

const text = (value: unknown): string | undefined =>
  typeof value === 'string' && value.length > 0 ? value : undefined

// The dispatch id a spawn's `description` carries: `relais <dispatch>`.
export const dispatchOfDescription = (description: unknown): string | undefined => {
  const match = /^relais (\S+)$/.exec(typeof description === 'string' ? description : '')
  return match ? match[1] : undefined
}

const failureText = (reason: unknown): string =>
  reason instanceof Error ? reason.message : String(reason)

// The reason a tool call was refused or errored, or undefined when it went through.
const refusal = (result: any): string | undefined => {
  if (result?.deny) return String(result.deny)
  if (result?.isError) return text(result.text) ?? 'the call errored'
  return undefined
}

function open(store: Store, child: Child, d: Dispatch) {
  store.dispatches.set(d.id, d)
  store.dispatchChild.set(d.id, child)
  store.agentDispatch.set(d.agent, d.id)
}

// A dispatch that ended without an agent run to report: relais is told it failed.
function failDispatch(fx: Fx, store: Store, child: Child, dispatch: string, agent: string, why: string) {
  const d = { ...openDispatch(dispatch, agent), isStopped: true, answer: why }
  store.dispatches.set(dispatch, d)
  store.dispatchChild.set(dispatch, child)
  sendStopped(fx, store, child, dispatch, stoppedPayload(d, 'failed'))
}

export async function handleSpawn(fx: Fx, store: Store, child: Child, line: any) {
  const dispatch = text(line.dispatch)
  const prompt = text(line.prompt)
  if (!dispatch || !prompt) return
  // The `agent.spawn` hook reads this, so it is set before the call.
  store.pending.set(dispatch, { cwd: text(line.cwd) ?? child.cwd })
  const request: Record<string, unknown> = { prompt, description: `relais ${dispatch}` }
  const type = text(line.subagent_type) ?? text(line.subagentType)
  if (type) request.subagentType = type
  if (text(line.model)) request.model = line.model
  let spawned: any
  try {
    spawned = await fx.agent.spawn(request)
  } catch (reason) {
    spawned = { deny: failureText(reason) }
  } finally {
    store.pending.delete(dispatch)
  }
  const agent: string | undefined = spawned?.deny ? undefined : spawned?.agentId ?? (await agentOf(fx, dispatch))
  if (!agent) {
    failDispatch(fx, store, child, dispatch, '', spawned?.deny ?? 'the agent did not start')
    return
  }
  open(store, child, { ...openDispatch(dispatch, agent), isBound: true })
  markAgent(store, dispatch, agent, type)
  sendBound(fx, store, child, dispatch, agent)
  startPoll(fx, store)
}

// The spawn's answer names the agent; should it not, the agent is the one in
// the list whose description is this dispatch's.
async function agentOf(fx: Fx, dispatch: string): Promise<string | undefined> {
  try {
    const listed: any[] = await fx.agent.list()
    return listed.find(a => a.description === `relais ${dispatch}`)?.id
  } catch {
    return undefined
  }
}

export async function handleContinue(fx: Fx, store: Store, child: Child, line: any) {
  const dispatch = text(line.dispatch)
  const agent = text(line.agent) ?? text(line.agent_id)
  const message = text(line.message) ?? text(line.prompt)
  if (!dispatch || !agent || !message) return
  // The status now, so that a stale `completed` is not taken for this run's end.
  let listed: any[] = []
  try {
    listed = await fx.agent.list()
  } catch {
    listed = []
  }
  const startStatus = listed.find((a: any) => a.id === agent)?.status
  const previous = store.agentDispatch.get(agent)
  open(store, child, openDispatch(dispatch, agent, startStatus))
  // The agent type is hidden from the model, and a hidden type cannot be
  // resumed: the offer hook lets it through while this call is in flight.
  store.resuming += 1
  let why: string | undefined
  try {
    why = refusal(await fx.tool.call({ tool: 'SendMessage', to: agent, message }))
  } catch (reason) {
    why = failureText(reason)
  } finally {
    store.resuming -= 1
  }
  if (why !== undefined) {
    if (previous === undefined) store.agentDispatch.delete(agent)
    else store.agentDispatch.set(agent, previous)
    failDispatch(fx, store, child, dispatch, agent, why)
    return
  }
  markAgent(store, dispatch, agent)
  const d = store.dispatches.get(dispatch)
  if (d) store.dispatches.set(dispatch, { ...d, isBound: true })
  sendBound(fx, store, child, dispatch, agent)
  startPoll(fx, store)
}

export async function handleStop(fx: Fx, store: Store, line: any) {
  const agent = text(line.agent) ?? text(line.agent_id)
  if (!agent) return
  let why: string | undefined
  try {
    why = refusal(await fx.tool.call({ tool: 'TaskStop', task_id: agent }))
  } catch (reason) {
    why = failureText(reason)
  }
  // Said in the run's timeline: an agent relais asked to stop may still be
  // running, and the person is the one who can stop it.
  if (why !== undefined) await note(fx, store, text(line.run), `TaskStop of agent ${agent} failed: ${why}`)
}

// A failure the plugin met, as a `stderr` entry of the run it names, else
// of every live run, else of the latest run: shown labelled in the pane and
// in the status reply. Only with no run at all is there nowhere to say it.
export async function note(fx: Fx, store: Store, run: string | undefined, message: string) {
  const now = await fx.clock.now()
  const keys = Object.keys(store.models)
  const live = keys.filter(key => isLive(store.models[key]))
  // A named run; else every live one; else the latest (a verdict waits
  // only once its run has ended).
  const runs = run && store.models[run] ? [run] : live.length > 0 ? live : keys.slice(-1)
  for (const key of runs) {
    store.models = { ...store.models, [key]: applyEvent(store.models[key], { kind: 'stderr', text: `relais plugin: ${message}` }, now) }
  }
  store.isDirty = true
}

// The pane's agent rows carry the agent id once it is known.
function markAgent(store: Store, dispatch: string, agent: string, type?: string) {
  for (const model of Object.values(store.models)) {
    const row = model.agents.find(a => a.dispatch === dispatch)
    if (row) {
      row.agentId = agent
      if (type !== undefined) row.type = type
    }
  }
  store.isDirty = true
}

// The pane's agent rows follow the list's status.
function showStatus(store: Store, listed: any[]) {
  for (const model of Object.values(store.models)) {
    for (const row of model.agents) {
      const seen = row.agentId ? listed.find((a: any) => a.id === row.agentId) : undefined
      if (seen && seen.status !== row.status) {
        row.status = seen.status
        store.isDirty = true
        markTranscript(store, row.agentId)
      }
    }
  }
}

// The shown agent's transcript is read again on the next tick.
export function markTranscript(store: Store, agent: string | undefined) {
  if (agent !== undefined && store.shown.kind === 'agent' && store.shown.agentId === agent) {
    store.transcriptDirty = true
  }
}

// What a tool call of a relais agent is, in one line: the tool and the
// argument that names what it touches.
const ACTIVITY_ARGS = ['command', 'file_path', 'notebook_path', 'pattern', 'path', 'url', 'query', 'description']

export function activityText(e: any): string {
  const arg = ACTIVITY_ARGS.map(key => e?.[key]).find(value => typeof value === 'string' && value.length > 0)
  const tool = String(e?.tool ?? 'tool')
  return arg ? `${tool} ${arg.replace(/\s+/g, ' ').trim()}` : tool
}

// A tool call of an agent a dispatch owns joins its run's current step, so
// the pane shows the worker working rather than only that it was started.
export function onAgentTool(store: Store, e: any, now: number) {
  const agent = typeof e?.agentId === 'string' ? e.agentId : undefined
  if (!agent || !store.agentDispatch.has(agent)) return
  markTranscript(store, agent)
  for (const [key, model] of Object.entries(store.models)) {
    if (!isLive(model) || !model.agents.some(a => a.agentId === agent)) continue
    store.models = { ...store.models, [key]: applyEvent(model, { kind: 'activity', text: activityText(e) }, now) }
    store.isDirty = true
  }
}

// `turn.complete` of an agent a dispatch owns: its usage joins the total.
export function onTurn(fx: Fx, store: Store, e: any) {
  const dispatch = store.agentDispatch.get(e.agentId)
  const d = dispatch ? store.dispatches.get(dispatch) : undefined
  if (!d || d.isStopped) return
  store.dispatches.set(d.id, withTurn(d, { usage: e.usage, answer: e.answer }))
  detach(check(fx, store))
}

// One look at the agent list: every open dispatch that ended is reported, once.
export async function check(fx: Fx, store: Store) {
  const open = [...store.dispatches.values()].filter(d => !d.isStopped)
  if (open.length === 0) return
  let listed: any[]
  try {
    listed = await fx.agent.list()
  } catch (reason) {
    // Retried on the next poll; said once per streak of failures.
    if (!store.isListFailing) await note(fx, store, undefined, `the agent list could not be read: ${failureText(reason)}`)
    store.isListFailing = true
    return
  }
  store.isListFailing = false
  showStatus(store, listed)
  for (const d of open) {
    // Re-read: a turn may have landed while the list was awaited.
    const current = store.dispatches.get(d.id)
    if (!current || current.isStopped) continue
    const seen = listed.find((a: any) => a.id === current.agent)
    const { dispatch, ended } = observe(current, seen && { id: seen.id, status: seen.status })
    if (ended === undefined) {
      store.dispatches.set(d.id, dispatch)
      continue
    }
    const stopped = { ...dispatch, isStopped: true }
    store.dispatches.set(d.id, stopped)
    const child = store.dispatchChild.get(d.id)
    if (child) sendStopped(fx, store, child, d.id, stoppedPayload(stopped, ended))
  }
}

// Every 2 s while a dispatch is open; the timer is cleared when none is.
function startPoll(fx: Fx, store: Store) {
  if (store.poll || store.closed) return
  store.poll = every(fx, store, POLL_MS, () => {
    detach(
      check(fx, store).then(() => {
        const isOpen = [...store.dispatches.values()].some(d => !d.isStopped)
        if (!isOpen && store.poll) {
          store.poll.cancel()
          store.poll = undefined
        }
      }),
    )
  })
}
