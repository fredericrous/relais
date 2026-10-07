// The end signal as a pure function: what one look at `$.agent.list()`
// says about a dispatch.

import { expect, test } from 'claude-code/testing'
import { observe, openDispatch } from '../hooks/dispatches.ts'

test('a repair whose stale listing is evicted before the resumed one shows is not ended', () => {
  // Continued while the list still showed the previous run's `completed`.
  let d = openDispatch('d2', 'agent-1', 'completed')
  d = observe(d, { id: 'agent-1', status: 'completed' }).dispatch
  // Evicted before the resumed run is listed: no turn, status never moved.
  const { ended } = observe(d, undefined)
  expect(ended).toBe(undefined)
})

test('a repair that ran and then vanished with no turn ends failed', () => {
  let d = openDispatch('d2', 'agent-1', 'completed')
  d = observe(d, { id: 'agent-1', status: 'running' }).dispatch
  expect(observe(d, undefined).ended).toBe('failed')
})

test('a first spawn seen and then gone with no turn ends failed', () => {
  let d = openDispatch('d1', 'agent-1')
  d = observe(d, { id: 'agent-1', status: 'running' }).dispatch
  expect(observe(d, undefined).ended).toBe('failed')
})
