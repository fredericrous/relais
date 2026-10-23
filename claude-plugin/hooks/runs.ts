// Starting `relais run --protocol` and reading what it says. The child is
// the stream: it lives as long as the loop reading it, and the module's
// unload ends both. The reading itself is the `process.spawn` hook's
// (register.ts), which calls `onChunk`.

import type { Fx } from './fx.ts'
import { handleContinue, handleSpawn, handleStop, note } from './agents.ts'
import { parseProtocolLine, splitLines } from './lines.ts'
import {
  applyEvent,
  applyLine,
  awaitsAPerson,
  awaitsState,
  changedText,
  emptyRun,
  isLive,
  rebuildRuns,
  shortId,
  statusReply,
  statusSummaryEntry,
  type RunModel,
  reconcileStatus,
} from './timeline.ts'
import { sendHello } from './callbacks.ts'
import { onRunDone } from './routing.ts'
import { detach, type Child, type PendingOutcome, type Store } from './store.ts'
import { refreshTranscript } from './transcript.ts'
import { hasQuietWork, tickQuiet } from './quiet-watch.ts'
import {
  announceRun,
  flush,
  markDirty,
  toastDecision,
  toastOutcome,
} from './ui.ts'

const STATUS_TIMEOUT_MS = 10_000
// A reconciling status of one live run: short, because the next poll asks again.
const RECONCILE_TIMEOUT_MS = 5_000
// The status request in flight for each run, per store: a second caller
// shares it instead of starting another.
const reconciling = new WeakMap<object, Map<string, Promise<void>>>()
// `native cancel` waits up to 10 s for the driver's outcome itself.
const CANCEL_TIMEOUT_MS = 20_000
// The freshness check sits on the path to the session's next turn: a
// second is long enough for a read-only ledger lookup, short enough not
// to be felt.
const FRESHEN_TIMEOUT_MS = 1_000
// Runs a status reply without `run` lists at most this many.
const STATUS_PAGE = 10
// The closing line of an outcome that waits on a person: the step that
// settles it (ADR-0004).
export const DECIDE_STEP =
  'Decide it, then record the answer with mcp__relais__decide (approve, revise, review-parts, reject, salvaged, abandon or decided). When the request or the approved plan does not answer it, ask the person and record their answer with on_behalf_of_person and their own words as person_quote.'
// The closing line of an accepted outcome: a later undo is recorded by the
// feedback tool (D23).
export const FEEDBACK_STEP =
  'When you revert or fix this change later, record it with mcp__relais__feedback.'
const MAX_SUMMARY = 4000
const STDERR_TAIL = 5

// What the model does next when a run is refused for a reason it can fix
// with the person: the tool that asks them.
const NEXT_STEP: Record<string, string> = {
  no_policy:
    'This repository has no relais.toml yet. Call mcp__relais__onboard with this cwd: it asks the person which checks to use and whether relais may run them, then call mcp__relais__run again.',
  missing_trust_grant:
    "The person has not allowed this repository's commands yet. Call mcp__relais__trust with this cwd: it shows them the exact commands and asks; then call mcp__relais__run again.",
}

// One tick of the session's timer: sends the verdicts waiting, then writes
// the pane's state if anything changed.
export async function pump(fx: Fx, store: Store) {
  // The engine's `prompt.submit` settles once the prompt's turn starts,
  // which is when the session is idle: behind a long turn it stays pending
  // for minutes. The timer ticks on meanwhile, so a tick that finds a
  // submit in flight only redraws; one that re-entered submitted the same
  // verdict again every 100 ms (4,168 copies of one outcome, 2026-10-09).
  if (!store.isSubmitting) {
    store.isSubmitting = true
    try {
      await submitVerdicts(fx, store)
    } finally {
      store.isSubmitting = false
    }
  }
  await refreshTranscript(fx, store)
  if (hasQuietWork(store)) await tickQuiet(fx, store)
  await flush(fx, store)
}

