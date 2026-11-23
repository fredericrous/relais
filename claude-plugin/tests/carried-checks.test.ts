import { expect, test } from 'claude-code/testing'
import { applyEvent, emptyRun, latestOutput, stepOutput, type RunModel } from '../hooks/timeline.ts'

const T0 = Date.parse('2026-11-23T11:00:00Z')

const fold = (events: Record<string, unknown>[]): RunModel =>
  events.reduce((m, event) => applyEvent(m, event, T0), emptyRun('run-carry', T0))

const verifying = { kind: 'phase', state: 'verifying', reason: 'verification_started', detail: {} }
const started = (label: string) => ({ kind: 'check_started', label, argv: ['make', label] })
const ended = (label: string, exit = 0) => ({ kind: 'check_ended', label, exit, duration_ms: 10 })
const carried = (label: string, from = 'attempt1') => ({ kind: 'check_carried', label, from })

test('a carried check is a non-running pass that names its attempt and leaves detail and wait alone', () => {
  const m = fold([
    verifying,
    started('lint@1'),
    ended('lint@1'),
    { kind: 'heartbeat', alive_at: '2026-11-23T11:00:10Z', progress_at: '2026-11-23T11:00:09Z', signal: 'cpu' },
    carried('make@2'),
  ])
  const step = m.steps[m.steps.length - 1]
  const check = step.checks[1]
  expect(check).toMatchObject({ label: 'make@2', running: false, exit: 0, carried: 'attempt1' })
  expect(step.detail).toBe('make lint@1')
  expect(step.checks[0].carried).toBeUndefined()
  expect(m.wait).toBe('alive, progressing (cpu time)')
})

test('a replay of started, ended, carried lists the checks in profile order, none running', () => {
  const m = fold([verifying, started('lint@1'), ended('lint@1'), carried('make@2'), carried('test@3')])
  const step = m.steps[m.steps.length - 1]
  expect(step.checks.map(c => c.label)).toEqual(['lint@1', 'make@2', 'test@3'])
  expect(step.checks.some(c => c.running)).toBe(false)
})

test('a carried check with no verifying step is a no-op and never reopens a closed step', () => {
  expect(fold([carried('make@2')]).steps).toEqual([])
  const closed = fold([
    verifying,
    started('lint@1'),
    ended('lint@1'),
    { kind: 'phase', state: 'accepted', reason: 'accepted', detail: {} },
  ])
  const after = applyEvent(closed, carried('make@2'), T0)
  expect(after.steps).toEqual(closed.steps)
})

test('an unknown event kind is still skipped', () => {
  const m = fold([verifying, started('lint@1'), { kind: 'check_from_the_future', label: 'x' }])
  expect(m.steps[m.steps.length - 1].checks.map(c => c.label)).toEqual(['lint@1'])
})

test('five carried passes after a re-run check are five rows in profile order, none running', () => {
  const labels = ['a@1', 'b@2', 'c@3', 'd@4', 'e@5']
  const m = fold([verifying, started('lint@0'), ended('lint@0'), ...labels.map(l => carried(l))])
  const step = m.steps[m.steps.length - 1]
  expect(step.checks.map(c => c.label)).toEqual(['lint@0', ...labels])
  expect(step.checks.slice(1).every(c => c.carried === 'attempt1' && !c.running)).toBe(true)
})

test('a carried event with no label or no source attempt is a no-op', () => {
  const before = fold([verifying, started('lint@1'), ended('lint@1')])
  expect(applyEvent(before, { kind: 'check_carried', from: 'attempt1' }, T0).steps).toEqual(before.steps)
  expect(applyEvent(before, { kind: 'check_carried', label: 'make@2' }, T0).steps).toEqual(before.steps)
})

test('the output shown after a carry is the re-run check, not a carried pass', () => {
  const m = fold([
    verifying,
    started('lint@1'),
    { kind: 'output', label: 'lint@1', text: 'error: lint failed\n', elided_bytes: 0 },
    ended('lint@1', 1),
    carried('make@2'),
  ])
  const step = m.steps[m.steps.length - 1]
  expect(stepOutput(step).join('\n')).toContain('lint failed')
  expect(latestOutput(m, 5).join('\n')).toContain('lint failed')
})

test('a step whose every check was carried shows its agent activity, not an empty row', () => {
  const m = fold([verifying, carried('make@2'), carried('test@3')])
  const step = m.steps[m.steps.length - 1]
  expect(stepOutput(step)).toBe(step.activity)
})
