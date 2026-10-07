// The session router's effects: router-state, the classifier, pinning a
// turn's decision, recovery from tool results, subagent routing, the
// observations and the person's two commands. The decisions themselves
// are router.ts's (pure). Everything here fails open: a hook that calls in
// catches, and a failure leaves the request as it was.

import type { Fx } from './fx.ts'
import { note } from './agents.ts'
import { escapeDisplay, NOT_NOW } from './consent.ts'
import {
  CLASSIFIER_MAX_TOKENS,
  CLASSIFIER_SYSTEM,
  CLASSIFIER_TIMEOUT_MS,
  classifierPrompt,
  clip,
  decideMain,
  decideSpawn,
  decisionOf,
  decisionRecord,
  DEFAULT_MODEL_IDS,
  escalate,
  evidenceOf,
  hash,
  modeEffective,
  newTask,
  onEvidence,
  parseClassification,
  parseRouterState,
  reassessRecord,
  rerun,
  revertedPaths,
  revertRecord,
  routeStatus,
  spawnPrompt,
  taskRecord,
  TIERS,
  usageRecord,
  type Classification,
  type Decision,
  type Mode,
  type RouterMemory,
  type RouterState,
  type TaskState,
} from './router.ts'
import { after, detach, type Store } from './store.ts'
import { markDirty } from './ui.ts'

const STATE_TIMEOUT_MS = 10_000
const OBSERVE_TIMEOUT_MS = 20_000
// The first request of a turn waits this long for the classifier at most,
// and leaves 500 ms of the hook's own budget.
const PIN_WAIT_MS = 2000
const BUDGET_MARGIN_MS = 500
const OBSERVE_BACKOFF_MS = [1000, 2000, 4000, 8000, 15000]
export const OBSERVE_RETRIES = 5
const ENVELOPE_EPSILON = '0.1'
export const ENVELOPE_YES = 'Allow session routing'

const TIMEOUT = Symbol('timeout')

// What the prompt's classifier call resolves to.
export type Classified = { cls: Classification | undefined; text: string; sessionModel: string | undefined; usage: any; model: string; at: number }

const modeOf = (r: RouterMemory): Mode => modeEffective(r.state)
const isApplying = (r: RouterMemory) => modeOf(r) === 'on' && r.state?.holdout !== true

// Waits for `promise` at most `ms` on the session's clock.
async function within<T>(fx: Fx, promise: Promise<T>, ms: number): Promise<T | typeof TIMEOUT> {
  if (!(ms > 0)) return TIMEOUT
  let timer: { cancel: () => void } | undefined
  const timeout = new Promise<typeof TIMEOUT>(resolve => {
    timer = fx.clock.after(ms, () => resolve(TIMEOUT))
  })
  try {
    return await Promise.race([promise, timeout])
  } finally {
    timer?.cancel()
  }
}

const waitFor = (remainingMs: number) => Math.min(PIN_WAIT_MS, remainingMs - BUDGET_MARGIN_MS)

function queue(store: Store, record: unknown) {
  const r = store.router
  if (!r.session || !r.state) return
  r.queue.push({ session: r.session, record })
}

function setStatus(store: Store, decision: Decision | undefined) {
  const r = store.router
  r.status = routeStatus(decision, modeOf(r), r.state?.holdout === true)
  markDirty(store)
}

// --- router-state -----------------------------------------------------------

export async function loadRouterState(fx: Fx, store: Store) {
  const r = store.router
  try {
    const session = r.session ?? (await fx.session.id())
    r.session = session
    const result = await fx.process.run(['relais', 'native', 'router-state', '--session', session], {
      ...(r.cwd ? { cwd: r.cwd } : {}),
      timeoutMs: STATE_TIMEOUT_MS,
    })
    r.state = result?.exitCode === 0 ? parseRouterState(result.stdout) : undefined
  } catch {
    r.state = undefined
  }
  r.isStale = false
}

// At session.start: the state is read in the background, and the first
// prompt's classifier waits for it.
export function refreshState(fx: Fx, store: Store): Promise<void> {
  store.router.isStale = true
  return refresh(fx, store)
}

