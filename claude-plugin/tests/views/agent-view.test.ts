// The agent rows of the pane are Buttons; pressing one shows that agent's
// transcript, read with `$.session.messages({ agentId })` outside the flush.

import { expect, test } from 'claude-code/testing'
import { continueLine, event, scriptedEngine, settle, spawnLine, startedRun, startQueued } from '../support.ts'

const RUN = 'run-65d322006dd13-c35c'
const KEY = 'agent:agent-1'

const phase = (seq: number, state: string) => event(RUN, seq, { kind: 'phase', state, reason: 'ok', detail: {} })
const started = (seq: number, dispatch: string) =>
  event(RUN, seq, { kind: 'dispatch_started', dispatch, agent_kind: 'worker', attempt: 1, model: 'sonnet', effort: 'medium' })
const ended = (seq: number, dispatch: string, agent: string) =>
  event(RUN, seq, { kind: 'dispatch_ended', dispatch, agent, outcome: 'completed', usage: null, cost: null })

const user = (text: string) => ({ role: 'user', text, toolUses: [] })
const agentSays = (text: string, toolUses: any[] = []) => ({ role: 'assistant', text, toolUses })
const ROWS = [
  user('Migrate to pnpm'),
  agentSays('I will read package.json.', [
    { tool: 'Read', input: { file_path: 'package.json' }, text: '{"name":"x"}' },
    { tool: 'Bash', input: { command: 'pnpm install' }, text: 'ERR_PNPM\nlockfile', isError: true },
    { tool: 'Bash', input: { command: 'pnpm test' } },
  ]),
]

async function tick(engine: any, count = 1) {
  for (let i = 0; i < count; i++) {
    await engine.clock.advance(100)
    await settle(engine)
  }
}

// The Pane is one instance: a second mount in a test replaces the first.
const mounted = new WeakMap<object, any>()

async function mount($: any, columns = 72) {
  await mounted.get($)?.unmount()
  const ui = await $.ui.mount({
    plugin: 'relais',
    surface: 'terminal',
    component: 'Pane',
    requestId: 'relais',
    props: {
      title: 'relais',
      isFocused: false,
      bodyColumns: columns,
      placement: 'dock',
      scroll: { offset: 0, bodyRows: 40 },
      view: {},
    },
    viewport: { columns: 100, rows: 40, isFullscreen: true },
  })
  mounted.set($, ui)
  const rows = await ui.findAll({ type: 'Text' })
  return { ui, rows, texts: rows.map((r: any) => r.text as string) }
}

// A run with one worker spawned (agent-1), the pane written.
async function withWorker($: any, on: any, spawns = 1) {
  const engine = await startedRun($, on)
  let lines = phase(0, 'running')
  for (let n = 1; n <= spawns; n++) lines += started(n, `d${n}`) + spawnLine(`d${n}`)
  engine.stream.push('stdout', lines)
  await settle(engine)
  await tick(engine, 2)
  return engine
}

const open = async ($: any, engine: any, key = KEY) => {
  const { ui } = await mount($)
  await ui.press({ key })
  return ui
}

const countOf = (texts: string[], needle: string) => texts.filter(t => t.includes(needle)).length

test('a known agent is a Button with its subagent type under it, and one hint', async ($: any, on: any) => {
  await withWorker($, on)
  const { ui, texts } = await mount($)
  const button = await ui.find({ key: KEY })
  expect(button?.type).toBe('Button')
  expect(texts.some(t => t.trim() === 'relais:relais-worker-sonnet-medium')).toBe(true)
  const hints = texts.filter(t => t.includes('to manage'))
  expect(hints.length).toBe(1)
  expect(hints[0].startsWith('Click or Enter an agent')).toBe(true)
  expect(Array.from(hints[0]).length).toBeLessThanOrEqual(70)
})

test('an agent known only by a continue has a Button and no name row', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  engine.stream.push('stdout', phase(0, 'running') + started(1, 'd1') + continueLine('d1', 'agent-1'))
  await settle(engine)
  await tick(engine, 2)
  const { ui, texts } = await mount($)
  expect((await ui.find({ key: KEY }))?.type).toBe('Button')
  expect(texts.some(t => t.includes('relais:relais-'))).toBe(false)
})

test('an agent not yet known is plain text: no Button, no hint', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  engine.stream.push('stdout', phase(0, 'running') + started(1, 'd1'))
  await settle(engine)
  await tick(engine, 2)
  const { ui, texts } = await mount($)
  expect((await ui.findAll({ type: 'Button' })).length).toBe(0)
  expect(texts.some(t => t.includes('● worker'))).toBe(true)
  expect(texts.some(t => t.includes('to manage'))).toBe(false)
})

test('a run’s fourth agent sits under +n more agents and is not pressable', async ($: any, on: any) => {
  await withWorker($, on, 4)
  const { ui, texts } = await mount($)
  expect((await ui.findAll({ type: 'Button' })).length).toBe(3)
  expect(await ui.find({ key: 'agent:agent-4' })).toBeUndefined()
  expect(texts.some(t => t.includes('+1 more agents'))).toBe(true)
})

