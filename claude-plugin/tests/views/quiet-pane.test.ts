// Quiet runs in the pane: the watcher's protocol lines are fed through the
// fake's `quiet` branch (support.ts), so `calls.spawn` stays runs only.

import { expect, test } from 'claude-code/testing'
import { event, quietLine, scriptedEngine, settle, spawnLine, startedRun } from '../support.ts'

const RUN = 'run-65d322006dd13-c35c'
const A = 'session-1'

const started = (id: string, check = 'clippy', session: string | null = A, tool = 'amont') =>
  quietLine('started', { id, session, tool, check, repo: '/repo' })
const lines = (id: string, ...text: string[]) => quietLine('output', { id, lines: text })
const ended = (id: string, exit: number | null) => quietLine('ended', { id, exit })

// A session with the plugin loaded and the quiet watcher up, no relais run.
async function quietSession($: any, on: any, options: { isPlaced?: boolean; session?: string } = {}) {
  const engine = scriptedEngine(on, { ...options, quiet: true })
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await settle(engine)
  return engine
}

async function pass(engine: any, ms: number) {
  await engine.clock.advance(ms)
  await settle(engine)
}

// Mounts the pane after a tick has written its state.
async function drawn($: any, engine: any, bodyRows = 40) {
  await pass(engine, 200)
  const ui = await $.ui.mount({
    plugin: 'relais',
    surface: 'terminal',
    component: 'Pane',
    requestId: 'relais',
    props: {
      title: 'relais',
      isFocused: false,
      bodyColumns: 72,
      placement: 'dock',
      scroll: { offset: 0, bodyRows },
      view: {},
    },
    viewport: { columns: 100, rows: 40, isFullscreen: true },
  })
  const rows = (await ui.findAll({})).filter((e: any) => e.type === 'Text' || e.type === 'Button')
  const snapshot = rows.map((r: any) => ({ type: r.type, text: r.text as string, props: { ...r.props } }))
  await ui.unmount()
  return { rows: snapshot, texts: snapshot.map((r: any) => r.text as string) }
}

const lastStatus = (engine: any) => engine.calls.statuses[engine.calls.statuses.length - 1] as string | undefined

test('a quiet run past 1 s draws a dim head and its last lines; before 1 s nothing', async ($: any, on: any) => {
  const engine = await quietSession($, on)
  engine.quiet.push('stdout', started('q1') + lines('q1', 'Checking amont', 'Compiling x'))
  await settle(engine)
  expect((await drawn($, engine)).texts).toEqual(['relais · waiting for the first event'])
  await pass(engine, 1000)
  const { texts, rows } = await drawn($, engine)
  expect(texts).toEqual(['amont · clippy · 0:01', '  │ Checking amont', '  │ Compiling x'])
  expect(rows.every((r: any) => r.type === 'Text' && r.props.dimColor === true)).toBe(true)
})

test('a quiet run with no line yet says it waits for output', async ($: any, on: any) => {
  const engine = await quietSession($, on)
  engine.quiet.push('stdout', started('q1'))
  await pass(engine, 1200)
  const { texts } = await drawn($, engine)
  expect(texts[0]).toMatch(/^amont · clippy · 0:0\d$/)
  expect(texts[1]).toBe('  │ waiting for output')
})

test('quiet rows sit below every relais run, as Text, and the agent Buttons keep their order', async ($: any, on: any) => {
  const engine = await startedRun($, on, { quiet: true })
  engine.stream.push(
    'stdout',
    event(RUN, 0, { kind: 'phase', state: 'running', reason: 'ok', detail: {} }) +
      event(RUN, 1, { kind: 'dispatch_started', dispatch: 'd1', agent_kind: 'worker', attempt: 1, model: 'sonnet', effort: 'medium' }) +
      spawnLine('d1'),
  )
  await settle(engine)
  const before = await drawn($, engine)
  const buttons = before.rows.filter((r: any) => r.type === 'Button').map((r: any) => r.text)
  expect(buttons.length).toBeGreaterThan(0)
  engine.quiet.push('stdout', started('q1') + lines('q1', 'one'))
  await pass(engine, 1200)
  const after = await drawn($, engine)
  expect(after.rows.filter((r: any) => r.type === 'Button').map((r: any) => r.text)).toEqual(buttons)
  const head = after.texts.findIndex(t => t.startsWith('amont · clippy'))
  expect(head).toBeGreaterThan(after.texts.findIndex(t => t.startsWith('cost')))
  expect(head).toBe(after.texts.length - 2)
  expect(after.rows.filter((r: any) => r.text.includes('clippy') || r.text.includes('│ one')).every((r: any) => r.type === 'Text')).toBe(true)
})

test('a passed run drops on ended', async ($: any, on: any) => {
  const engine = await quietSession($, on)
  engine.quiet.push('stdout', started('q1') + lines('q1', 'ok'))
  await pass(engine, 1200)
  expect((await drawn($, engine)).texts.length).toBe(2)
  engine.quiet.push('stdout', ended('q1', 0))
  expect((await drawn($, engine)).texts).toEqual(['relais · waiting for the first event'])
})

