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
}

export type Agents = { id: string; status: string; type?: string; name?: string; description?: string }[]

const ok = { exitCode: 0, stdout: '', stderr: '', isStdoutTruncated: false, isStderrTruncated: false }

export const RELAIS_AGENT_TYPE = 'relais:relais-worker-sonnet-medium'

export function scriptedEngine(on: any, options: { session?: string; isPlaced?: boolean } = {}) {
  const clock = mock.clock(on, { now: Date.parse('2026-10-06T12:00:00Z') })
  const calls: Calls = { run: [], spawn: [], spawned: [], tool: [], toasts: [], statuses: [], prompts: [], opened: [] }
  const agents: Agents = []
  // What a test changes to script the world: `relais native …` results, a
  // tool's answer, and a hook run inside a tool call.
  const script = {
    runResult: (_argv: string[], _init: any): any => ok,
    toolResult: (_e: any): any => ({ result: 'ok', text: 'ok' }),
    duringTool: async (_$: any, _e: any): Promise<void> => {},
    isPlaced: options.isPlaced ?? true,
  }
  const store: Record<string, { value: unknown; version: number }> = {}
  let nextAgent = 1

  // Each child's output, fed by the test one chunk at a time. A child is
  // known by its task (`relais run --task <task> --protocol`).
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
  const stream = feed('fix it')

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
    const queue = queueOf(e.argv[3])
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
    agents.push({
      id: `agent-${nextAgent++}`,
      status: 'running',
      type: e.subagent_type ?? e.subagentType,
      description: e.description,
    })
    return { model: e.model ?? 'sonnet' }
  })
  on('agent.offer', () => ({ isOffered: true }))
  on('turn.complete', (_$: any, e: any) => ({ text: e.answer }))
  on('tool.call', async ($: any, e: any) => {
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
    calls.prompts.push(e)
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
  await $.tool.call({ tool: 'mcp__relais__run', task: 'fix it', cwd: '/repo' })
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