test('pressing shows loading at once, then the header and the rows after a tick', async ($: any, on: any) => {
  const engine = await withWorker($, on)
  engine.script.messages = () => ROWS
  const ui = await open($, engine)
  let drawn = await mount($)
  expect(drawn.texts.some(t => t.includes('loading'))).toBe(true)
  expect(engine.calls.messages.length).toBe(0)
  await tick(engine)
  drawn = await mount($)
  expect(engine.calls.messages).toEqual([{ agentId: 'agent-1' }])
  const header = drawn.texts.find(t => t.startsWith('run '))!
  expect(header).toMatch(/^run .+ · worker .+ · running · sonnet@medium · \d+:\d\d$/)
  expect(drawn.texts).toContain('person │ Migrate to pnpm')
  expect(drawn.texts.some(t => t.startsWith('▸ Read package.json'))).toBe(true)
  const fail = drawn.rows.find((r: any) => r.text.startsWith('FAIL '))
  expect(fail?.props.color).toBe('error')
  expect(drawn.texts.some(t => t.endsWith('… running'))).toBe(true)
  expect((await drawn.ui.find({ key: 'back' }))?.type).toBe('Button')
  void ui
})

test('a deny is shown, read once and noted once on the run', async ($: any, on: any) => {
  const engine = await withWorker($, on)
  engine.script.messages = () => ({ deny: 'agent has no saved transcript' })
  const ui = await open($, engine)
  await tick(engine, 10)
  expect(engine.calls.messages.length).toBe(1)
  let drawn = await mount($)
  expect(drawn.texts.some(t => t === 'transcript unavailable · agent has no saved transcript')).toBe(true)
  await (await mount($)).ui.press({ key: 'back' })
  drawn = await mount($)
  expect(countOf(drawn.texts, 'transcript unavailable')).toBe(1)
  void ui
})

test('rows that landed stay when a later read is denied, with one dim line', async ($: any, on: any) => {
  const engine = await withWorker($, on)
  engine.script.messages = () => ROWS
  await open($, engine)
  await tick(engine, 2)
  engine.script.messages = () => ({ deny: 'gone' })
  engine.agents[0].status = 'completed'
  engine.stream.push('stdout', ended(10, 'd1', 'agent-1'))
  await settle(engine)
  await tick(engine, 3)
  const drawn = await mount($)
  expect(drawn.texts).toContain('person │ Migrate to pnpm')
  const line = drawn.rows.filter((r: any) => r.text.startsWith('transcript unavailable'))
  expect(line.length).toBe(1)
  expect(line[0].props.dimColor).toBe(true)
  expect(drawn.texts.some(t => t.includes('· completed ·'))).toBe(true)
})

test('a read slower than 5 s shows slow and is noted once', async ($: any, on: any) => {
  const engine = await withWorker($, on)
  engine.script.messagesDelay = 8000
  await open($, engine)
  await tick(engine)
  await engine.clock.advance(5200)
  await settle(engine)
  const drawn = await mount($)
  expect(drawn.texts.some(t => t.startsWith('transcript slow'))).toBe(true)
  expect(engine.calls.messages.length).toBe(1)
  await (await mount($)).ui.press({ key: 'back' })
  expect(countOf((await mount($)).texts, 'transcript slow')).toBe(1)
})

test('a read held across three ticks is one call', async ($: any, on: any) => {
  const engine = await withWorker($, on)
  engine.script.messagesDelay = 300
  engine.script.messages = () => ROWS
  await open($, engine)
  await tick(engine, 6)
  expect(engine.calls.messages.length).toBe(1)
  expect((await mount($)).texts).toContain('person │ Migrate to pnpm')
})

test('a read that never settles is one call, also after back and reopen', async ($: any, on: any) => {
  const engine = await withWorker($, on)
  engine.script.messagesNever = true
  await open($, engine)
  await tick(engine, 30)
  await engine.clock.advance(5000)
  await settle(engine)
  expect(engine.calls.messages.length).toBe(1)
  await (await mount($)).ui.press({ key: 'back' })
  const { ui } = await mount($)
  await ui.press({ key: KEY })
  await tick(engine, 3)
  expect((await mount($)).texts.some(t => t.startsWith('transcript slow'))).toBe(true)
  expect(engine.calls.messages.length).toBe(1)
})