// A verdict leaves the queue only once submitted: one that fails is
// tried again on the next tick, never lost.
async function submitVerdicts(fx: Fx, store: Store) {
  while (store.verdicts.length > 0) {
    try {
      // Checked here, at the end of the wait in this queue, because no
      // later point is ours: the engine runs a `prompt.submit` through
      // every plugin's hooks but the submitter's.
      const text = await freshen(fx, store, store.verdicts[0])
      store.ownPrompts.add(text)
      try {
        await fx.prompt.submit({ text })
      } finally {
        // The submit settles after every `prompt.submit` hook has run, so
        // a mark still here was never read and never will be: the engine
        // skips the submitter's own hooks. Kept, it would grow by one per
        // outcome for the life of the session.
        store.ownPrompts.delete(text)
      }
    } catch (reason) {
      // Kept for the next tick; said once per streak of failures.
      if (!store.isSubmitFailing) {
        await note(fx, store, undefined, `the outcome message could not be sent yet: ${String((reason as any)?.message ?? reason)}`)
      }
      store.isSubmitFailing = true
      return
    }
    store.isSubmitFailing = false
    // Forgotten only once submitted: a submit that failed is checked
    // again, from the original text, when the next tick retries it.
    const sent = store.verdicts.shift()!
    const pending = store.pendingOutcomes.get(sent)
    if (pending) store.outcomeDeliveredAt.set(pending.run, await fx.clock.now())
    store.pendingOutcomes.delete(sent)
  }
}

// The text to deliver for an outcome message, checked against what the
// ledger holds now. Its decision answered since the run ended rewrites
// it: the original asks the model to act on something already settled
// (2026-10-09: an approved run announced as "yours to decide" ten
// minutes later). Anything else delivers the text unchanged — an outcome
// lost is worse than one stale.
async function freshen(fx: Fx, store: Store, text: string): Promise<string> {
  const pending = store.pendingOutcomes.get(text)
  if (!pending) return text
  const say = async (kind: 'failed' | 'answered', message: string) => {
    if (pending.noted.has(kind)) return
    pending.noted.add(kind)
    await note(fx, store, pending.run, message)
  }
  let current: any
  try {
    const result = await fx.process.run(['relais', 'native', 'status', '--run', pending.run], {
      timeoutMs: FRESHEN_TIMEOUT_MS,
    })
    if (result.exitCode !== 0) {
      const why = String(result.stderr ?? '').trim().slice(0, 200)
      await say('failed', `could not check the outcome is still current (status exited ${result.exitCode}${why ? `: ${why}` : ''}); sent as it was`)
      return text
    }
    current = JSON.parse(String(result.stdout ?? '')).current
  } catch (reason) {
    await say('failed', `could not check the outcome is still current (${String((reason as any)?.message ?? reason).slice(0, 200)}); sent as it was`)
    return text
  }
  if (!current || typeof current !== 'object') {
    // An installed relais older than the plugin: said once, not per outcome.
    if (!store.isCurrentMissingNoted) {
      store.isCurrentMissingNoted = true
      await note(fx, store, pending.run, 'relais native status gives no current state (relais older than the plugin?); outcome messages are sent as they were')
    }
    return text
  }
  const decision = current.decision
  if (!decision || decision.resolution === null || decision.resolution === undefined) return text
  // relais's own output, but read as it comes: a rewrite names only
  // strings, so a shape this plugin does not know sends the text as it was.
  if (typeof current.state !== 'string' || typeof decision.resolution !== 'string') {
    await say('failed', 'could not check the outcome is still current (relais native status answered in a shape this plugin does not read); sent as it was')
    return text
  }
  await say('answered', `answered since it ended (${decision.resolution}): the outcome message says so`)
  return answeredText(pending, decision, current.state)
}

// An outcome message for a run whose decision a person answered since:
// what it ended as, what was answered, and that nothing waits. The
// runner's `detail` and the next step are left out, since both ask for
// the answer already given.
function answeredText(pending: PendingOutcome, decision: any, state: string): string {
  const code = pending.code ? ` (${pending.code})` : ''
  const headline =
    `relais ${pending.kind} ${pending.run} finished: ${pending.outcome}${code}. ` +
    `Since then: ${decision.resolution} by ${decision.actor ?? 'someone'} at ${decision.resolved_at ?? 'an unknown time'}; ` +
    `the run is now ${state}. Nothing is waiting on you for this run.`
  return [headline, ...pending.facts].join('\n')
}

