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
  emptyRun,
  isLive,
  rebuildRuns,
  shortId,
  statusReply,
  type RunModel,
} from './timeline.ts'
import { sendHello } from './callbacks.ts'
import { detach, type Child, type Store } from './store.ts'
import {
  announceRun,
  flush,
  markDirty,
  toastDecision,
  toastOutcome,
} from './ui.ts'

const STATUS_TIMEOUT_MS = 10_000
const MAX_SUMMARY = 4000

// One tick of the session's timer: sends the verdicts waiting, then writes
// the pane's state if anything changed.
export async function pump(fx: Fx, store: Store) {
  // A verdict leaves the queue only once submitted: one that fails is
  // tried again on the next tick, never lost.
  while (store.verdicts.length > 0) {
    try {
      await fx.prompt.submit({ text: store.verdicts[0] })
    } catch (reason) {
      // Kept for the next tick; said once per streak of failures.
      if (!store.isSubmitFailing) {
        await note(fx, store, undefined, `the outcome message could not be sent yet: ${String((reason as any)?.message ?? reason)}`)
      }
      store.isSubmitFailing = true
      break
    }
    store.isSubmitFailing = false
    store.verdicts.shift()
  }
  await flush(fx, store)
}

export async function startRun(fx: Fx, store: Store, input: { task: string; cwd: string }) {
  await startChild(fx, store, ['relais', 'run', '--task', input.task, '--protocol'], input.cwd)
  return `Started relais run for: ${input.task} (in ${input.cwd}). It runs in the relais pane and /relais-status; its outcome arrives here as a message when it ends.`
}

export async function startReplay(
  fx: Fx,
  store: Store,
  input: { task: string; recipe: string; cwd: string },
) {
  const argv = ['relais', 'dataset', 'replay', '--task', input.task, '--recipe', input.recipe, '--protocol']
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

function addStderr(store: Store, child: Child, text: string, now: number) {
  const model = store.models[child.key] ?? emptyRun(child.key, now)
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
      ? { ...rest, [line.run]: { ...emptyRun(line.run, placeholder.startedAt), events: placeholder.events } }
      : rest
    child.key = line.run
  }
  store.models = applyLine(store.models, line, now)
}

async function onDone(fx: Fx, store: Store, child: Child, line: any, now: number) {
  const run = typeof line.run === 'string' ? line.run : child.key
  const outcome = String(line.outcome ?? 'ended')
  const receipt = typeof line.receipt === 'string' ? line.receipt : null
  const model = store.models[run]
  if (model && isLive(model)) {
    store.models = { ...store.models, [run]: applyEvent(model, { kind: 'outcome', state: outcome, receipt }, now) }
  }
  store.heldOutcome = `${shortId(run)} ${outcome}`
  markDirty(store)
  toastOutcome(fx, run, outcome, receipt)
  const trial = typeof line.trial === 'string' ? line.trial : null
  const text = [
    `relais ${trial ? 'replay' : 'run'} ${run} finished: ${outcome}.`,
    receipt ? `Receipt: ${receipt}` : '',
    summaryText(line.summary),
    trial ? `Replay trial: ${trial}` : '',
  ]
    .filter(Boolean)
    .join('\n')
  // The stream hook descends from the `run` tool call, and a prompt cannot
  // be submitted from under a tool or command hook: the session's timer
  // (`pump`) sends it.
  store.verdicts.push(text)
}

// What relais's `done` says the candidate changed: an object
// `{files_changed, insertions, deletions}`, or nothing.
export function summaryText(summary: unknown): string {
  if (typeof summary !== 'object' || summary === null) return ''
  const { files_changed: files, insertions, deletions } = summary as Record<string, unknown>
  if (typeof files !== 'number') return ''
  const plus = typeof insertions === 'number' ? insertions : 0
  const minus = typeof deletions === 'number' ? deletions : 0
  return `Changed: ${files} file${files === 1 ? '' : 's'}, +${plus} -${minus}`.slice(0, MAX_SUMMARY)
}

// A child that exited with its run still live was cut off.
function endUnfinished(store: Store, child: Child, code: number | null, now: number) {
  const model = store.models[child.key]
  if (!model || !isLive(model)) return
  const state = code === 0 ? 'ended' : 'interrupted'
  const note = `relais exited${code === null ? '' : ` with code ${code}`}`
  const noted = applyEvent(model, { kind: 'stderr', text: note }, now)
  store.models = { ...store.models, [child.key]: applyEvent(noted, { kind: 'outcome', state, receipt: null }, now) }
}

// The timeline rebuilt from `relais native status`, for what the module's
// memory no longer holds (after /clear, /resume or /branch).
export async function reloadTimeline(fx: Fx, store: Store, run?: string) {
  const argv = ['relais', 'native', 'status', ...(run ? ['--run', run] : [])]
  try {
    const result = await fx.process.run(argv, { timeoutMs: STATUS_TIMEOUT_MS })
    if (result.exitCode !== 0) {
      store.statusFailure = `relais native status exited ${result.exitCode}: ${String(result.stderr ?? '').trim().slice(0, 400)}`
      return undefined
    }
    store.statusFailure = undefined
    const text: string = result.stdout
    const now = await fx.clock.now()
    const rebuilt: Record<string, RunModel> = rebuildRuns(JSON.parse(text), now)
    // A run the module is still reading is fresher than the file.
    store.models = { ...rebuilt, ...store.models }
    markDirty(store)
    return text
  } catch (reason) {
    store.statusFailure = `relais native status failed: ${String((reason as any)?.message ?? reason).slice(0, 400)}`
    return undefined
  }
}

// The reply of the `status` tool: the models, or relais's own timeline.
export async function statusOf(fx: Fx, store: Store, run?: string): Promise<string> {
  const now = await fx.clock.now()
  const known = () => Object.values(store.models).filter(m => !run || m.run === run)
  if (known().length === 0) await reloadTimeline(fx, store, run)
  const models = known()
  if (models.length === 0) {
    return store.statusFailure
      ? `No relais run is known in this session, and relais's own timeline could not be read: ${store.statusFailure}`
      : 'No relais run is known in this session.'
  }
  return JSON.stringify(models.map(m => statusReply(m, now)))
}