test('a failed run stays in the error colour until the same session, tool and check starts again', async ($: any, on: any) => {
  const engine = await quietSession($, on)
  engine.quiet.push('stdout', started('q1') + lines('q1', 'error: boom') + ended('q1', 1))
  await pass(engine, 200)
  let view = await drawn($, engine)
  expect(view.texts).toEqual(['amont · clippy · 0:00', '  │ error: boom'])
  expect(view.rows[0].props.color).toBe('error')
  // A sibling check, another tool's check and another session's run leave it.
  engine.quiet.push('stdout', started('q2', 'test'))
  engine.quiet.push('stdout', started('q3', 'clippy', 'other-session'))
  engine.quiet.push('stdout', started('q4', 'clippy', A, 'amont-agent'))
  await pass(engine, 200)
  view = await drawn($, engine)
  expect(view.texts.some(t => t === 'amont · clippy · 0:00')).toBe(true)
  expect(view.rows.find((r: any) => r.text === 'amont · clippy · 0:00').props.color).toBe('error')
  // The next amont clippy run of the session clears it.
  engine.quiet.push('stdout', started('q5'))
  await pass(engine, 200)
  view = await drawn($, engine)
  expect(view.rows.some((r: any) => r.props.color === 'error')).toBe(false)
  expect(view.texts.some(t => t.includes('error: boom'))).toBe(false)
})

test('a killed run (null exit) is drawn as failed', async ($: any, on: any) => {
  const engine = await quietSession($, on)
  engine.quiet.push('stdout', started('q1') + ended('q1', null))
  const { rows } = await drawn($, engine)
  expect(rows[0].props.color).toBe('error')
})

test('a sub-second pass never draws or opens the pane; a sub-second failure does both', async ($: any, on: any) => {
  const engine = await quietSession($, on)
  const opened = engine.calls.opened.length
  engine.quiet.push('stdout', started('q1') + ended('q1', 0))
  await pass(engine, 1500)
  expect((await drawn($, engine)).texts).toEqual(['relais · waiting for the first event'])
  expect(engine.calls.opened.length).toBe(opened)
  engine.quiet.push('stdout', started('q2', 'test') + lines('q2', 'FAILED') + ended('q2', 101))
  const { texts } = await drawn($, engine)
  expect(texts[1]).toBe('  │ FAILED')
  expect(engine.calls.opened.length).toBe(opened + 1)
})

test('the first run past 1 s opens the pane on a tick with no further line; a second does not toast again', async ($: any, on: any) => {
  const engine = await quietSession($, on, { isPlaced: false })
  const opened = engine.calls.opened.length
  engine.quiet.push('stdout', started('q1'))
  await pass(engine, 500)
  expect(engine.calls.opened.length).toBe(opened)
  await pass(engine, 700)
  expect(engine.calls.opened.length).toBe(opened + 1)
  const toast = 'amont clippy running: /relais-status'
  expect(engine.calls.toasts.filter(t => t === toast).length).toBe(1)
  engine.quiet.push('stdout', started('q2', 'test'))
  await pass(engine, 1500)
  expect(engine.calls.opened.length).toBe(opened + 1)
  expect(engine.calls.toasts.filter(t => t.endsWith('running: /relais-status')).length).toBe(1)
})

test('runs of sessions not served are never drawn; a session added later shows its earlier run', async ($: any, on: any) => {
  const options = { session: 'A', quiet: true }
  const engine = scriptedEngine(on, options)
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await settle(engine)
  engine.quiet.push('stdout', started('qa', 'clippy', 'A') + started('qb', 'test', 'B') + started('qc', 'fmt', 'C') + started('qn', 'lint', null))
  await pass(engine, 1500)
  let texts = (await drawn($, engine)).texts
  expect(texts.filter(t => t.startsWith('amont ·'))).toEqual([expect.stringContaining('clippy')])
  // /clear: the module serves a new session id.
  options.session = 'C'
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await pass(engine, 200)
  texts = (await drawn($, engine)).texts
  expect(texts.some(t => t.includes('clippy'))).toBe(true)
  expect(texts.some(t => t.includes('fmt'))).toBe(true)
  expect(texts.some(t => t.includes('test'))).toBe(false)
  expect(texts.some(t => t.includes('lint'))).toBe(false)
})

test('one protocol line split across two chunks folds correctly', async ($: any, on: any) => {
  const engine = await quietSession($, on)
  const whole = started('q1') + lines('q1', 'split line')
  const cut = whole.indexOf('split') + 3
  engine.quiet.push('stdout', whole.slice(0, cut))
  await settle(engine)
  engine.quiet.push('stdout', whole.slice(cut))
  await pass(engine, 1200)
  expect((await drawn($, engine)).texts).toContain('  │ split line')
})