// The run tool's `task`: a contract object, which relais validates and saves
// under the repository's `.relais/tasks/`, or the path of a contract file.
// Prose is refused with what a contract needs.
export async function contractPath(fx: Fx, task: unknown, cwd: string): Promise<{ path: string; deny?: undefined } | { deny: string }> {
  if (typeof task === 'string') {
    if (task.endsWith('.json')) return { path: task }
    return {
      deny: 'relais run takes a task contract, not prose: pass task as an object {schema_version: 1, kind, objective, base_ref, write_scope, acceptance, verification_profile} (see the relais skill).',
    }
  }
  if (!task || typeof task !== 'object') return { deny: 'relais run needs a task contract object.' }
  const saved = await fx.process.run(['relais', 'native', 'contract'], {
    cwd,
    stdin: JSON.stringify(task),
    timeoutMs: STATUS_TIMEOUT_MS,
  })
  if (saved.exitCode !== 0) {
    return { deny: `relais refused the contract: ${String(saved.stderr ?? '').trim().slice(0, 1000)}` }
  }
  return { path: String(saved.stdout ?? '').trim() }
}

const WORKSPACES = ['auto', 'fork', 'in-place', 'isolated']

// The run tool's optional `workspace`, checked here so a typo is refused with
// the four spellings instead of reaching relais as a usage error mid-run.
export function workspaceOf(value: unknown): { value?: string; deny?: string } {
  if (value === undefined || value === null) return {}
  if (typeof value === 'string' && WORKSPACES.includes(value)) return { value }
  return { deny: `relais run: workspace is one of ${WORKSPACES.join(', ')}.` }
}

export async function startRun(fx: Fx, store: Store, input: { task: string; cwd: string; workspace?: string }) {
  const flag = input.workspace ? ['--workspace', input.workspace] : []
  await startChild(fx, store, ['relais', 'run', '--task', input.task, ...flag, '--protocol'], input.cwd)
  // The plugin returns before relais resolves where the run works, so this names only
  // what was asked for. The run's own first event says where it works and, in place,
  // that the worktree is held until the outcome arrives.
  const asked = input.workspace ? ` (workspace: ${input.workspace})` : ''
  return `Started relais run for: ${input.task} (in ${input.cwd})${asked}. It runs in the relais pane and /relais-status; its outcome arrives here as a message when it ends.`
}

// A successor intent a `decide revise` recorded (relais ADR-0009 A5): launched
// as `relais run --intent <id>`, which claims it atomically; a second claim
// is refused `intent_already_claimed` with the run that holds it. Nothing here
// remembers a launch: relais's compare-and-set claim is the only guard, so an
// intent whose successor died is launched again by the next caller.
export async function startIntent(fx: Fx, store: Store, input: { intent: string; cwd: string }) {
  await startChild(fx, store, ['relais', 'run', '--intent', input.intent, '--protocol'], input.cwd)
  return `Started relais successor intent ${input.intent} (in ${input.cwd}). It runs in the relais pane and /relais-status; its outcome arrives here as a message when it ends.`
}

export type PendingIntent = { intent_id: string; repo: string }

// The intents `relais native status --session` lists as launchable: pending
// ones, and claimed ones whose run is gone. A pending intent whose last
// launch was refused carries a `retry_after`; inside it the intent is listed
// for the pane but not launched. Anything else it says is not this
// function's.
export function launchableIntents(text: string, now: number = Date.now()): PendingIntent[] {
  let parsed: any
  try {
    parsed = JSON.parse(text)
  } catch {
    return []
  }
  const listed: unknown[] = Array.isArray(parsed?.intents) ? parsed.intents : []
  const backingOff = (intent: any) => {
    const retry = typeof intent?.retry_after === 'string' ? Date.parse(intent.retry_after) : NaN
    return !Number.isNaN(retry) && retry > now
  }
  return listed.flatMap((intent: any) =>
    typeof intent?.intent_id === 'string' && typeof intent?.repo === 'string' && intent.repo !== '' && !backingOff(intent)
      ? [{ intent_id: intent.intent_id, repo: intent.repo }]
      : [],
  )
}

