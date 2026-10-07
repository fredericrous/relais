// The pane view: what the timeline draws. Modelled on the built-in diff
// mod's pane-view test: mount the `Pane` component through the plugin, read
// the drawing by element.

import { expect, test } from 'claude-code/testing'
import { continueLine, event, settle, spawnLine, startedRun, startQueued } from '../support.ts'

const RUN = 'run-65d322006dd13-c35c'

const phase = (seq: number, state: string, reason = 'ok') => event(RUN, seq, { kind: 'phase', state, reason, detail: {} })
const step = (seq: number, name: string, detail = '') => event(RUN, seq, { kind: 'step', name, detail })
const started = (seq: number, dispatch: string, attempt: number | null = 1) =>
  event(RUN, seq, { kind: 'dispatch_started', dispatch, agent_kind: 'worker', attempt, model: 'sonnet', effort: 'medium' })
const checkStarted = (seq: number, label: string) =>
  event(RUN, seq, { kind: 'check_started', label, argv: ['python3', '-m', 'unittest', '-q'] })
const output = (seq: number, label: string, text: string) =>
  event(RUN, seq, { kind: 'output', label, text, elided_bytes: 0 })

// Feeds events, lets the 100 ms tick write the state, and mounts the pane.
async function drawn($: any, engine: any, lines: string[], room: { bodyRows?: number; bodyColumns?: number } = {}) {
  if (lines.length > 0) engine.stream.push('stdout', lines.join(''))
  await settle(engine)
  await engine.clock.advance(200)
  await settle(engine)
  const ui = await $.ui.mount({
    plugin: 'relais',
    surface: 'terminal',
    component: 'Pane',
    requestId: 'relais',
    props: {
      title: 'relais',
      isFocused: false,
      bodyColumns: room.bodyColumns ?? 72,
      placement: 'dock',
      scroll: { offset: 0, bodyRows: room.bodyRows ?? 40 },
      view: {},
    },
    viewport: { columns: 100, rows: 40, isFullscreen: true },
  })
  const texts = (await ui.findAll({ type: 'Text' })).map((t: any) => t.text as string)
  return { ui, texts, rows: await ui.findAll({ type: 'Text' }) }
}

const indexOf = (texts: string[], needle: string) => texts.findIndex(t => t.includes(needle))

test('before the first event the pane says it is waiting', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  await engine.clock.advance(200)
  await settle(engine)
  const { texts } = await drawn($, engine, [])
  expect(texts.some(t => t.includes('waiting for the first event'))).toBe(true)
  expect(texts.some(t => t.includes('starting-'))).toBe(false)
})

test('phases draw in order, finished ones on one line, the current one expanded', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  // The shapes relais emits: steps that are not transitions, a baseline
  // check labelled by its program, then the transitions.
  const { texts } = await drawn($, engine, [
    step(0, 'preflight'),
    event(RUN, 1, { kind: 'step', name: 'preflight', detail: 'route implementation · sonnet@medium', max_attempts: 3 }),
    step(2, 'baseline'),
    checkStarted(3, 'check.sh@4a7b'),
    event(RUN, 4, { kind: 'check_ended', label: 'check.sh@4a7b', exit: 1, duration_ms: 900 }),
    step(5, 'baseline', '1 check · 1 red on base'),
    step(6, 'worktree · setup'),
    phase(7, 'running'),
    started(8, 'd1'),
    event(RUN, 9, { kind: 'dispatch_ended', dispatch: 'd1', outcome: 'exit 0', usage: null, cost: null }),
    phase(10, 'verifying'),
    checkStarted(11, 'unit'),
    output(12, 'unit', 'test_greet ... ok\nRan 1 test\nOK\n'),
  ])
  const order = ['preflight', 'baseline', 'worktree · setup', 'attempt 1 · worker', 'verification'].map(name =>
    indexOf(texts, name),
  )
  expect(order.every(i => i >= 0)).toBe(true)
  expect([...order].sort((a, b) => a - b)).toEqual(order)
  // Finished phases are one line; the current one carries its live output.
  expect(texts[order[0]].startsWith('✓')).toBe(true)
  expect(texts[order[0]]).toContain('route implementation · sonnet@medium')
  expect(texts[order[1]]).toContain('1 check · 1 red on base')
  expect(texts[order[4]].startsWith('▸')).toBe(true)
  const outputAt = indexOf(texts, '│ test_greet ... ok')
  expect(outputAt).toBeGreaterThan(order[4])
  expect(indexOf(texts, '· review')).toBeGreaterThan(outputAt)
  expect(indexOf(texts, '· receipt')).toBeGreaterThan(indexOf(texts, '· review'))
  expect(texts[0]).toContain('verifying')
  expect(texts[0]).toContain('attempt 1/3')
  expect(texts.some(t => t.includes('completed'))).toBe(true)
})