function refresh(fx: Fx, store: Store): Promise<void> {
  const r = store.router
  if (!r.refreshing) {
    r.refreshing = loadRouterState(fx, store).finally(() => {
      r.refreshing = undefined
    })
  }
  return r.refreshing
}

// --- prompt.submit ------------------------------------------------------------

// One of the person's own prompts (never a task notification, never a
// message this plugin submitted): the batch so far is sent, and the
// classifier starts, detached, for the turn's first request to pick up.
export async function onPrompt(fx: Fx, store: Store, text: string) {
  const r = store.router
  if (r.isTaskEndRequested && r.task) {
    endTask(store, r.task, await fx.clock.now())
    r.task = undefined
  }
  r.isTaskEndRequested = false
  detach(flushObservations(fx, store))
  if (!r.state && !r.isStale) return
  const seq = ++r.seq
  const promise = classifyPrompt(fx, store, text).catch(() => undefined)
  r.pendingRoute = { seq, promise }
}

async function classifyPrompt(fx: Fx, store: Store, text: string): Promise<Classified | undefined> {
  const r = store.router
  const session = await fx.session.id()
  if (session !== r.session || r.isStale) {
    r.session = session
    await refresh(fx, store)
  }
  if (!r.state || r.state.mode === 'off') return undefined
  const [sessionModel, reply] = await Promise.all([
    Promise.resolve(fx.session.model()).catch(() => undefined),
    complete(fx, store, classifierPrompt(text, r.task)),
  ])
  return { ...reply, text, cls: reply.ok ? parseClassification(reply.text) : undefined, sessionModel }
}

const classifierModel = (store: Store) => {
  const tiers = store.router.state?.tiers
  const haiku = tiers ? TIERS.map(t => tiers[t].model).find(m => m.includes('haiku')) : undefined
  return haiku ?? DEFAULT_MODEL_IDS.haiku
}

async function complete(fx: Fx, store: Store, prompt: string) {
  const model = classifierModel(store)
  const reply = await fx.model.complete({
    model,
    system: [{ text: CLASSIFIER_SYSTEM, cache: true }],
    prompt,
    maxTokens: CLASSIFIER_MAX_TOKENS,
    effort: 'low',
    timeoutMs: CLASSIFIER_TIMEOUT_MS,
  })
  return { ok: reply?.isAnswered === true, text: reply?.text, usage: reply?.usage, model, at: await fx.clock.now() }
}

function queueClassifierUsage(store: Store, taskId: string, c: { usage: any; model: string; at: number } | undefined) {
  if (!c?.usage) return
  const r = store.router
  queue(store, usageRecord({ taskId, turnId: `classifier:${c.at}:${r.seq}`, step: 0, agentId: null, source: 'classifier', model: c.model, usage: c.usage, now: c.at }))
}

// --- turn.step --------------------------------------------------------------------

// The request as it goes down: rewritten only when the mode is on, the
// session is not held out, and the router decided for this loop.
export async function beforeStep(fx: Fx, store: Store, e: any, remainingMs: number): Promise<any> {
  const r = store.router
  if (e.agentId) return beforeSubagentStep(fx, store, e)
  if (r.route?.turnId !== e.turnId) await pin(fx, store, e, remainingMs)
  const decision = r.route?.decision
  if (!decision || !decision.override || !isApplying(r)) return e
  return { ...e, model: decision.model, ...(decision.effort ? { effort: decision.effort } : {}) }
}

