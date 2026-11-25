// relais's requests to spawn, continue and stop agents, and what comes back:
// each turn's usage, and the one `stopped` a dispatch ends with.

import type { Fx } from './fx.ts'
import { type Dispatch, ENDED, type Listed, observe, openDispatch, STOP_CONFIRM_MS, stoppedPayload, usagePayload, withTurn } from './dispatches.ts'
import { sendBound, sendStopped, sendUsage } from './callbacks.ts'
import { withinBound } from './events.ts'
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

// relais names the agent when it saw it bound; otherwise only the dispatch,
// and the agent is the one this plugin spawned for it (relais#408: a lost
// plugin's `bound` call is what failed). A child's lines are handled in
// order, so the stop comes after its dispatch's spawn has been handled.
export async function handleStop(fx: Fx, store: Store, line: any) {
  const dispatch = text(line.dispatch)
  const named = text(line.agent) ?? text(line.agent_id)
  const known = dispatch ? store.dispatches.get(dispatch) : undefined
  // A dispatch-only stop for an agent that already ended has nothing to stop.
  if (!named && known?.isStopped) return
  const agent = named ?? (known?.agent || undefined)
  if (!agent) return
  await stopAgent(fx, store, text(line.run), agent)
}

// What a bounded `TaskStop` came to. `late` is a call with no answer within
// STOP_CONFIRM_MS: the wait ends, but the call is not cancelled, so the stop
// may still take.
type StopAnswer = { kind: 'done' } | { kind: 'late'; why: string } | { kind: 'refused'; why: string }

// `ms` is what is left of the STOP_CONFIRM_MS window; the text names the
// window, not the remainder, which can be near 0 after a slow list.
async function taskStop(fx: Fx, agent: string, ms = STOP_CONFIRM_MS): Promise<StopAnswer> {
  const settled = await withinBound(fx, ms, () => fx.tool.call({ tool: 'TaskStop', task_id: agent }))
  if (settled.kind === 'late') {
    const why = ms === STOP_CONFIRM_MS ? `no answer in ${STOP_CONFIRM_MS / 1000} s` : `no answer within the ${STOP_CONFIRM_MS / 1000} s window`
    return { kind: 'late', why }
  }
  const why = settled.kind === 'ok' ? refusal(settled.value) : failureText(settled.reason)
  return why === undefined ? { kind: 'done' } : { kind: 'refused', why }
}

async function stopAgent(fx: Fx, store: Store, run: string | undefined, agent: string, ms = STOP_CONFIRM_MS): Promise<StopAnswer> {
  const answer = await taskStop(fx, agent, ms)
  // Said in the run's timeline: an agent relais asked to stop may still be
  // running, and the person is the one who can stop it.
  if (answer.kind === 'late') await note(fx, store, run, `TaskStop of agent ${agent} had ${answer.why}; it may still take.`)
  if (answer.kind === 'refused') await note(fx, store, run, `TaskStop of agent ${agent} failed: ${answer.why}`)
  return answer
}

// The run whose dispatch the agent is bound to now, if this plugin knows it.
export function runOfAgent(store: Store, agent: string): string | undefined {
  const dispatch = store.agentDispatch.get(agent)
  return dispatch === undefined ? undefined : store.dispatchChild.get(dispatch)?.key
}

