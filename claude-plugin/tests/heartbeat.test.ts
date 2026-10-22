import { expect, test } from 'claude-code/testing'
import { applyEvent, emptyRun, heartbeatText, statusReply, timelineText } from '../hooks/timeline.ts'

const T0 = Date.parse('2026-10-06T11:00:00Z')
const fold = (event: Record<string, unknown>) => applyEvent(emptyRun('run-h', T0), event, T0)

test('a heartbeat with fresh progress reads alive and names the signal as text', () => {
  const text = heartbeatText({
    kind: 'heartbeat',
    alive_at: '2026-10-06T11:10:00Z',
    progress_at: '2026-10-06T11:09:30Z',
    signal: 'cpu',
  })
  expect(text).toBe('alive, progressing (cpu time)')
})

test('a heartbeat with stale progress reads alive, no progress for 14m', () => {
  const m = fold({
    kind: 'heartbeat',
    alive_at: '2026-10-06T11:14:30Z',
    progress_at: '2026-10-06T11:00:00Z',
    signal: 'log_bytes',
  })
  expect(m.wait).toBe('alive, no progress for 14m (last log output)')
  expect(timelineText(m, T0)).toContain('alive, no progress for 14m (last log output)')
  expect(statusReply(m, T0).waiting).toContain('no progress for 14m')
  expect(m.events.some(e => e.kind === 'heartbeat')).toBe(true)
})

test('waiting_for_slot reads as text with its class, position and wait', () => {
  const m = fold({ kind: 'waiting_for_slot', class: 'heavy_local', position: 2, waited_s: 90 })
  expect(m.wait).toBe('waiting for a heavy local slot, position 2 · 90s')
  expect(fold({ kind: 'waiting_for_slot', class: 'host_slot', position: 0, waited_s: 300 }).wait).toBe(
    'waiting for a host slot · 5m',
  )
  expect(fold({ kind: 'waiting_for_slot', class: 'coordinator_unreachable', position: 0, waited_s: 90 }).wait).toBe(
    'coordinator unreachable, retrying · 90s',
  )
})

test('waiting_for_disk reads as text with the volume and the floor', () => {
  const g = 2 ** 30
  const m = fold({ kind: 'waiting_for_disk', volume: '/work', available: 2 * g, floor: 8 * g, reserved: 0 })
  expect(m.wait).toBe('waiting for disk · 2.0 GiB free of 8.0 GiB floor on /work')
})

test('flaky and resource_exhausted are rows of text, and a check ending clears the wait', () => {
  const flaky = fold({ kind: 'flaky', label: 'make@ab12', attempts: 2 })
  expect(flaky.events[0].text).toBe('flaky · make@ab12 passed after 2 attempts')
  const exhausted = fold({ kind: 'resource_exhausted', label: 'make@ab12', detail: 'out of memory' })
  expect(exhausted.events[0].text).toBe('resource exhausted · make@ab12 · out of memory')
  const waiting = fold({ kind: 'waiting_for_slot', class: 'host_slot', position: 0, waited_s: 30 })
  expect(applyEvent(waiting, { kind: 'check_started', label: 'x', argv: [] }, T0).wait).toBeUndefined()
})