// The turn's first request: the classifier raced against the budget, then
// the decision pinned for every later request of the turn.
async function pin(fx: Fx, store: Store, e: any, remainingMs: number) {
  const r = store.router
  const pending = r.pendingRoute
  r.pendingRoute = undefined
  let classified: Classified | undefined
  let timedOut = false
  if (pending) {
    const got = (await within(fx, pending.promise, waitFor(remainingMs))) as Classified | undefined | typeof TIMEOUT
    if (got === TIMEOUT || got === undefined) timedOut = true
    else classified = got
  }
  const state = r.state
  if (!state) {
    r.route = { turnId: e.turnId, decision: undefined }
    return
  }
  const now = await fx.clock.now()
  const out = decideMain({
    state,
    task: r.task,
    cls: classified?.cls,
    timedOut: timedOut || (pending !== undefined && classified?.cls === undefined),
    forceNew: r.forceNew,
    text: classified?.text ?? '',
    sessionModel: classified?.sessionModel ?? e.model,
    lastModel: r.lastModel,
    contextTokens: r.lastContext,
    newTaskId: `task-${now}-${++r.seq}`,
    now,
  })
  if (out.ended) endTask(store, out.ended, now)
  if (out.relation === 'new_task') r.forceNew = false
  r.task = out.task
  r.route = { turnId: e.turnId, decision: out.decision }
  if (out.task) {
    queueClassifierUsage(store, out.task.id, classified)
    for (const record of out.reassess) queue(store, reassessRecord(out.task, record, now))
    if (out.decision && out.relation) {
      queue(
        store,
        decisionRecord({
          task: out.task,
          turnId: e.turnId,
          agentId: null,
          relation: out.relation,
          decision: out.decision,
          mode: modeOf(r),
          holdout: state.holdout,
          applied: out.decision.override && isApplying(r),
          now,
        }),
      )
    }
  } else if (classified) {
    queueClassifierUsage(store, 'unassigned', classified)
  }
  setStatus(store, out.decision)
}

async function beforeSubagentStep(fx: Fx, store: Store, e: any) {
  const r = store.router
  let task = r.subtasks[e.agentId]
  if (!task && Object.keys(r.unkeyed).length > 0) task = await keyUnkeyed(fx, store, e.agentId)
  if (!task || !r.state) return e
  const decision = decisionOf(r.state, task, task.lastReason)
  if (!decision.override || !isApplying(r)) return e
  return { ...e, model: decision.model, ...(decision.effort ? { effort: decision.effort } : {}) }
}

// A subagent whose spawn returned no id, keyed by its description in the
// agent list. Two pending spawns with one description stay unkeyed.
async function keyUnkeyed(fx: Fx, store: Store, agentId: string): Promise<TaskState | undefined> {
  const r = store.router
  let listed: any[] = []
  try {
    listed = await fx.agent.list()
  } catch {
    return undefined
  }
  const description = listed.find(a => a.id === agentId)?.description
  const entry = typeof description === 'string' ? r.unkeyed[description] : undefined
  if (!entry || entry.ambiguous) return undefined
  delete r.unkeyed[description]
  const task = { ...entry.task, agentId }
  r.subtasks[agentId] = task
  return task
}

// What came back: each request's usage, against the loop's task.
export function afterStep(store: Store, e: any, result: any, now: number) {
  const r = store.router
  const usage = result?.usage
  if (!usage || !r.state) return
  // relais's own run agents are accounted by relais's run, never here; any
  // other subagent the router did not route counts against the main task.
  if (e.agentId && !r.subtasks[e.agentId] && store.agentDispatch.has(e.agentId)) return
  const task = e.agentId ? r.subtasks[e.agentId] ?? r.task : r.task
  if (!e.agentId) {
    r.lastModel = typeof usage.model === 'string' ? usage.model : r.lastModel
    r.lastContext = (usage.input_tokens ?? 0) + (usage.cache_read_input_tokens ?? 0) + (usage.cache_creation_input_tokens ?? 0)
  }
  if (!task) return
  queue(
    store,
    usageRecord({
      taskId: task.id,
      turnId: e.turnId ?? null,
      step: e.index ?? 0,
      agentId: e.agentId ?? null,
      source: 'step',
      model: typeof usage.model === 'string' ? usage.model : String(e.model ?? ''),
      usage,
      now,
    }),
  )
}

// --- agent.spawn ------------------------------------------------------------------