test('review and receipt steps replace their placeholders', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  const { texts } = await drawn($, engine, [
    step(0, 'preflight'),
    phase(1, 'running'),
    started(2, 'd1'),
    phase(3, 'verifying'),
    step(4, 'review', 'strong tier'),
    event(RUN, 5, { kind: 'decision', what: 'accept', reason: 'checks_and_review_passed' }),
    step(6, 'receipt', '/state/runs/r/receipt.json'),
    step(7, 'worktree · setup'),
  ])
  expect(texts.some(t => t.startsWith('decision accept · checks_and_review_passed'))).toBe(true)
  // A step with no detail shows its time alone, with no dangling separator.
  expect(texts[indexOf(texts, 'worktree · setup')]).not.toContain(' · ·')
  expect(texts[indexOf(texts, 'worktree · setup')].trimEnd().endsWith('·')).toBe(false)
  expect(indexOf(texts, '· review')).toBe(-1)
  expect(indexOf(texts, '· receipt')).toBe(-1)
  expect(texts[indexOf(texts, 'review')].startsWith('✓')).toBe(true)
  expect(texts[indexOf(texts, 'receipt')]).toContain('/state/runs/r/receipt.json')
})

test('the live output is capped by the rows the pane gets; the extra lines are dropped', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  const many = Array.from({ length: 30 }, (_, i) => `out-${String(i + 1).padStart(2, '0')}`).join('\n') + '\n'
  const lines = [phase(0, 'prepared'), phase(1, 'verifying'), checkStarted(2, 'unit'), output(3, 'unit', many)]
  const { texts } = await drawn($, engine, lines, { bodyRows: 20 })
  const shown = texts.filter(t => t.includes('│ out-'))
  expect(shown.length).toBeGreaterThan(0)
  expect(shown.length).toBeLessThan(30)
  expect(texts.length).toBeLessThanOrEqual(20)
  expect(shown[shown.length - 1]).toContain('out-30')
  expect(texts.some(t => t.includes('out-01'))).toBe(false)
})

test('a very tall pane shows at most the last 40 stored lines', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  const many = Array.from({ length: 80 }, (_, i) => `row-${String(i + 1).padStart(2, '0')}`).join('\n') + '\n'
  const lines = [phase(0, 'prepared'), phase(1, 'verifying'), checkStarted(2, 'unit'), output(3, 'unit', many)]
  const { texts } = await drawn($, engine, lines, { bodyRows: 200 })
  const shown = texts.filter(t => t.includes('│ row-'))
  expect(shown.length).toBe(40)
  expect(shown[0]).toContain('row-41')
})

test('lines are cut before they are stored, and drawn truncated at the end', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  const wide = 'w'.repeat(5000)
  const lines = [phase(0, 'prepared'), phase(1, 'verifying'), checkStarted(2, 'unit'), output(3, 'unit', wide + '\n')]
  const { texts, rows } = await drawn($, engine, lines)
  const stored = texts.find(t => t.includes('│ www'))!
  expect(stored.length).toBeLessThan(260)
  const row = rows.find((r: any) => r.text === stored)
  expect(row.props.wrap).toBe('truncate-end')
})

test('stderr carries its label and the error colour', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  engine.stream.push('stderr', 'thread main panicked at src/main.rs\n')
  const { rows } = await drawn($, engine, [phase(0, 'prepared')])
  const stderr = rows.find((r: any) => r.text.startsWith('stderr'))
  expect(stderr).toBeDefined()
  expect(stderr.text).toContain('panicked')
  expect(stderr.props.color).toBe('error')
})

test('a failure carries the FAIL label and the error colour; a preflight failure shows no agents', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  const { rows } = await drawn($, engine, [
    phase(0, 'prepared'),
    phase(1, 'blocked', 'baseline_unrunnable'),
    event(RUN, 2, { kind: 'outcome', state: 'blocked', receipt: null }),
  ])
  const failed = rows.find((r: any) => r.text.startsWith('FAIL'))
  expect(failed).toBeDefined()
  expect(failed.text).toContain('preflight')
  expect(failed.props.color).toBe('error')
  expect(rows.some((r: any) => r.text.includes('agents  none'))).toBe(true)
  expect(rows.some((r: any) => r.text.includes('outcome blocked'))).toBe(true)
})

test('more agents than fit are summarised, and an unknown cost says so', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  const dispatches = [1, 2, 3, 4, 5].map(n => started(n, `d${n}`, n))
  const { texts } = await drawn($, engine, [phase(0, 'running'), ...dispatches])
  expect(texts.filter(t => t.includes('● worker')).length).toBe(3)
  expect(texts.some(t => t.includes('+2 more agents'))).toBe(true)
  expect(texts.some(t => t.startsWith('cost') && t.includes('unknown'))).toBe(true)
})

test('the outcome row names what the candidate changed', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  const { texts } = await drawn($, engine, [
    phase(0, 'running'),
    event(RUN, 1, {
      kind: 'outcome',
      state: 'accepted',
      receipt: '/runs/r/receipt.json',
      summary: { files_changed: 2, insertions: 5, deletions: 1 },
    }),
  ])
  const row = texts.find(t => t.startsWith('outcome '))
  expect(row).toBe('outcome accepted · 2 files, +5 -1 · receipt /runs/r/receipt.json')
})