// Why a reconcile could not read the intents relais lists.
export type ReconcileFailure =
  | { kind: 'status_exit'; exitCode: number; detail: string }
  | { kind: 'status_threw'; detail: string }
  | { kind: 'status_unparsable'; detail: string }

export type Reconciled = { started: string[]; failure?: ReconcileFailure }

// The plugin is the only launcher of successor intents: on start and at each
// status poll it asks relais what is waiting and launches all of it; relais
// claims atomically, so a launch of an intent that is running is refused. A
// status that cannot be read launches nothing and is returned as a failure.
export async function reconcileIntents(fx: Fx, store: Store): Promise<Reconciled> {
  const failed = (failure: ReconcileFailure): Reconciled => {
    store.intentFailure = failure
    return { started: [], failure }
  }
  let stdout: string
  try {
    const session = await fx.session.id()
    const result = await fx.process.run(['relais', 'native', 'status', '--session', session], {
      timeoutMs: STATUS_TIMEOUT_MS,
    })
    if (result.exitCode !== 0) {
      return failed({
        kind: 'status_exit',
        exitCode: result.exitCode,
        detail: String(result.stderr ?? '').trim().slice(0, 400),
      })
    }
    stdout = String(result.stdout ?? '')
  } catch (reason) {
    return failed({ kind: 'status_threw', detail: String((reason as any)?.message ?? reason).slice(0, 400) })
  }
  let parsed: unknown = {}
  try {
    // No output at all lists nothing; output that is not JSON is a failure.
    if (stdout.trim() !== '') parsed = JSON.parse(stdout)
  } catch (reason) {
    return failed({ kind: 'status_unparsable', detail: String((reason as any)?.message ?? reason).slice(0, 400) })
  }
  store.intentFailure = undefined
  const started: string[] = []
  for (const intent of launchableIntents(JSON.stringify(parsed))) {
    try {
      await startIntent(fx, store, { intent: intent.intent_id, cwd: intent.repo })
      started.push(intent.intent_id)
    } catch (reason) {
      return failed({ kind: 'status_threw', detail: `launching ${intent.intent_id}: ${String((reason as any)?.message ?? reason).slice(0, 400)}` })
    }
  }
  return { started }
}

const describeFailure = (f: ReconcileFailure) =>
  `successor intents were not reconciled (${f.kind}${f.kind === 'status_exit' ? ` ${f.exitCode}` : ''}): ${f.detail}`

export async function startReplay(
  fx: Fx,
  store: Store,
  input: { task: string; recipe: string; cwd: string; design?: string },
) {
  const design = input.design ? ['--design', input.design] : []
  const argv = ['relais', 'dataset', 'replay', '--task', input.task, '--recipe', input.recipe, ...design, '--protocol']
  await startChild(fx, store, argv, input.cwd)
  return `Started relais replay of ${input.task} under ${input.recipe} (in ${input.cwd}). It runs in the relais pane and /relais-status; its outcome arrives here as a message when it ends.`
}

// The one way a relais child starts: `run` and `replay` differ in argv only.
async function startChild(fx: Fx, store: Store, argvOf: string[], cwd: string) {
  const session = await fx.session.id()
  store.sessions.add(session)
  store.heldOutcome = undefined
  await sendHello(fx, store)
  const now = await fx.clock.now()
  const key = `starting-${store.children.length + 1}`
  const request = {
    argv: argvOf,
    cwd,
    env: { RELAIS_HOST: 'claude-code-mod', RELAIS_SESSION_ID: session },
  }
  const argv = JSON.stringify(request.argv)
  const child: Child = { session, cwd, argv, exited: false, key }
  store.children.push(child)
  store.starting.push(child)
  store.models = { ...store.models, [key]: emptyRun(key, now) }
  detach(drain(fx, store, child, fx.process.spawn(request)))
  await announceRun(fx, store)
}