// A subagent the model asked for (never one of relais's own: the caller
// passes those by): classified, then routed within the envelope. The
// parent's `model` is a preference; a pin or the person's model is kept.
export async function routeSpawn(fx: Fx, store: Store, e: any, next: any, remainingMs: number) {
  const r = store.router
  const state = r.state
  if (!state || state.mode === 'off') return next(e)
  const type = String(e.subagentType ?? e.subagent_type ?? '')
  const description = String(e.description ?? '')
  const prompt = String(e.prompt ?? '')
  let classified: Awaited<ReturnType<typeof complete>> | undefined
  let cls: Classification | undefined
  if (!state.pins[type]) {
    const got = await within(fx, complete(fx, store, spawnPrompt(type, description, prompt)).catch(() => undefined), waitFor(remainingMs))
    if (got !== TIMEOUT && got) {
      classified = got
      cls = got.ok ? parseClassification(got.text) : undefined
    }
  }
  const spawn = decideSpawn({ state, type, cls, personModel: r.task?.subagentModel ?? null })
  const applied = !!spawn.decision?.override && isApplying(r)
  const result = await next(applied && spawn.decision ? { ...e, model: spawn.decision.model } : e)
  // The subagent is started: nothing below may throw, or the hook's error
  // handler would see a failure after `next` and the spawn could repeat.
  try {
    await afterSpawn(fx, store, { state, spawn, applied, classified, description, prompt, result })
  } catch (reason) {
    await note(fx, store, undefined, `relais router: the spawn's bookkeeping failed: ${String((reason as any)?.message ?? reason).slice(0, 200)}`).catch(() => {})
  }
  return result
}

async function afterSpawn(
  fx: Fx,
  store: Store,
  input: {
    state: RouterState
    spawn: ReturnType<typeof decideSpawn>
    applied: boolean
    classified: Awaited<ReturnType<typeof complete>> | undefined
    description: string
    prompt: string
    result: any
  },
) {
  const r = store.router
  const { state, spawn, applied, classified, description, prompt, result } = input
  const now = await fx.clock.now()
  const agentId: string | undefined = typeof result?.agentId === 'string' ? result.agentId : undefined
  let owner: TaskState | undefined = r.task
  if (spawn.isRouted && spawn.decision && spawn.class) {
    const task = newTask(`task-${now}-${++r.seq}`, agentId ?? null, `${description}\n${prompt}`, spawn.class, spawn.decision.tier, now)
    task.lastReason = 'table'
    if (agentId) r.subtasks[agentId] = task
    else keepUnkeyed(r, description, prompt, task)
    owner = task
  }
  if (classified) queueClassifierUsage(store, owner?.id ?? 'unassigned', classified)
  if (spawn.decision && owner) {
    queue(
      store,
      decisionRecord({
        task: spawn.isRouted ? owner : { ...owner, class: spawn.class ?? owner.class },
        turnId: null,
        agentId: agentId ?? null,
        relation: 'subagent',
        decision: spawn.decision,
        mode: modeOf(r),
        holdout: state.holdout,
        applied,
        now,
      }),
    )
  }
  // The delegation re-runs the main task's table: it may raise, never lower.
  if (r.task) {
    const out = rerun(state, r.task, 'spawn')
    if (agentId) out.task.subagents = [...out.task.subagents, agentId].slice(-20)
    r.task = out.task
    queue(store, reassessRecord(out.task, out.record, now))
    refreshRoute(store)
  }
}

// Spawns without an id, keyed on description and the first 1 KB of the
// prompt: a second one with the same description makes both ambiguous.
function keepUnkeyed(r: RouterMemory, description: string, prompt: string, task: TaskState) {
  const key = `${description}\u0000${hash(clip(prompt, 1024))}`
  const existing = r.unkeyed[description]
  if (existing) {
    existing.ambiguous = true
    return
  }
  r.unkeyed[description] = { key, task, ambiguous: false }
}

// --- tool.call ------------------------------------------------------------------------

// A finished tool call, as evidence for the task its loop belongs to: the
// main session's, or a router-created subagent's (any other is ignored).
export function onToolResult(fx: Fx, store: Store, e: any, result: any, now: number) {
  const r = store.router
  const state = r.state
  if (!state) return
  const agentId: string | undefined = e.agentId
  const task = agentId ? r.subtasks[agentId] : r.task
  if (!task) return
  noteRevert(store, state, task, e, result, now)
  const evidence = evidenceOf(String(e.tool), e, result, state.checks)
  if (!evidence) return
  const out = onEvidence(state, task, evidence, r.cwd)
  if (agentId) r.subtasks[agentId] = out.task
  else r.task = out.task
  if (out.reassess) {
    queue(store, reassessRecord(out.task, out.reassess, now))
    if (!agentId) refreshRoute(store)
  }
}

