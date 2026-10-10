// A scripted engine beneath the plugin: it answers every `$` call the plugin
// makes from memory and records what reached it, so a test drives the plugin
// with relais's request lines and reads what the plugin did.

import { mock } from 'claude-code/testing'

export type Calls = {
  run: { argv: string[]; init: any }[]
  spawn: any[]
  // What reached the engine's `agent.spawn`, after the plugin's own hook.
  spawned: any[]
  tool: any[]
  toasts: string[]
  statuses: (string | undefined)[]
  prompts: any[]
  opened: any[]
  // What reached the engine's `turn.step` (after the plugin's hooks), and
  // each `$.model.complete` request.
  steps: any[]
  completes: any[]
  asked: { question: string; options: string[] }[]
  // Each `$.session.messages` call that reached the engine.
  messages: any[]
}

export type Agents = { id: string; status: string; type?: string; name?: string; description?: string }[]

const ok = { exitCode: 0, stdout: '', stderr: '', isStdoutTruncated: false, isStderrTruncated: false }

export const RELAIS_AGENT_TYPE = 'relais:relais-worker-sonnet-medium'

export const zeroUsage = { input_tokens: 0, output_tokens: 0, cache_read_input_tokens: 0, cache_creation_input_tokens: 0 }

export function scriptedEngine(
  on: any,
  options: { session?: string; isPlaced?: boolean; store?: Record<string, unknown> } = {},
) {
  const clock = mock.clock(on, { now: Date.parse('2026-10-06T12:00:00Z') })
  mock.store(on, options.store ?? {})
  mock.env(on, { HOME: '/Users/p' })
  const calls: Calls = {
    run: [],
    spawn: [],
    spawned: [],
    tool: [],
    toasts: [],
    statuses: [],
    prompts: [],
    opened: [],
    steps: [],
    completes: [],
    asked: [],
    messages: [],
  }
  const agents: Agents = []
  // What a test changes to script the world: `relais native …` results, a
  // tool's answer, and a hook run inside a tool call.
  const script = {
    runResult: (_argv: string[], _init: any): any => ok,
    toolResult: (_e: any): any => ({ result: 'ok', text: 'ok' }),
    duringTool: async (_$: any, _e: any): Promise<void> => {},
    isPlaced: options.isPlaced ?? true,
    // How many `prompt.submit` calls fail before one goes through.
    submitFailures: 0,
    // A promise every `prompt.submit` call waits on before it settles: the
    // engine's, which resolves once the session is idle and the prompt's
    // turn starts. Null settles at once.
    submitHold: null as Promise<void> | null,
    // `$.model.complete`'s answer (the classifier): unanswered by default.
    complete: (_e: any): any => ({ isAnswered: false, reason: 'empty-reply', usage: zeroUsage }),
    sessionModel: 'claude-sonnet-5-5',
    // A response's usage; the model that answered is the request's.
    stepUsage: (e: any): any => ({ ...zeroUsage, input_tokens: 10, output_tokens: 5, model: e.model }),
    // Whether `agent.spawn` names the started agent (a workflow's remote one does not).
    spawnHasAgentId: true,
    // `$.ui.ask`'s answer; a throw is a dismissal.
    ask: (_question: string, _options: string[]): any => {
      throw new Error('dismissed')
    },
    // `$.session.messages({ agentId })`'s answer: the rows, or `{ deny }`.
    messages: (_e: any): any => [],
    // How long (mock clock, ms) the call takes to answer; 0 answers at once.
    messagesDelay: 0,
    // The call never settles.
    messagesNever: false,
  }
  const store: Record<string, { value: unknown; version: number }> = {}
  let nextAgent = 1

  // Each child's output, fed by the test one chunk at a time. A child is
  // known by its task (`relais run --task <task> --protocol`, or `relais
  // dataset replay --task <task> --recipe <recipe> --protocol`).
  const queues = new Map<string, { items: any[]; wake: (() => void) | undefined }>()
  const queueOf = (task: string) => {
    const queue = queues.get(task) ?? { items: [], wake: undefined }
    queues.set(task, queue)
    return queue
  }
  const feed = (task: string) => ({
    push: (which: 'stdout' | 'stderr', text: string) => {
      const queue = queueOf(task)
      queue.items.push({ stream: which, text })
      queue.wake?.()
    },
    end: (code = 0) => {
      const queue = queueOf(task)
      queue.items.push({ end: code })
      queue.wake?.()
    },
  })
  const stream = feed('fix-it.json')

  on('session.id', () => ({ value: options.session ?? 'session-1' }))
  on('session.start', (_$: any, e: any) => ({ cwd: e.cwd }))
  on('session.end', (_$: any, e: any) => ({ sessionId: e.sessionId }))
  on('tool.register', (_$: any, e: any) => ({ value: { tool: `mcp__relais__${e.name}` } }))
  on('command.register', (_$: any, e: any) => ({ value: { command: e.name } }))
  on('process.run', (_$: any, e: any) => {
    calls.run.push({ argv: [...e.argv], init: e.init })
    return { value: script.runResult(e.argv, e.init) }
  })
  on('process.spawn', async function* (_$: any, e: any) {
    calls.spawn.push(e)
    const queue = queueOf(e.argv[e.argv.indexOf('--task') + 1])
    for (;;) {
      while (queue.items.length === 0) await new Promise<void>(resolve => (queue.wake = resolve))
      const next = queue.items.shift()
      if ('end' in next) return { value: { code: next.end, signal: null } }
      yield next
    }
  })
  on('agent.list', () => ({ value: agents.map(a => ({ type: RELAIS_AGENT_TYPE, description: '', ...a })) }))
  // `agent.spawn` hooks decide the model and the directory; the agent then
  // shows in `$.agent.list()` as running.
  on('agent.spawn', (_$: any, e: any) => {
    calls.spawned.push(e)
    const id = `agent-${nextAgent++}`
    agents.push({
      id,
      status: 'running',
      type: e.subagent_type ?? e.subagentType,
      description: e.description,
    })
    return { model: e.model ?? 'sonnet', ...(script.spawnHasAgentId ? { agentId: id } : {}) }
  })
  on('turn.step', async function* (_$: any, e: any) {
    calls.steps.push(e)
    return { turnId: e.turnId, index: e.index, answer: '', toolUses: [], stopReason: 'end_turn', usage: script.stepUsage(e) }
  })
  // The engine's own verdict on a tool call: allowed (bypass mode).
  on('tool.check', () => ({ decision: 'allow' }))
  on('model.complete', async (_$: any, e: any) => {
    calls.completes.push(e)
    return { value: await script.complete(e) }
  })
  on('session.model', () => ({ value: script.sessionModel }))
  on('session.messages', async (_$: any, e: any) => {
    calls.messages.push(e)
    if (script.messagesNever) await new Promise<void>(() => {})
    if (script.messagesDelay > 0) await clock.sleep(script.messagesDelay)
    return { value: script.messages(e) }
  })

  on('agent.offer', () => ({ isOffered: true }))
  on('turn.complete', (_$: any, e: any) => ({ text: e.answer }))
  on('tool.call', async ($: any, e: any) => {
    // `$.ui.ask` is a call of the AskUserQuestion tool beneath the hooks.
    if (e.tool === 'AskUserQuestion') {
      const q = e.questions?.[0] ?? {}
      const options = (q.options ?? []).map((o: any) => o.label)
      calls.asked.push({ question: q.question, options })
      const answer = await script.ask(q.question, options)
      return { result: { questions: e.questions, answers: { [q.question]: answer } }, text: String(answer) }
    }
    calls.tool.push(e)
    await script.duringTool($, e)
    return script.toolResult(e)
  })
  on('ui.open', (_$: any, e: any) => {
    calls.opened.push(e)
    return {
      value: script.isPlaced ? { isPlaced: true } : { isPlaced: false, reason: 'unasked below 144 columns (120 now)' },
    }
  })
  on('ui.toast', (_$: any, e: any) => {
    calls.toasts.push(e.text)
    return { value: undefined }
  })
  on('ui.status', (_$: any, e: any) => {
    calls.statuses.push(e.text)
    return { value: undefined }
  })
  on('state.get', (_$: any, e: any) => ({ value: store[`${e.plugin}.${e.key}`] ?? { value: undefined, version: 0 } }))
  on('state.set', (_$: any, e: any) => {
    const key = `${e.plugin}.${e.key}`
    const version = (store[key]?.version ?? 0) + 1
    store[key] = { value: e.value, version }
    return { value: { isSet: true, version } }
  })
  on('prompt.submit', (_$: any, e: any) => {
    if (script.submitFailures > 0) {
      script.submitFailures -= 1
      throw new Error('not now')
    }
    calls.prompts.push(e)
    if (script.submitHold) return script.submitHold.then(() => ({ text: e.text }))
    return { text: e.text }
  })

  return { clock, calls, stream, feed, script, agents }
}