// The child a `process.spawn` request starts, if it is one of relais's runs.
// The module's `process.spawn` hook reads the child's output as it passes,
// from inside the hook: only a call made inside a hook frame runs this
// plugin's own `agent.spawn` hook, and a detached loop's calls skip it.
export function childOf(store: Store, request: { argv?: string[] }): Child | undefined {
  const argv = JSON.stringify(request.argv ?? [])
  const index = store.starting.findIndex(c => c.argv === argv)
  return index < 0 ? undefined : store.starting.splice(index, 1)[0]
}

export type Carry = { out: string; err: string }

// One piece of the child's output, folded into the module's memory.
export async function onChunk(fx: Fx, store: Store, child: Child, carry: Carry, chunk: any) {
  const now = await fx.clock.now()
  if (chunk.stream === 'stderr') onStderr(store, child, carry, chunk.text, now)
  else await onStdout(fx, store, child, carry, chunk.text, now)
  markDirty(store)
}

// Waits for the child to end, whatever ends it, and marks it exited so that
// no callback keeps retrying for it.
async function drain(fx: Fx, store: Store, child: Child, stream: any) {
  let exit: { code: number | null } | undefined
  try {
    let step = await stream.next()
    while (!step.done) step = await stream.next()
    exit = step.value
  } catch (reason) {
    const now = await fx.clock.now()
    addStderr(store, child, `relais could not run: ${reason instanceof Error ? reason.message : reason}`, now)
  }
  child.exited = true
  const now = await fx.clock.now()
  endUnfinished(store, child, exit?.code ?? null, now)
  markDirty(store)
}

function onStderr(store: Store, child: Child, carry: Carry, text: string, now: number) {
  const { lines, carry: rest } = splitLines(carry.err, text)
  carry.err = rest
  for (const line of lines) addStderr(store, child, line, now)
}

// relais's first stderr line names the session it bound the run to
// (`session: <id> (<source>)`): metadata, kept apart from its errors.
const SESSION_LINE = /^session: (\S+)/

function addStderr(store: Store, child: Child, text: string, now: number) {
  const model = store.models[child.key] ?? emptyRun(child.key, now)
  const session = model.session === undefined ? SESSION_LINE.exec(text) : null
  if (session) {
    store.models = { ...store.models, [child.key]: applyEvent(model, { kind: 'session', id: session[1] }, now) }
    return
  }
  child.stderrTail = [...(child.stderrTail ?? []), text].slice(-STDERR_TAIL)
  store.models = { ...store.models, [child.key]: applyEvent(model, { kind: 'stderr', text }, now) }
}

async function onStdout(fx: Fx, store: Store, child: Child, carry: Carry, text: string, now: number) {
  const { lines, carry: rest } = splitLines(carry.out, text)
  carry.out = rest
  for (const line of lines) {
    const parsed = parseProtocolLine(line)
    if (parsed) await onLine(fx, store, child, parsed, now)
  }
}

async function onLine(fx: Fx, store: Store, child: Child, line: any, now: number) {
  switch (line.relais) {
    case 'spawn':
      await handleSpawn(fx, store, child, line)
      return
    case 'continue':
      await handleContinue(fx, store, child, line)
      return
    case 'stop':
      await handleStop(fx, store, line)
      return
    case 'event':
      onEvent(store, child, line, now)
      if (line.event?.kind === 'decision' && line.event.what === 'escalate') {
        toastDecision(fx, store, `escalating · ${line.event.reason}`)
      }
      return
    case 'done':
      await onDone(fx, store, child, line, now)
      return
  }
}

// Folds an event into its run; the first event renames the child's
// placeholder to the run's id and keeps what the placeholder collected.
function onEvent(store: Store, child: Child, line: any, now: number) {
  if (typeof line.run !== 'string') return
  if (child.key !== line.run) {
    const placeholder = store.models[child.key]
    const { [child.key]: _gone, ...rest } = store.models
    store.models = placeholder
      ? {
          ...rest,
          [line.run]: { ...emptyRun(line.run, placeholder.startedAt), events: placeholder.events, session: placeholder.session },
        }
      : rest
    child.key = line.run
  }
  store.models = applyLine(store.models, line, now)
}

