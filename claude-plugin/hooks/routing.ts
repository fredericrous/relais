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
  classificationRecord,
  classifierPrompt,
  clip,
  decideMain,
  decideSpawn,
  decisionOf,
  decisionRecord,
  DEFAULT_MODEL_IDS,
  escalate,
  evidenceOf,
  gateChecks,
  hash,
  mayWrite,
  repositoryName,
  unmatchedChecksOf,
  withUnmatchedChecks,
  modeEffective,
  newTask,
  onEvidence,
  outcomeOf,
  parseClassification,
  parseGateStamp,
  parseRouterState,
  reassessRecord,
  rerun,
  revertedPaths,
  revertRecord,
  routeStatus,
  spawnPrompt,
  taskOutcomeRecord,
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
// The amont gate stamp is read at task end, and never holds it up longer.
const GATE_TIMEOUT_MS = 1000
const GATE_REPOSITORIES = 5
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
    await endTask(fx, store, r.task, await fx.clock.now())
    r.task = undefined
  }
  r.isTaskEndRequested = false
  detach(flushObservations(fx, store))
  if (!r.state && !r.isStale) return
  const seq = ++r.seq
  const promise = classifyPrompt(fx, store, text, seq).catch(() => undefined)
  r.pendingRoute = { seq, promise }
}

async function classifyPrompt(fx: Fx, store: Store, text: string, seq: number): Promise<Classified | undefined> {
  const r = store.router
  const session = await fx.session.id()
  if (session !== r.session || r.isStale) {
    r.session = session
    await refresh(fx, store)
  }
  if (!r.state || r.state.mode === 'off') return undefined
  const [sessionModel, reply] = await Promise.all([
    Promise.resolve(fx.session.model()).catch(() => undefined),
    classify(fx, store, classifierPrompt(text, r.task), 'prompt', seq),
  ])
  return { ...reply, text, sessionModel }
}

const classifierModel = (store: Store) => {
  const tiers = store.router.state?.tiers
  const haiku = tiers ? TIERS.map(t => tiers[t].model).find(m => m.includes('haiku')) : undefined
  return haiku ?? DEFAULT_MODEL_IDS.haiku
}

async function complete(fx: Fx, store: Store, prompt: string) {
  const model = classifierModel(store)
  // A plain string, never marked for the prompt cache: the mark wrote the
  // cache on nearly every call and was almost never read back.
  const reply = await fx.model.complete({
    model,
    system: CLASSIFIER_SYSTEM,
    prompt,
    maxTokens: CLASSIFIER_MAX_TOKENS,
    effort: 'low',
    timeoutMs: CLASSIFIER_TIMEOUT_MS,
  })
  return { ok: reply?.isAnswered === true, text: reply?.text, usage: reply?.usage, model, at: await fx.clock.now() }
}