// A cancel of a run that has ended stops the agents its dispatches bound
// that still run (f24): a worker can outlive its driver, and the session
// has no other way to stop it. An agent rebound to another run's dispatch
// is that run's, and kept. One line per agent, for the cancel's reply.
// The list read and the stops share one STOP_CONFIRM_MS window.
export async function stopAgentsOf(fx: Fx, store: Store, run: string): Promise<string[]> {
  const agents = new Set<string>()
  for (const [id, child] of store.dispatchChild) {
    const agent = child.key === run ? store.dispatches.get(id)?.agent : undefined
    if (agent && runOfAgent(store, agent) === run) agents.add(agent)
  }
  if (agents.size === 0) {
    return [`no agent of run ${run} is known to this plugin; if one still runs, ask the person to stop it in Claude Code`]
  }
  // Bounded too: a list that never answers must not hold the cancel.
  const started = await fx.clock.now()
  const read = await withinBound(fx, STOP_CONFIRM_MS, () => fx.agent.list())
  if (read.kind !== 'ok') {
    const why = read.kind === 'late' ? `no answer in ${STOP_CONFIRM_MS / 1000} s` : failureText(read.reason)
    return [`the agent list could not be read (${why}); no agent of run ${run} was stopped`]
  }
  const listed: readonly Listed[] = read.value
  // What is left of the window, read before the ownership check below.
  const left = Math.max(0, STOP_CONFIRM_MS - ((await fx.clock.now()) - started))
  // Ownership is checked again: a `continue` may have rebound the agent to
  // another run while the list was awaited. No await from here to the calls.
  const live = [...agents].filter(agent => {
    const seen = listed.find(a => a.id === agent)
    return seen !== undefined && !ENDED.includes(seen.status)
  })
  const running = live.filter(agent => runOfAgent(store, agent) === run)
  const moved = live.filter(agent => !running.includes(agent)).map(agent => `agent ${agent} now belongs to another run; not stopped`)
  if (live.length === 0) return [`no agent of run ${run} is still running`]
  // In parallel, with what is left of the window: one bound in all.
  const answers = await Promise.all(running.map(agent => stopAgent(fx, store, run, agent, left)))
  return [...moved, ...answers.map((answer, i) => {
    const agent = running[i]
    // The call went through; the agent's end is not listed again here.
    if (answer.kind === 'done') return `stop sent to agent ${agent}`
    if (answer.kind === 'refused') return `could not stop agent ${agent}: ${answer.why}`
    return `agent ${agent}: TaskStop had ${answer.why}; it may still take`
  })]
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

// The ids that name this turn's responses, for relais's dedup: the turn's
// own list when it carries one, else the turn id.
function messageIdsOf(e: any): string[] {
  if (Array.isArray(e?.messageIds)) return e.messageIds.filter((id: unknown): id is string => typeof id === 'string')
  return typeof e?.turnId === 'string' ? [e.turnId] : []
}

// `turn.complete` of an agent a dispatch owns: its usage joins the total.
export function onTurn(fx: Fx, store: Store, e: any) {
  const dispatch = store.agentDispatch.get(e.agentId)
  const d = dispatch ? store.dispatches.get(dispatch) : undefined
  if (!d || d.isStopped) return
  const turned = withTurn(d, {
    usage: e.usage,
    answer: e.answer,
    reason: typeof e.reason === 'string' ? e.reason : undefined,
    isAborted: e.isAborted === true,
  })
  store.dispatches.set(d.id, turned)
  // The cost is booked every turn, not only at the agent's end: an agent
  // that never stops would otherwise book nothing.
  const child = store.dispatchChild.get(d.id)
  if (child) sendUsage(fx, store, child, d.id, usagePayload(turned, messageIdsOf(e)))
  detach(check(fx, store))
}

// One look at the agent list: every open dispatch that ended is reported, once.
export async function check(fx: Fx, store: Store) {
  const open = [...store.dispatches.values()].filter(d => !d.isStopped)
  if (open.length === 0) return
  // Taken before the call: a look is ordered by when it was asked for, not by the clock.
  store.listSeq += 1
  const look = { now: await fx.clock.now(), seq: store.listSeq }
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
    const { dispatch, ended, stop, isUnconfirmed } = observe(current, seen && { id: seen.id, status: seen.status }, look)
    if (ended === undefined) {
      // `isEnding` is written before any await: a poll and a turn's check overlap.
      store.dispatches.set(d.id, dispatch)
      // Detached: a stop held by the engine must not hold the other dispatches' ends.
      if (stop) detach(stopAnswered(fx, store, dispatch))
      if (isUnconfirmed) {
        await note(fx, store, undefined, `agent ${dispatch.agent} still runs ${STOP_CONFIRM_MS / 1000} s after its stop; it ends with its shell or at the wall time. To end it now, stop it in Claude Code.`)
      }
      continue
    }
    const stopped = { ...dispatch, isStopped: true }
    store.dispatches.set(d.id, stopped)
    const child = store.dispatchChild.get(d.id)
    if (child) sendStopped(fx, store, child, d.id, stoppedPayload(stopped, ended))
  }
}

// `TaskStop` of an agent that answered but is held open by its background work.
// A refusal is said in the timeline and the agent is not stopped again; an
// answer starts the wait for a look, ordered after it, that shows it gone.
// A late call is said too, but the dispatch stays `isEnding`: the stop may
// still take, and a later `killed` is then this stop's, its answer kept. Its
// note stands for the 30 s note, which is not written again.
// A new answer turn that lands while the call is out clears `isEnding`, and
// the stop is then not recorded: that agent ends as today. Accepted: a
// stopped agent's last turn is aborted, not an answer.
async function stopAnswered(fx: Fx, store: Store, dispatch: Dispatch) {
  const answer = await taskStop(fx, dispatch.agent)
  const sentAt = await fx.clock.now()
  const current = store.dispatches.get(dispatch.id)
  if (current && !current.isStopped && current.isEnding) {
    const sent = { ...current, stopSeq: store.listSeq, stopSentAt: sentAt }
    store.dispatches.set(
      dispatch.id,
      answer.kind === 'done'
        ? sent
        : answer.kind === 'late'
          ? { ...sent, isNoted: true }
          : { ...current, isEnding: false, stopFailed: true },
    )
  }
  if (answer.kind === 'late') {
    await note(fx, store, undefined, `the stop of agent ${dispatch.agent} after its answer had ${answer.why}; it may still take, else the run ends when its background work ends or at the wall time.`)
  }
  if (answer.kind === 'refused') {
    await note(fx, store, undefined, `could not stop agent ${dispatch.agent} after its answer (${answer.why}); the run ends when its background work ends or at the wall time.`)
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
