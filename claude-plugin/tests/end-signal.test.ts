// The end signal as a pure function: what one look at `$.agent.list()`
// says about a dispatch.

import { expect, test } from 'claude-code/testing'
import { observe, openDispatch, withTurn } from '../hooks/dispatches.ts'

// A look for the cases that do not depend on time.
const look = { now: 0, seq: 0 }

test('a repair whose stale listing is evicted before the resumed one shows is not ended', () => {
  // Continued while the list still showed the previous run's `completed`.
  let d = openDispatch('d2', 'agent-1', 'completed')
  d = observe(d, { id: 'agent-1', status: 'completed' }, look).dispatch
  // Evicted before the resumed run is listed: no turn, status never moved.
  const { ended } = observe(d, undefined, look)
  expect(ended).toBe(undefined)
})

test('a repair that ran and then vanished with no turn ends failed', () => {
  let d = openDispatch('d2', 'agent-1', 'completed')
  d = observe(d, { id: 'agent-1', status: 'running' }, look).dispatch
  expect(observe(d, undefined, look).ended).toBe('failed')
})

test('a first spawn seen and then gone with no turn ends failed', () => {
  let d = openDispatch('d1', 'agent-1')
  d = observe(d, { id: 'agent-1', status: 'running' }, look).dispatch
  expect(observe(d, undefined, look).ended).toBe('failed')
})

const answered = (answer = 'done', extra: Record<string, unknown> = {}) =>
  withTurn(openDispatch('d1', 'agent-1'), { answer, reason: 'answer', ...extra })
const running = { id: 'agent-1', status: 'running' }

// The first look stamps the answer's time; `later` is a look that many ms after it.
const stampedAt = (d: ReturnType<typeof answered>, now: number) => observe(d, running, { now, seq: 1 }).dispatch
const stopAnswered = (d: ReturnType<typeof answered>) => ({ ...d, isEnding: true, stopSeq: 1, stopSentAt: 0 })

test('an answered agent still running inside the grace is not stopped', () => {
  const d = stampedAt(answered(), 1000)
  const out = observe(d, running, { now: 2000, seq: 2 })
  expect(out.stop).toBe(false)
  expect(out.ended).toBe(undefined)
})

test('an answered agent still running past the grace is stopped', () => {
  const d = stampedAt(answered(), 1000)
  const out = observe(d, running, { now: 6000, seq: 2 })
  expect(out.stop).toBe(true)
  expect(out.dispatch.isEnding).toBe(true)
  expect(out.ended).toBe(undefined)
})

test('a dispatch the plugin stopped ends completed once a later look shows it killed', () => {
  const d = stopAnswered(stampedAt(answered(), 0))
  const out = observe(d, { id: 'agent-1', status: 'killed' }, { now: 7000, seq: 2 })
  expect(out.ended).toBe('completed')
})

test('a dispatch the plugin stopped ends completed once a later look no longer lists it', () => {
  const d = stopAnswered(stampedAt(answered(), 0))
  expect(observe(d, undefined, { now: 7000, seq: 2 }).ended).toBe('completed')
})

test('a look begun before the stop answered confirms nothing', () => {
  const d = stopAnswered(stampedAt(answered(), 0))
  expect(observe(d, { id: 'agent-1', status: 'killed' }, { now: 7000, seq: 1 }).ended).toBe(undefined)
})

test('an aborted turn that is not an answer keeps today’s result', () => {
  const d = withTurn(openDispatch('d1', 'agent-1'), { answer: 'partial', reason: 'aborted', isAborted: true })
  const first = observe(d, running, { now: 0, seq: 1 })
  expect(observe(first.dispatch, running, { now: 60000, seq: 2 }).stop).toBe(false)
  expect(observe(first.dispatch, { id: 'agent-1', status: 'killed' }, { now: 60000, seq: 3 }).ended).toBe('killed')
})

test('a refused stop is not tried again', () => {
  const d = { ...stampedAt(answered(), 0), stopFailed: true }
  expect(observe(d, running, { now: 60000, seq: 2 }).stop).toBe(false)
})

test('the aborted empty turn after the stop keeps the latch and the answer', () => {
  const stopped = stopAnswered(stampedAt(answered('the answer'), 0))
  const after = withTurn(stopped, { answer: '', reason: 'aborted', isAborted: true })
  expect(after.isEnding).toBe(true)
  const out = observe(after, { id: 'agent-1', status: 'killed' }, { now: 7000, seq: 2 })
  expect(out.ended).toBe('completed')
  expect(out.dispatch.answer).toBe('the answer')
})

test('a new answer turn clears the latch', () => {
  const again = withTurn(stopAnswered(stampedAt(answered(), 0)), { answer: 'again', reason: 'answer' })
  expect(again.isEnding).toBe(false)
  expect(again.answeredAt).toBe(undefined)
})

test('a turn with no reason, not aborted, with an answer counts as an answer', () => {
  const d = withTurn(openDispatch('d1', 'agent-1'), { answer: 'done', isAborted: false })
  const stamped = observe(d, running, { now: 0, seq: 1 }).dispatch
  expect(observe(stamped, running, { now: 5000, seq: 2 }).stop).toBe(true)
})

test('a still running agent 30 s after its stop is noted once', () => {
  const d = stopAnswered(stampedAt(answered(), 0))
  const early = observe(d, running, { now: 29000, seq: 2 })
  expect(early.isUnconfirmed).toBe(false)
  const late = observe(early.dispatch, running, { now: 30000, seq: 3 })
  expect(late.isUnconfirmed).toBe(true)
  expect(observe(late.dispatch, running, { now: 32000, seq: 4 }).isUnconfirmed).toBe(false)
})
