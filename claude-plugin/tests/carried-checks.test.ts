import { expect, test } from 'claude-code/testing'
import { applyEvent, emptyRun, type RunModel } from '../hooks/timeline.ts'

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
