// A session with the router loaded: router-state answered from memory, the
// classifier scripted per request, and helpers to drive turns, tools and
// spawns through the plugin's hooks.

import { nativeCalls, scriptedEngine, settle, zeroUsage, type Engine } from './support.ts'

const ok = { exitCode: 0, stdout: '', stderr: '', isStdoutTruncated: false, isStderrTruncated: false }

// Micro-USD per million tokens; `cache_write` is the 1-hour rate.
export const ROUTER_STATE = {
  schema: 1,
  mode: 'on',
  mode_reason: 'envelope and r3 recorded',
  envelope: { granted_at: '2026-10-06T10:00:00Z', by: 'the person', epsilon_max: 0.1, source: 'plugin-ask' },
  r3: { id: 'r3-1', passed: true, at: '2026-10-06T11:00:00Z', source: 'plugin-ask' },
  holdout: false,
  seed: '0123456789abcdef',
  epsilon: 0.1,
  tiers: {
    research: { model: 'claude-haiku-5-5', effort: null },
    implementation: { model: 'claude-sonnet-5-5', effort: null },
    escalation: { model: 'claude-opus-5-5', effort: null },
  },
  capability_table: {
    version: 1,
    rules: [
      { when: { difficulty_min: 4 }, tier: 'escalation' },
      { when: { uncertainty: ['high'], scope: ['module', 'cross-cutting', 'unknown'] }, tier: 'escalation' },
      { when: { difficulty_max: 2, uncertainty: ['low'], verifiable_or_question: true }, tier: 'research' },
      { when: {}, tier: 'implementation' },
    ],
  },
  rates: {
    'claude-haiku-5-5': { input: 1_000_000, output: 5_000_000, cache_read: 100_000, cache_write: 2_000_000 },
    'claude-sonnet-5-5': { input: 3_000_000, output: 15_000_000, cache_read: 300_000, cache_write: 6_000_000 },
    'claude-opus-5-5': { input: 5_000_000, output: 25_000_000, cache_read: 500_000, cache_write: 10_000_000 },
  },
  priors: { task_tokens_by_difficulty: [20000, 60000, 150000, 400000, 800000], task_tokens: {} },
  pins: { 'pinned-agent': 'opus' },
  excluded_models: [],
  checks: [['cargo', 'test'], ['npm', 'test']],
  adjustments: [],
}

export const CONSENTS = {
  envelope_consent: { answer: 'Allow session routing', at: '2026-10-06T10:00:00Z', session: 'session-0' },
  r3_consent: { id: 'r3-1', at: '2026-10-06T11:00:00Z', session: 'session-0' },
}

// The classifier's JSON for one request.
export const label = (over: Record<string, unknown> = {}) => ({
  relation: 'new_task',
  kind: 'edit',
  difficulty: 2,
  scope: 'local',
  uncertainty: 'low',
  verifiable: true,
  explicit: { value: 'none', quote: null },
  user_model: null,
  confidence: 0.9,
  ...over,
})

export const answered = (value: unknown) => ({
  isAnswered: true,
  text: JSON.stringify(value),
  usage: { ...zeroUsage, input_tokens: 400, output_tokens: 6 },
})

export type Routed = Engine & {
  // The labels the classifier gives, by the request text it sees (the last
  // matching key wins), else `fallback`.
  labels: Map<string, unknown>
  fallback: unknown
  observeExits: number[]
}

// A session started with router-state and the store records given.
export async function routedSession(
  $: any,
  on: any,
  options: { state?: Record<string, unknown>; store?: Record<string, unknown> } = {},
): Promise<Routed> {
  const engine = scriptedEngine(on, { store: options.store ?? CONSENTS }) as Routed
  engine.labels = new Map()
  engine.fallback = label()
  engine.observeExits = []
  const state = { ...ROUTER_STATE, ...(options.state ?? {}) }
  engine.script.runResult = (argv: string[]) => {
    if (argv[1] === 'native' && argv[2] === 'router-state') return { ...ok, stdout: JSON.stringify(state) }
    if (argv[1] === 'native' && argv[2] === 'router-observe') {
      const code = engine.observeExits.length > 0 ? engine.observeExits.shift()! : 0
      return { ...ok, exitCode: code, stderr: code === 0 ? '' : `exit ${code}` }
    }
    return ok
  }
  engine.script.complete = (e: any) => {
    // Matched on the request alone, not the active task's summary.
    const prompt = String(e.prompt)
    const request = prompt.slice(prompt.indexOf('REQUEST'))
    let chosen: unknown = engine.fallback
    for (const [needle, value] of engine.labels) if (request.includes(needle)) chosen = value
    if (typeof chosen === 'function') return (chosen as () => unknown)()
    return answered(chosen)
  }
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await settle(engine)
  return engine
}

let turns = 0

// Reads a step through every hook, as the engine does.
export async function step($: any, e: Record<string, unknown>) {
  const stream = $.turn.step({ model: 'claude-sonnet-5-5', messageCount: 1, ...e })
  for await (const _chunk of stream) {
    // drained
  }
  return stream.result
}

// The person types `text`; the turn makes `steps` requests. Returns the
// requests as the engine received them.
export async function personTurn($: any, engine: Engine, text: string, steps = 1) {
  await $.prompt.submit({ text, wait: false, origin: { kind: 'composer' } })
  const turnId = `turn-${++turns}`
  const before = engine.calls.steps.length
  for (let index = 0; index < steps; index++) await step($, { turnId, index })
  return { turnId, steps: engine.calls.steps.slice(before) }
}

// A step of the same turn, after tool results.
export async function nextStep($: any, engine: Engine, turnId: string, index: number, extra: Record<string, unknown> = {}) {
  const before = engine.calls.steps.length
  await step($, { turnId, index, ...extra })
  return engine.calls.steps[before]
}

export const failingCheck = { isError: true, result: 'Exit code 101', text: 'Exit code 101\ntest result: FAILED' }
export const passingCheck = { result: { stdout: 'ok', stderr: '', interrupted: false }, text: 'test result: ok' }

// A tool call through the plugin's hooks; the engine answers `result`.
export async function toolCall($: any, engine: Engine, input: Record<string, unknown>, result: unknown) {
  const previous = engine.script.toolResult
  engine.script.toolResult = () => result
  try {
    return await $.tool.call(input)
  } finally {
    engine.script.toolResult = previous
  }
}

export const edit = (file: string, agentId?: string) => ({
  tool: 'Edit',
  file_path: file,
  old_string: 'a',
  new_string: 'b',
  ...(agentId ? { agentId } : {}),
})

export const bash = (command: string, agentId?: string) => ({ tool: 'Bash', command, ...(agentId ? { agentId } : {}) })

// The records sent through `relais native router-observe`, in order.
export const observed = (engine: Engine, kind?: string) =>
  nativeCalls(engine.calls, 'router-observe')
    .flatMap(c => c.payload.records)
    .filter((r: any) => !kind || r.kind === kind)

export async function endSession($: any, engine: Engine, reason = 'other') {
  await $.session.end({ reason, sessionId: 'session-1', resume: { command: 'claude --resume session-1' } })
  await settle(engine)
}