async function onDone(fx: Fx, store: Store, child: Child, line: any, now: number) {
  child.doneSeen = true
  const run = typeof line.run === 'string' ? line.run : child.key
  const outcome = String(line.outcome ?? 'ended')
  const code = typeof line.code === 'string' ? line.code : null
  const detail = typeof line.detail === 'string' ? line.detail : null
  const receipt = typeof line.receipt === 'string' ? line.receipt : null
  const model = store.models[run]
  if (model && isLive(model)) {
    store.models = { ...store.models, [run]: applyEvent(model, { kind: 'outcome', state: outcome, receipt, summary: line.summary }, now) }
  }
  store.heldOutcome = `${shortId(run)} ${outcome}`
  onRunDone(store, outcome)
  markDirty(store)
  toastOutcome(fx, run, outcome, receipt)
  const trial = typeof line.trial === 'string' ? line.trial : null
  const why = code ? ` (${code})${detail ? `: ${detail}` : ''}` : detail ? `: ${detail}` : ''
  // `run: null`: refused before a run existed, so there is no id to name.
  const headline =
    typeof line.run === 'string'
      ? `relais ${trial ? 'replay' : 'run'} ${run} finished: ${outcome}${why}.`
      : `relais ${trial ? 'replay' : 'run'} ${outcome}${why}. Nothing ran.`
  const facts = [receipt ? `Receipt: ${receipt}` : '', summaryText(line.summary), trial ? `Replay trial: ${trial}` : '', commandsChangedText(model)].filter(Boolean)
  const awaits = awaitsState(outcome) && typeof line.run === 'string'
  const text = [headline, ...facts, code ? NEXT_STEP[code] ?? '' : '', awaits ? DECIDE_STEP : '', outcome === 'accepted' && typeof line.run === 'string' && !trial ? FEEDBACK_STEP : ''].filter(Boolean).join('\n')
  // The stream hook descends from the `run` tool call, and a prompt cannot
  // be submitted from under a tool or command hook: the session's timer
  // (`pump`) sends it.
  store.verdicts.push(text)
  if (awaits) {
    store.pendingOutcomes.set(text, { run, outcome, code, kind: trial ? 'replay' : 'run', facts, noted: new Set() })
  }
}

// The run's `commands_changed` decision, when relais warned that the
// repository's commands changed since the last run (ADR-0007): the person
// is told once, here, in the outcome message.
export function commandsChangedText(model: RunModel | undefined): string {
  const warned = model?.decisions.find(d => d.text.startsWith('commands_changed · '))
  return warned ? `relais warned: the repository's commands changed ${warned.text.slice('commands_changed · '.length)}`.slice(0, MAX_SUMMARY) : ''
}

// What relais's `done` says the candidate changed: an object
// `{files_changed, insertions, deletions}`, or nothing.
export function summaryText(summary: unknown): string {
  const changed = changedText(summary)
  return changed ? `Changed: ${changed}`.slice(0, MAX_SUMMARY) : ''
}

// A child that exited with its run still live was cut off. One that exited
// without a `done` line at all (a panic, a refusal before the protocol was
// up) still tells the model: the skill waits for an outcome, and silence
// would leave it waiting.
function endUnfinished(store: Store, child: Child, code: number | null, now: number) {
  if (!child.doneSeen) {
    const tail = (child.stderrTail ?? []).join('\n')
    store.verdicts.push(
      [
        `relais exited${code === null ? '' : ` with code ${code}`} without an outcome. Nothing was accepted.`,
        tail ? `Its last output:\n${tail}` : '',
      ]
        .filter(Boolean)
        .join('\n'),
    )
  }
  const model = store.models[child.key]
  if (!model || !isLive(model)) return
  const state = code === 0 ? 'ended' : 'interrupted'
  const note = `relais exited${code === null ? '' : ` with code ${code}`}`
  const noted = applyEvent(model, { kind: 'stderr', text: note }, now)
  store.models = { ...store.models, [child.key]: applyEvent(noted, { kind: 'outcome', state, receipt: null }, now) }
}