test('heartbeat lines do not mark the store dirty', async ($: any, on: any) => {
  const engine = await quietSession($, on)
  await pass(engine, 500)
  const flushes = engine.calls.statuses.length
  engine.quiet.push('stdout', quietLine('heartbeat') + quietLine('heartbeat'))
  await pass(engine, 1500)
  expect(engine.calls.statuses.length).toBe(flushes)
  engine.quiet.push('stdout', started('q1'))
  await pass(engine, 300)
  expect(engine.calls.statuses.length).toBeGreaterThan(flushes)
})

test('a watcher with no quiet run leaves the empty state unchanged and the pane idle', async ($: any, on: any) => {
  const engine = await quietSession($, on)
  await pass(engine, 500)
  const flushes = engine.calls.statuses.length
  await pass(engine, 5000)
  expect(engine.calls.statuses.length).toBe(flushes)
  expect((await drawn($, engine)).texts).toEqual(['relais · waiting for the first event'])
})

test('6 concurrent quiet runs draw 3 heads and a +3 more line', async ($: any, on: any) => {
  const engine = await quietSession($, on)
  const all = [1, 2, 3, 4, 5, 6].map(n => started(`q${n}`, `check${n}`) + lines(`q${n}`, `out ${n}`)).join('')
  engine.quiet.push('stdout', all)
  await pass(engine, 1500)
  const { texts } = await drawn($, engine)
  expect(texts.filter(t => t.startsWith('amont · check')).length).toBe(3)
  expect(texts).toContain('+3 more quiet runs')
  // Live first, oldest first.
  expect(texts[0]).toContain('check1')
})

test('live runs come before kept failures, and the status line lists both', async ($: any, on: any) => {
  const engine = await quietSession($, on)
  engine.quiet.push('stdout', started('q1', 'test') + ended('q1', 1) + started('q2', 'clippy'))
  await pass(engine, 1500)
  const { texts } = await drawn($, engine)
  expect(texts.filter(t => t.startsWith('amont ·')).map(t => t.split(' · ')[1])).toEqual(['clippy', 'test'])
  expect(lastStatus(engine)).toMatch(/amont clippy 0:0\d, amont test failed 0:0\d/)
})

const masked = (texts: string[]) => texts.map(t => t.replace(/\d+:\d\d/g, 'm:ss').replace(/[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏]/g, '*'))

for (const rows of [8, 24]) {
  test(`relais rows are the same with or without a quiet run at ${rows} rows`, async ($: any, on: any) => {
    const engine = await startedRun($, on, { quiet: true })
    const many = Array.from({ length: 30 }, (_, i) => `out-${i}`).join('\n') + '\n'
    engine.stream.push(
      'stdout',
      event(RUN, 0, { kind: 'phase', state: 'verifying', reason: 'ok', detail: {} }) +
        event(RUN, 1, { kind: 'check_started', label: 'unit', argv: ['t'] }) +
        event(RUN, 2, { kind: 'output', label: 'unit', text: many, elided_bytes: 0 }),
    )
    await settle(engine)
    const baseline = masked((await drawn($, engine, rows)).texts)
    engine.quiet.push('stdout', started('q1') + lines('q1', ...Array.from({ length: 12 }, (_, i) => `q-${i}`)))
    await pass(engine, 1500)
    const withQuiet = masked((await drawn($, engine, rows)).texts)
    // The relais rows come first and are untouched; the quiet run gets only
    // the rows they leave, right below them, and the pane never grows past
    // its body.
    expect(withQuiet.slice(0, baseline.length)).toEqual(baseline)
    expect(withQuiet.length).toBeLessThanOrEqual(Math.max(rows, 6))
    if (baseline.length < Math.max(rows, 6)) expect(withQuiet[baseline.length]).toMatch(/^amont · clippy/)
    else expect(withQuiet.length).toBe(baseline.length)
  })
}

test('watcher exit 2 is noted without a respawn and nothing is drawn', async ($: any, on: any) => {
  const engine = await quietSession($, on)
  engine.quiet.end(2)
  await pass(engine, 90_000)
  expect(engine.calls.quiet.length).toBe(1)
  expect((await drawn($, engine)).texts).toEqual(['relais · waiting for the first event'])
})

test('a crashed watcher is respawned after 30 s, and says it is down meanwhile', async ($: any, on: any) => {
  const engine = await quietSession($, on)
  engine.quiet.end(1)
  await pass(engine, 200)
  expect((await drawn($, engine)).texts).toEqual(['relais · waiting for the first event', 'quiet watcher down'])
  await pass(engine, 28_000)
  expect(engine.calls.quiet.length).toBe(1)
  await pass(engine, 2_500)
  expect(engine.calls.quiet.length).toBe(2)
  expect((await drawn($, engine)).texts).toEqual(['relais · waiting for the first event'])
})