// A Bash command that succeeded in reverting files (`git revert`, `git
// checkout -- …`, `git restore …`): recorded against the task in progress,
// with the files hashed, so R2 can match it to the task that edited them.
function noteRevert(store: Store, state: RouterState, task: TaskState, e: any, result: any, now: number) {
  if (String(e.tool) !== 'Bash' || e.run_in_background === true) return
  if (!result || result.deny !== undefined || result.isError) return
  const paths = revertedPaths(typeof e.command === 'string' ? e.command : '')
  if (paths === undefined) return
  queue(store, reassessRecord(task, revertRecord(state, task, paths, store.router.cwd), now))
}

// The pinned decision follows the task from the next request on.
function refreshRoute(store: Store) {
  const r = store.router
  if (!r.state || !r.task || !r.route) return
  r.route = { ...r.route, decision: decisionOf(r.state, r.task, r.task.lastReason) }
  setStatus(store, r.route.decision)
}

// --- /relais-flag ---------------------------------------------------------------------

export async function flag(fx: Fx, store: Store): Promise<string> {
  const r = store.router
  if (!r.state || !r.task) return 'No routed task is in progress: nothing to flag.'
  const now = await fx.clock.now()
  const corrected = { ...r.task, corrected: true }
  const out = escalate(r.state, corrected, 'flag')
  r.task = out.task
  queue(store, reassessRecord(out.task, out.record, now))
  refreshRoute(store)
  const to = decisionOf(r.state, out.task, 'recovery')
  return `Flagged: this task counts as corrected, and its next request runs at ${to.tier} (${to.model}${to.effort ? `, effort ${to.effort}` : ''})${modeOf(r) === 'on' ? '' : ` once routing is on (now ${modeOf(r)})`}.`
}

// --- turn.complete, session.end -----------------------------------------------------------

export function onTurnComplete(store: Store, e: any, now: number) {
  const r = store.router
  if (e.agentId) {
    const task = r.subtasks[e.agentId]
    if (!task) return
    delete r.subtasks[e.agentId]
    if (e.isAborted && !task.inferred.includes('aborted')) task.inferred.push('aborted')
    endTask(store, task, now)
    return
  }
  if (e.isAborted && r.task && !r.task.inferred.includes('aborted')) r.task.inferred.push('aborted')
}

function endTask(store: Store, task: TaskState, now: number) {
  queue(store, taskRecord(task, now, store.router.cwd))
}

// A relais run ended: its acceptance verifies the task, which ends at the
// next prompt.
export function onRunDone(store: Store, outcome: string) {
  const r = store.router
  if (!r.task) return
  if (outcome === 'accepted') r.task = { ...r.task, relaisAccepted: true }
  r.isTaskEndRequested = true
}

// Every reason: the open tasks are recorded and the batch sent. After a
// /clear or a resume the module goes on under another session: a new task.
export async function onSessionEnd(fx: Fx, store: Store, reason: string, now: number) {
  const r = store.router
  if (r.task) endTask(store, r.task, now)
  for (const task of Object.values(r.subtasks)) endTask(store, task, now)
  r.task = undefined
  r.subtasks = {}
  r.unkeyed = {}
  r.route = undefined
  r.pendingRoute = undefined
  r.isTaskEndRequested = false
  await flushObservations(fx, store, { isFinal: true })
  if (reason === 'clear' || reason === 'resume') {
    r.forceNew = true
    r.isStale = true
    r.lastModel = undefined
    r.lastContext = 0
    setStatus(store, undefined)
  }
}

// --- router-observe ----------------------------------------------------------------------

// The queue sent as one `relais native router-observe` per session: 0 is
// done, 2 is a bad payload (dropped, noted), anything else is retried at
// most five times, then dropped and noted.
export async function flushObservations(fx: Fx, store: Store, options: { isFinal?: boolean } = {}) {
  const r = store.router
  if (r.queue.length === 0) return
  const batch = r.queue
  r.queue = []
  const bySession = new Map<string, unknown[]>()
  for (const { session, record } of batch) bySession.set(session, [...(bySession.get(session) ?? []), record])
  for (const [session, records] of bySession) {
    await observe(fx, store, { schema: 1, session, records }, 0, options.isFinal === true)
  }
}