// The timeline rebuilt from `relais native status`, for what the module's
// memory no longer holds (after /clear, /resume or /branch). Without `run`
// it asks for this session's runs (`--session`), never the machine's latest;
// with one it is an explicit lookup that works for any session's run, and a
// run of another session is kept marked `foreign`.
export async function reloadTimeline(fx: Fx, store: Store, run?: string) {
  const session = await fx.session.id()
  const argv = ['relais', 'native', 'status', ...(run ? ['--run', run] : ['--session', session])]
  try {
    const result = await fx.process.run(argv, { timeoutMs: STATUS_TIMEOUT_MS })
    if (result.exitCode !== 0) {
      store.statusFailure = `relais native status exited ${result.exitCode}: ${String(result.stderr ?? '').trim().slice(0, 400)}`
      return undefined
    }
    store.statusFailure = undefined
    const text: string = result.stdout
    const now = await fx.clock.now()
    const rebuilt = foreignMarked(rebuildRuns(JSON.parse(text), now), session)
    // A run the module is still reading is fresher than the file.
    store.models = { ...rebuilt, ...store.models }
    markDirty(store)
    return text
  } catch (reason) {
    store.statusFailure = `relais native status failed: ${String((reason as any)?.message ?? reason).slice(0, 400)}`
    return undefined
  }
}

// A run whose ledger session is not `session` is another session's.
function foreignMarked(models: Record<string, RunModel>, session: string): Record<string, RunModel> {
  return Object.fromEntries(
    Object.entries(models).map(([key, m]) => [key, { ...m, foreign: m.session !== undefined && m.session !== session }]),
  )
}

// What the ledger says now about a run the module holds live: an outcome it
// holds (a reap or a cancel by another process wrote none to the stream),
// and whether it has gone quiet. A run the status cannot be read for stays
// as it was.
export function refreshRun(fx: Fx, store: Store, run: string): Promise<void> {
  const inFlight = reconciling.get(store) ?? new Map<string, Promise<void>>()
  reconciling.set(store, inFlight)
  const running = inFlight.get(run)
  if (running) return running
  const request = readRun(fx, store, run).finally(() => inFlight.delete(run))
  inFlight.set(run, request)
  return request
}

async function readRun(fx: Fx, store: Store, run: string) {
  try {
    const result = await fx.process.run(['relais', 'native', 'status', '--run', run], { timeoutMs: RECONCILE_TIMEOUT_MS })
    if (result.exitCode !== 0) {
      store.statusFailure = `relais native status --run ${run} exited ${result.exitCode}: ${String(result.stderr ?? '').trim().slice(0, 400)}`
      return
    }
    const now = await fx.clock.now()
    const reply = JSON.parse(String(result.stdout ?? ''))
    const fresh = rebuildRuns(reply, now)[run]
    const known = store.models[run]
    if (!fresh || !known) return
    const held = reconcileStatus(known, reply)
    if (held !== known) {
      store.models = { ...store.models, [run]: held }
      markDirty(store)
    }
    if (!isLive(held)) {
      // A run that stopped awaiting a person may have been answered since.
      if (awaitsAPerson(held) && fresh.outcome && fresh.outcome.state !== held.outcome.state) {
        const outcome = { ...held.outcome, state: fresh.outcome.state, ...(fresh.outcome.answer ? { answer: fresh.outcome.answer } : {}) }
        store.models = { ...store.models, [run]: { ...held, phase: fresh.outcome.state, outcome } }
        markDirty(store)
      }
      return
    }
    const next = fresh.outcome
      ? applyEvent(held, { kind: 'outcome', state: fresh.outcome.state, receipt: fresh.outcome.receipt, reason: fresh.outcome.reason }, now)
      : { ...held, stalled: fresh.stalled }
    store.models = { ...store.models, [run]: next }
    markDirty(store)
  } catch (reason) {
    // The status reply then shows the model as the stream left it, and
    // says why it could not be refreshed.
    store.statusFailure = `relais native status --run ${run} failed: ${String((reason as any)?.message ?? reason).slice(0, 400)}`
  }
}