// One classifier call, parsed, and kept for training whatever came of it
// (an unparsed reply is a record too). Kept here, not where the decision is
// taken, so a reply that lands after the turn stopped waiting is kept as
// well. Fails open: the record is a side effect and never costs the call.
async function classify(fx: Fx, store: Store, prompt: string, purpose: 'prompt' | 'spawn', seq: number) {
  const started = await fx.clock.now()
  const reply = await complete(fx, store, prompt)
  const cls = reply.ok ? parseClassification(reply.text) : undefined
  try {
    queue(
      store,
      classificationRecord({
        callId: `classifier:${reply.at}:${seq}:${purpose}`,
        purpose,
        model: reply.model,
        text: prompt,
        cls,
        latencyMs: reply.at - started,
        now: reply.at,
      }),
    )
  } catch {
    // The call's answer is what routing needs; the record is not.
  }
  return { ...reply, cls }
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
    session: r.session,
    now,
  })
  if (out.ended) detach(endTask(fx, store, out.ended, now))
  if (out.relation === 'new_task') r.forceNew = false
  r.task = out.task
  r.route = { turnId: e.turnId, decision: out.decision }
  if (out.task) {
    queueClassifierUsage(store, out.task.id, classified)
    for (const record of out.reassess) queue(store, reassessRecord(out.task, record, now))
    if (out.relation === 'correction') detach(queueCorrected(fx, store, out.task))
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
          tableVersion: state.capability_table.version,
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
  if (r.task) {
    r.task = { ...r.task, subagentTasks: r.task.subagentTasks.map(s => (s.taskId === task.id ? { ...s, agentId } : s)) }
  }
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
  let classified: Awaited<ReturnType<typeof classify>> | undefined
  let cls: Classification | undefined
  if (!state.pins[type]) {
    const got = await within(fx, classify(fx, store, spawnPrompt(type, description, prompt), 'spawn', ++r.seq).catch(() => undefined), waitFor(remainingMs))
    if (got !== TIMEOUT && got) {
      classified = got
      cls = got.cls
    }
  }
  // The subagent task's id is minted before the spawn: the exploration draw
  // is keyed by it, so the ledger can recompute the draw from the record.
  const taskId = `task-${await fx.clock.now()}-${++r.seq}`
  const spawn = decideSpawn({ state, type, cls, personModel: r.task?.subagentModel ?? null, session: r.session, taskId })
  const applied = !!spawn.decision?.override && isApplying(r)
  const result = await next(applied && spawn.decision ? { ...e, model: spawn.decision.model } : e)
  // The subagent is started: nothing below may throw, or the hook's error
  // handler would see a failure after `next` and the spawn could repeat.
  try {
    await afterSpawn(fx, store, { state, spawn, applied, classified, description, prompt, result, taskId })
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
    classified: Awaited<ReturnType<typeof classify>> | undefined
    description: string
    prompt: string
    result: any
    taskId: string
  },
) {
  const r = store.router
  const { state, spawn, applied, classified, description, prompt, result, taskId } = input
  const now = await fx.clock.now()
  const agentId: string | undefined = typeof result?.agentId === 'string' ? result.agentId : undefined
  let owner: TaskState | undefined = r.task
  // A subagent the router did not create edits files the hook never sees.
  if (!(spawn.isRouted && spawn.decision && spawn.class) && r.task) r.task = { ...r.task, maybeWrote: true }
  if (spawn.isRouted && spawn.decision && spawn.class) {
    const task = newTask(taskId, agentId ?? null, `${description}\n${prompt}`, spawn.class, spawn.decision.tier, now, r.task?.id)
    task.lastReason = 'table'
    // A drawn spawn keeps its lowered floor: its own re-runs may raise the
    // tier to one step under the table's, never back to it.
    task.exploredDown = !!spawn.decision.exploration?.explored
    if (agentId) r.subtasks[agentId] = task
    else keepUnkeyed(r, description, prompt, task)
    owner = task
    // The parent's end amends this task's outcome (`task_outcome`).
    if (r.task) r.task = { ...r.task, subagentTasks: [...r.task.subagentTasks, { taskId: task.id, agentId: agentId ?? null }].slice(-20) }
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
        tableVersion: state.capability_table.version,
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
  const wrote = String(e.tool) === 'Bash' && typeof e.command === 'string' && mayWrite(e.command)
  const noted = wrote && !task.maybeWrote ? { ...task, maybeWrote: true } : task
  if (noted !== task) {
    if (agentId) r.subtasks[agentId] = noted
    else r.task = noted
  }
  const unmatched = String(e.tool) === 'Bash' && typeof e.command === 'string' && result?.deny === undefined ? unmatchedChecksOf(e.command, state.checks) : []
  const counted = unmatched.length > 0 ? withUnmatchedChecks(noted, unmatched) : noted
  if (counted !== noted) {
    if (agentId) r.subtasks[agentId] = counted
    else r.task = counted
    resolveRepository(fx, store).catch(() => {}) // best effort: no unmatched entry is sent until the git root is known
  }
  const evidence = evidenceOf(String(e.tool), e, result, state.checks, state.check_kinds)
  if (!evidence) return
  const out = onEvidence(state, counted, evidence, r.cwd)
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
  await queueCorrected(fx, store, out.task)
  refreshRoute(store)
  const to = decisionOf(r.state, out.task, 'recovery')
  return `Flagged: this task counts as corrected, and its next request runs at ${to.tier} (${to.model}${to.effort ? `, effort ${to.effort}` : ''})${modeOf(r) === 'on' ? '' : ` once routing is on (now ${modeOf(r)})`}.`
}

// --- turn.complete, session.end -----------------------------------------------------------

export function onTurnComplete(fx: Fx, store: Store, e: any, now: number) {
  const r = store.router
  if (e.agentId) {
    const task = r.subtasks[e.agentId]
    if (!task) return
    delete r.subtasks[e.agentId]
    if (e.isAborted && !task.inferred.includes('aborted')) task.inferred.push('aborted')
    detach(endTask(fx, store, task, now))
    return
  }
  if (e.isAborted && r.task && !r.task.inferred.includes('aborted')) r.task.inferred.push('aborted')
}

// A correction is a fact the moment it happens: the task is recorded as
// `corrected` then, not only when it ends, because a session that dies
// before its end (a crash, a kill) would leave the reassessment in the
// ledger with no task for it to belong to. The end's record, later,
// carries the outcome at the end; relais keeps the first one it gets.
// The repository name is resolved first: a name guessed from a subdirectory
// would be stored with the unmatched checks.
async function queueCorrected(fx: Fx, store: Store, task: TaskState) {
  const repository = await resolveRepository(fx, store).catch(() => undefined)
  queue(store, taskRecord(task, null, store.router.cwd, repository))
}

// The task ends: the amont stamp on the trees it edited is read (bounded,
// failing open) and folded in as checks, the task is recorded, and each
// subagent task it spawned is amended with its outcome.
async function endTask(fx: Fx, store: Store, task: TaskState, now: number) {
  const gate = await gateEvidence(fx, store, task, now).catch(() => [])
  const ended: TaskState =
    gate.length === 0 ? task : { ...task, verifications: [...task.verifications, ...gate].slice(-20) }
  const repository = await resolveRepository(fx, store).catch(() => undefined)
  queue(store, taskRecord(ended, now, store.router.cwd, repository))
  const outcome = outcomeOf(ended)
  if (outcome === 'unknown' || outcome === 'answered') return
  for (const sub of ended.subagentTasks ?? []) {
    if (sub.agentId) queue(store, taskOutcomeRecord(sub, outcome, now))
  }
}

// The basename of the session directory's git root, asked once per directory;
// undefined (and nothing cached) when git cannot say.
async function resolveRepository(fx: Fx, store: Store): Promise<string | undefined> {
  const router = store.router
  const cwd = router.cwd
  if (!cwd) return undefined
  if (router.repository?.cwd === cwd) return router.repository.name
  const toplevelRun = await fx.process.run(['git', 'rev-parse', '--show-toplevel'], { cwd, timeoutMs: GATE_TIMEOUT_MS }).catch(() => undefined)
  const toplevel = toplevelRun?.exitCode === 0 ? String(toplevelRun.stdout ?? '').trim() : ''
  if (toplevel === '') return undefined
  const name = repositoryName(toplevel)
  router.repository = { cwd, name }
  return name
}

// The directory a path lies in, the session's cwd for a bare name.
function directoryOf(file: string, cwd: string | undefined): string | undefined {
  const absolute = file.startsWith('/') ? file : cwd ? `${cwd.replace(/\/+$/, '')}/${file}` : undefined
  if (!absolute) return undefined
  const at = absolute.lastIndexOf('/')
  return at <= 0 ? '/' : absolute.slice(0, at)
}

// The gate checks of the repositories the task edited files in, or none
// when anything is slow or fails.
async function gateEvidence(fx: Fx, store: Store, task: TaskState, now: number): Promise<{ command: string; kind: 'gate'; passed: boolean }[]> {
  if (task.files.length === 0) return []
  const read = readGateChecks(fx, store, task, now)
  read.catch(() => {})
  const got = await within(fx, read, GATE_TIMEOUT_MS)
  return got === TIMEOUT ? [] : got
}

async function readGateChecks(fx: Fx, store: Store, task: TaskState, now: number) {
  const cwd = store.router.cwd
  const directories = [...new Set(task.files.map(file => directoryOf(file, cwd)).filter((d): d is string => !!d))]
  const toplevels = await Promise.all(
    directories.map(async dir => {
      const result = await fx.process.run(['git', 'rev-parse', '--show-toplevel'], { cwd: dir, timeoutMs: GATE_TIMEOUT_MS }).catch(() => undefined)
      return result?.exitCode === 0 ? String(result.stdout ?? '').trim() : ''
    }),
  )
  const repositories = [...new Set(toplevels.filter(t => t !== ''))].slice(0, GATE_REPOSITORIES)
  const stamps = await Promise.all(
    repositories.map(async repository => {
      const result = await fx.process
        .run(['relais', 'native', 'gate-stamp', '--cwd', repository], { cwd: repository, timeoutMs: GATE_TIMEOUT_MS })
        .catch(() => undefined)
      return result?.exitCode === 0 ? parseGateStamp(result.stdout) : undefined
    }),
  )
  return stamps.flatMap(stamp => (stamp ? gateChecks(stamp, task.startedAt, now) : [])).map(c => ({ command: c.name, kind: c.kind, passed: c.passed }))
}

// A relais run or replay was launched from the task: it may write files the
// hook never sees.
export function noteRelaisLaunch(store: Store) {
  const r = store.router
  if (r.task) r.task = { ...r.task, maybeWrote: true }
}

// A relais run ended: its acceptance verifies the task, which ends at the
// next prompt.
export function onRunDone(store: Store, outcome: string) {
  const r = store.router
  if (!r.task) return
  r.task = { ...r.task, maybeWrote: true }
  if (outcome === 'accepted') r.task = { ...r.task, relaisAccepted: true }
  r.isTaskEndRequested = true
}

// Every reason: the open tasks are recorded and the batch sent. After a
// /clear or a resume the module goes on under another session: a new task.
export async function onSessionEnd(fx: Fx, store: Store, reason: string, now: number) {
  const r = store.router
  // The subagents first, so the parent's outcome amends the ones still open.
  await Promise.all(Object.values(r.subtasks).map(task => endTask(fx, store, task, now)))
  if (r.task) await endTask(fx, store, r.task, now)
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
  const understandsAnswered = await supportsAnswered(fx, store)
  const bySession = new Map<string, unknown[]>()
  for (const { session, record } of batch) {
    const sent = understandsAnswered ? record : withoutAnswered(record)
    bySession.set(session, [...(bySession.get(session) ?? []), sent])
  }
  for (const [session, records] of bySession) {
    await observe(fx, store, { schema: 1, session, records }, 0, options.isFinal === true)
  }
}

// The first relais release that accepts the `answered` outcome.
const ANSWERED_MIN_VERSION: [number, number] = [0, 37]

// Whether the installed relais accepts `answered`, asked once per session
// through `relais --version`. A failed or unreadable answer is cached as "no"
// for the session (docs/router-protocol.md), so the batch goes in the v0.35
// form and the probe is not repeated.
async function supportsAnswered(fx: Fx, store: Store): Promise<boolean> {
  const router = store.router
  if (router.supportsAnswered !== undefined) return router.supportsAnswered
  let versionText = ''
  try {
    const versionRun = await fx.process.run(['relais', '--version'], { timeoutMs: OBSERVE_TIMEOUT_MS })
    if (versionRun?.exitCode === 0) versionText = String(versionRun.stdout ?? '')
  } catch {
    // An unreadable version is cached as "no" below.
  }
  const found = /(\d+)\.(\d+)\.(\d+)/.exec(versionText)
  const [major, minor] = found ? [Number(found[1]), Number(found[2])] : [0, 0]
  router.supportsAnswered = found !== null && (major > ANSWERED_MIN_VERSION[0] || (major === ANSWERED_MIN_VERSION[0] && minor >= ANSWERED_MIN_VERSION[1]))
  return router.supportsAnswered
}

// The v0.35 form of a record: `answered` and the parent and unmatched-check
// fields did not exist (relais refuses unknown fields), so they are dropped.
function withoutAnswered(record: any): unknown {
  if (!record || typeof record !== 'object') return record
  const out = { ...record }
  delete out.parent_task_id
  delete out.parent_link_recorded
  delete out.unmatched_checks
  if (out.outcome === 'answered') out.outcome = 'unknown'
  if (out.completed_at_end === 'answered') out.completed_at_end = 'unknown'
  return out
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
    return `Session routing is off: no classifier call and no model switch, until you run /relais-routing again. Mode now: ${modeOf(r)}.`
  }
  const answer = await ask(fx, envelopeQuestion(r.state), [NOT_NOW, ENVELOPE_YES])
  if (answer !== ENVELOPE_YES) return 'Session routing was not granted. Nothing was written.'
  const result = await run(['relais', 'native', 'router-envelope', '--by', by, '--epsilon-max', ENVELOPE_EPSILON, '--source', 'plugin-ask'])
  if (result?.exitCode !== 0) return `relais could not record the envelope (exit ${result?.exitCode}): ${failed(result)}. Nothing was granted.`
  await refresh(fx, store)
  return `Session routing granted. Mode now: ${modeOf(r)}. Turn it off with /relais-routing off.`
}