test('a known cost is shown with how well it is known', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  const { texts } = await drawn($, engine, [
    phase(0, 'running'),
    event(RUN, 1, { kind: 'cost', booked: 1_400_000 / 100, completeness: 'estimated' }),
  ])
  expect(texts.some(t => t.startsWith('cost') && t.includes('¢1.40 est'))).toBe(true)
})

test('concurrent runs each get a section', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  const second = engine.feed('second task')
  await $.tool.call({ tool: 'mcp__relais__run', task: 'second task', cwd: '/repo' })
  await startQueued(engine)
  second.push('stdout', event('run-two', 0, { kind: 'phase', state: 'running', reason: 'ok', detail: {} }))
  const { texts } = await drawn($, engine, [phase(0, 'verifying')])
  const headers = texts.filter(t => t.startsWith('relais · run'))
  expect(headers.length).toBe(2)
  expect(headers.some(t => t.includes('verifying'))).toBe(true)
  expect(headers.some(t => t.includes('running'))).toBe(true)
})

test('an agent’s status in the footer follows the agent list', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  engine.stream.push('stdout', started(0, 'd1') + spawnLine('d1'))
  await settle(engine)
  engine.agents[0].status = 'completed'
  await engine.clock.advance(2000)
  await settle(engine)
  const { texts } = await drawn($, engine, [])
  expect(texts.some(t => t.includes('● worker') && t.includes('completed'))).toBe(true)
})

test('a repair: the failed verification is FAIL, the next attempt is the repair, one agent row', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  engine.stream.push('stdout', phase(0, 'running') + started(1, 'd1') + spawnLine('d1'))
  await settle(engine)
  const repair = [
    phase(2, 'verifying'),
    checkStarted(3, 'unit'),
    output(4, 'unit', 'FAIL: test_greet\n'),
    event(RUN, 5, { kind: 'check_ended', label: 'unit', exit: 1, duration_ms: 400 }),
    event(RUN, 6, { kind: 'decision', what: 'repair', reason: 'behavioral_failure' }),
    phase(7, 'repairing', 'behavioral_failure'),
    phase(8, 'running'),
    started(9, 'd2', 2),
  ].join('')
  engine.stream.push('stdout', repair + continueLine('d2', 'agent-1'))
  await settle(engine)
  const { texts } = await drawn($, engine, [])
  const failed = indexOf(texts, 'verification')
  expect(texts[failed].startsWith('FAIL')).toBe(true)
  expect(indexOf(texts, 'attempt 2 · repair')).toBeGreaterThan(failed)
  expect(texts.some(t => /^\W*repair\s/.test(t.replace(/^[✓▸] /, '')))).toBe(false)
  const agentRows = texts.filter(t => t.includes('● worker'))
  expect(agentRows.length).toBe(1)
  expect(agentRows[0]).toContain('2 dispatches')
})

test('where the pane is not placed, a toast points to /relais-status, which prints the timeline', async ($: any, on: any) => {
  const engine = await startedRun($, on, { isPlaced: false })
  expect(engine.calls.toasts).toContain('relais run started: /relais-status')
  engine.stream.push('stdout', phase(0, 'prepared') + phase(1, 'running') + started(2, 'd1'))
  await settle(engine)
  const reply = await $.command.run({ command: 'relais-status' })
  expect(reply.text).toContain('preflight')
  expect(reply.text).toContain('attempt 1 · worker')
  expect(reply.text).not.toContain('relais pane opened')
})

test('where the pane is placed there is no toast, and /relais-status opens it', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  expect(engine.calls.toasts).not.toContain('relais run started: /relais-status')
  const before = engine.calls.opened.length
  const reply = await $.command.run({ command: 'relais-status' })
  expect(reply.text).toBe('relais pane opened.')
  expect(engine.calls.opened.length).toBe(before + 1)
  // Opened with focus left out, and never holding the toasts.
  for (const open of engine.calls.opened) {
    expect(open.focus).toBe(undefined)
    expect(open.holdToasts).toBe(undefined)
  }
})

test('the status tool returns phases, decisions, cost and at most 40 output lines', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  const many = Array.from({ length: 70 }, (_, i) => `line ${i + 1}`).join('\n') + '\n'
  engine.stream.push(
    'stdout',
    [
      phase(0, 'prepared'),
      phase(1, 'verifying'),
      event(RUN, 2, { kind: 'decision', what: 'repair', reason: 'checks_failed' }),
      checkStarted(3, 'unit'),
      output(4, 'unit', many),
    ].join(''),
  )
  await settle(engine)
  const reply = await $.tool.call({ tool: 'mcp__relais__status' })
  const [run] = JSON.parse(reply.result)
  expect(run.phase).toBe('verifying')
  expect(run.decisions).toEqual(['repair · checks_failed'])
  expect(run.output.length).toBe(40)
  expect(run.output[39]).toBe('line 70')
  expect(run.phases.map((p: any) => p.title)).toContain('preflight')
})