// The reply of the `status` tool: without `run`, a page of at most
// STATUS_PAGE short entries for this session's runs (live or awaiting a
// person first, then the most recent; `offset` pages on), with `more` and
// `waiting`; with `run`, that run in full (marked `foreign` when another
// session started it). Every live
// model, and every one awaiting a person, is checked against the ledger first.
export async function statusOf(fx: Fx, store: Store, run?: string, opts: { output?: boolean; offset?: number } = {}): Promise<string> {
  const { failure } = await reconcileIntents(fx, store)
  const now = await fx.clock.now()
  const known = () => Object.values(store.models).filter(m => (run ? m.run === run : !m.foreign))
  const live = known().filter(
    m => !m.run.startsWith('starting-') && (isLive(m) || awaitsAPerson(m)),
  )
  await Promise.all(live.map(m => refreshRun(fx, store, m.run)))
  if (known().length === 0) await reloadTimeline(fx, store, run)
  const models = known()
  if (models.length === 0) {
    return store.statusFailure
      ? `No relais run is known in this session, and relais's own timeline could not be read: ${store.statusFailure}${failure ? ` ${describeFailure(failure)}` : ''}`
      : `No relais run is known in this session.${failure ? ` ${describeFailure(failure)}` : ''}`
  }
  if (run) {
    const replies = models.map(m => statusReply(m, now, opts))
    return JSON.stringify(failure ? { runs: replies, intent_reconcile_failure: failure } : replies)
  }
  // Without a run: at most STATUS_PAGE runs, the live or waiting-for-a-person
  // ones first, then the most recent; `offset` pages through the rest.
  const needsLook = (m: RunModel) => isLive(m) || awaitsAPerson(m)
  const ordered = [...models].sort((a, b) => Number(needsLook(b)) - Number(needsLook(a)) || b.startedAt - a.startedAt)
  // The tool handler refuses an offset that is not a whole number >= 0.
  const offset = opts.offset ?? 0
  const page = ordered.slice(offset, offset + STATUS_PAGE)
  const end = offset + page.length
  const more = Math.max(0, ordered.length - end)
  const summary = {
    runs: page.map(statusSummaryEntry),
    more,
    waiting: ordered.filter(needsLook).length,
    ...(more > 0 ? { next_offset: end } : {}),
    ...(failure ? { intent_reconcile_failure: failure } : {}),
  }
  return JSON.stringify(summary)
}

// The `cancel` tool: `relais native cancel --run <run> --session <session>`.
// Its stdout line and exit status come back as they were. A run it ended is
// marked stopped in the store, so no late `stopped` is sent for it.
export async function cancelRun(fx: Fx, store: Store, run: string): Promise<string> {
  const session = await fx.session.id()
  let result: { exitCode: number; stdout?: string; stderr?: string }
  try {
    result = await fx.process.run(['relais', 'native', 'cancel', '--run', run, '--session', session], {
      timeoutMs: CANCEL_TIMEOUT_MS,
    })
  } catch (reason) {
    return `relais native cancel could not run: ${String((reason as any)?.message ?? reason).slice(0, 400)}\nexit status: unknown`
  }
  const line = String(result.stdout ?? '').trim() || String(result.stderr ?? '').trim().slice(0, 400)
  // Only a run that has ended: one whose driver was told to stop but has
  // not yet ended still waits for its dispatches' `stopped`.
  if (result.exitCode === 0 && !line.startsWith('cancel requested')) stopDispatchesOf(store, run)
  return `${line}\nexit status: ${result.exitCode}`
}

// The run's dispatches are over: `stopped` for them would only be refused.
function stopDispatchesOf(store: Store, run: string) {
  for (const [id, child] of store.dispatchChild) {
    const d = store.dispatches.get(id)
    if (child.key === run && d && !d.isStopped) store.dispatches.set(id, { ...d, isStopped: true })
  }
}