async function observe(fx: Fx, store: Store, payload: unknown, attempt: number, isFinal: boolean): Promise<void> {
  let code: number | undefined
  let stderr = ''
  try {
    const result = await fx.process.run(['relais', 'native', 'router-observe'], {
      stdin: JSON.stringify(payload),
      timeoutMs: OBSERVE_TIMEOUT_MS,
      ...(store.router.cwd ? { cwd: store.router.cwd } : {}),
    })
    code = result?.exitCode
    stderr = String(result?.stderr ?? '').trim()
  } catch (reason) {
    stderr = String((reason as any)?.message ?? reason)
  }
  if (code === 0) {
    if (!isFinal) detach(refresh(fx, store))
    return
  }
  if (code === 2) {
    await note(fx, store, undefined, `session routing observations were refused and dropped: ${clip(stderr, 400)}`)
    return
  }
  if (attempt >= OBSERVE_RETRIES || isFinal || store.closed) {
    if (!isFinal) await note(fx, store, undefined, `session routing observations dropped after ${attempt + 1} tries: ${clip(stderr, 400)}`)
    return
  }
  const wait = OBSERVE_BACKOFF_MS[Math.min(attempt, OBSERVE_BACKOFF_MS.length - 1)]
  after(fx, store, wait, () => detach(observe(fx, store, payload, attempt + 1, false)))
}

// --- the person's commands ----------------------------------------------------------------

async function ask(fx: Fx, question: string, options: string[]): Promise<string | undefined> {
  try {
    return await fx.ui.ask(question, options)
  } catch {
    return undefined
  }
}

export function envelopeQuestion(state: RouterMemory['state']): string {
  const tiers = state
    ? TIERS.map(t => `${t} ${escapeDisplay(state.tiers[t].model)}`).join(' · ')
    : 'the tiers relais.toml or machine.toml name'
  return [
    'relais · Allow session routing in your Claude Code sessions?',
    `- route the session's own model and its subagents' by the task, and move up on failure evidence (${tiers});`,
    `- explore one tier lower at ε ≤ ${ENVELOPE_EPSILON} at a task start (R2);`,
    '- switch on a learned adjustment only when it passes every gate (R2).',
    'Turn it off with /relais-routing off, or RELAIS_SESSION_ROUTING=off for one shell.',
  ].join('\n')
}

// `/relais-routing`: routing on (the envelope, after the person's yes), or
// with `off` routing off (the envelope removed, and kept off).
export async function routingCommand(fx: Fx, store: Store, args = ''): Promise<string> {
  const r = store.router
  const session = await fx.session.id()
  const by = `the person in Claude Code session ${session}`
  const run = (argv: string[]) => fx.process.run(argv, { ...(r.cwd ? { cwd: r.cwd } : {}), timeoutMs: STATE_TIMEOUT_MS })
  const failed = (result: any) => clip(String(result?.stderr ?? '').trim(), 600)
  if (args.trim() === 'off') {
    const result = await run(['relais', 'native', 'router-envelope', '--off', '--by', by, '--source', 'plugin-ask'])
    if (result?.exitCode !== 0) return `relais could not turn session routing off (exit ${result?.exitCode}): ${failed(result)}.`
    await refresh(fx, store)
    return `Session routing is off, and stays off until you run /relais-routing again. Mode now: ${modeOf(r)}.`
  }
  const answer = await ask(fx, envelopeQuestion(r.state), [NOT_NOW, ENVELOPE_YES])
  if (answer !== ENVELOPE_YES) return 'Session routing was not granted. Nothing was written.'
  const result = await run(['relais', 'native', 'router-envelope', '--by', by, '--epsilon-max', ENVELOPE_EPSILON, '--source', 'plugin-ask'])
  if (result?.exitCode !== 0) return `relais could not record the envelope (exit ${result?.exitCode}): ${failed(result)}. Nothing was granted.`
  await refresh(fx, store)
  return `Session routing granted. Mode now: ${modeOf(r)}. Turn it off with /relais-routing off.`
}