export type Engine = ReturnType<typeof scriptedEngine>

// Lets everything the plugin started unawaited run to its end.
export async function settle(engine: Engine) {
  for (let i = 0; i < 12; i++) await engine.clock.settle()
}

// The run tool only queues the run; the session's 100 ms timer starts it.
export async function startQueued(engine: Engine) {
  await engine.clock.advance(100)
  await settle(engine)
}

// A session with the plugin loaded and one run started.
export async function startedRun($: any, on: any, options: { isPlaced?: boolean } = {}) {
  const engine = scriptedEngine(on, options)
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await settle(engine)
  await $.tool.call({ tool: 'mcp__relais__run', task: 'fix-it.json', cwd: '/repo' })
  await startQueued(engine)
  return engine
}

export const line = (value: unknown) => JSON.stringify(value) + '\n'

export const event = (run: string, seq: number, body: Record<string, unknown>) =>
  line({
    relais: 'event',
    run,
    seq,
    at: new Date(Date.parse('2026-10-06T12:00:00Z') + seq * 1000).toISOString(),
    event: body,
  })

export const spawnLine = (dispatch: string, extra: Record<string, unknown> = {}) =>
  line({
    relais: 'spawn',
    dispatch,
    prompt: 'do the thing',
    subagent_type: RELAIS_AGENT_TYPE,
    model: 'sonnet',
    cwd: `/work/${dispatch}`,
    ...extra,
  })

export const continueLine = (dispatch: string, agent: string) =>
  line({ relais: 'continue', dispatch, agent, message: 'the checks failed: fix them' })

export const turn = (agentId: string, usage: Record<string, unknown>, answer = 'done') => ({
  agentId,
  answer,
  durationMs: 10,
  isAborted: false,
  turnId: `turn-${agentId}`,
  reason: 'answer',
  usage: {
    input_tokens: 0,
    output_tokens: 0,
    cache_read_input_tokens: 0,
    cache_creation_input_tokens: 0,
    model: 'sonnet',
    ...usage,
  },
})

// The `relais native <verb>` calls the plugin made, with their stdin parsed.
export const nativeCalls = (calls: Calls, verb: string) =>
  calls.run
    .filter(c => c.argv[1] === 'native' && c.argv[2] === verb)
    .map(c => ({ argv: c.argv, init: c.init, payload: c.init?.stdin ? JSON.parse(c.init.stdin) : undefined }))