test('back while a read is pending: the runs view stays; reopening reads again', async ($: any, on: any) => {
  const engine = await withWorker($, on)
  engine.script.messagesDelay = 1000
  engine.script.messages = () => ROWS
  await open($, engine)
  await tick(engine)
  await (await mount($)).ui.press({ key: 'back' })
  await engine.clock.advance(1500)
  await settle(engine)
  await tick(engine, 2)
  let drawn = await mount($)
  expect(drawn.texts.some(t => t.includes('● worker'))).toBe(true)
  expect(drawn.texts).not.toContain('person │ Migrate to pnpm')
  engine.script.messagesDelay = 0
  await drawn.ui.press({ key: KEY })
  await tick(engine, 2)
  drawn = await mount($)
  expect(engine.calls.messages.length).toBe(2)
  expect(drawn.texts).toContain('person │ Migrate to pnpm')
})

test('a tool call of the agent is read on the next tick; 900 ms without one is not', async ($: any, on: any) => {
  const engine = await withWorker($, on)
  engine.script.messages = () => ROWS
  await open($, engine)
  await tick(engine)
  expect(engine.calls.messages.length).toBe(1)
  await tick(engine, 8)
  expect(engine.calls.messages.length).toBe(1)
  await $.tool.call({ tool: 'Read', file_path: 'src/a.ts', agentId: 'agent-1' })
  await tick(engine)
  expect(engine.calls.messages.length).toBe(2)
})

test('an agent that finished is read once more, then never', async ($: any, on: any) => {
  const engine = await withWorker($, on)
  engine.script.messages = () => ROWS
  await open($, engine)
  await tick(engine)
  expect(engine.calls.messages.length).toBe(1)
  engine.agents[0].status = 'completed'
  engine.stream.push('stdout', ended(10, 'd1', 'agent-1'))
  await settle(engine)
  await tick(engine, 2)
  expect(engine.calls.messages.length).toBe(2)
  await tick(engine, 30)
  expect(engine.calls.messages.length).toBe(2)
  expect((await mount($)).texts.some(t => t.includes('· completed ·'))).toBe(true)
})

test('another run’s outcome leaves the view alone and updates the status line', async ($: any, on: any) => {
  const engine = await withWorker($, on)
  engine.script.messages = () => ROWS
  const second = engine.feed('second-task.json')
  await $.tool.call({ tool: 'mcp__relais__run', task: 'second-task.json', cwd: '/repo' })
  await startQueued(engine)
  second.push('stdout', event('run-two', 0, { kind: 'phase', state: 'running', reason: 'ok', detail: {} }))
  await settle(engine)
  await open($, engine)
  await tick(engine, 3)
  expect(engine.calls.statuses[engine.calls.statuses.length - 1]).toContain('2 runs')
  second.push('stdout', event('run-two', 1, { kind: 'outcome', state: 'accepted', receipt: null }))
  await settle(engine)
  await tick(engine, 3)
  expect(engine.calls.statuses[engine.calls.statuses.length - 1]).toContain('1 run ')
  const drawn = await mount($)
  expect(drawn.texts).toContain('person │ Migrate to pnpm')
})

test('opened after the run ended: the read lands and the elapsed time stays', async ($: any, on: any) => {
  const engine = await withWorker($, on)
  engine.script.messages = () => ROWS
  engine.agents[0].status = 'completed'
  engine.stream.push('stdout', ended(10, 'd1', 'agent-1') + event(RUN, 11, { kind: 'outcome', state: 'accepted', receipt: null }))
  await settle(engine)
  await tick(engine, 2)
  await open($, engine)
  await tick(engine, 2)
  const first = (await mount($)).texts.find(t => t.startsWith('run '))!
  expect((await mount($)).texts).toContain('person │ Migrate to pnpm')
  await engine.clock.advance(5000)
  await settle(engine)
  const later = (await mount($)).texts.find(t => t.startsWith('run '))!
  expect(later).toBe(first)
})

test('on a live run the elapsed time advances while the agent is shown', async ($: any, on: any) => {
  const engine = await withWorker($, on)
  engine.script.messages = () => ROWS
  await open($, engine)
  await tick(engine, 2)
  const first = (await mount($)).texts.find(t => t.startsWith('run '))!
  await engine.clock.advance(3000)
  await settle(engine)
  const later = (await mount($)).texts.find(t => t.startsWith('run '))!
  expect(later).not.toBe(first)
})

test('a view left in the state by an earlier load is replaced by the runs view', async ($: any, on: any) => {
  const engine = scriptedEngine(on, {
    store: {
      'relais.pane': {
        value: {
          runs: [],
          now: 0,
          shown: { kind: 'agent', agentId: 'agent-1', run: RUN, transcript: { kind: 'loading' } },
        },
        version: 1,
      },
    },
  })
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await settle(engine)
  await tick(engine, 2)
  const { ui } = await mount($)
  expect(await ui.find({ key: 'back' })).toBeUndefined()
})

test('Escape-free return: back shows the runs view with the Buttons again', async ($: any, on: any) => {
  const engine = await withWorker($, on)
  engine.script.messages = () => ROWS
  await open($, engine)
  await tick(engine, 2)
  await (await mount($)).ui.press({ key: 'back' })
  const { ui } = await mount($)
  expect((await ui.find({ key: KEY }))?.type).toBe('Button')
})
