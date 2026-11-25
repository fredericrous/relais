// The plugin's own stop of an answered agent, as a unit: `check` with a fake
// fx whose clock the test moves by hand. A 30 s bound then costs one step,
// not fifteen 2 s polls of the scripted engine, which the runner's 5 s limit
// per test cannot hold on a loaded machine.

import { expect, test } from 'claude-code/testing'
import { check } from '../hooks/agents.ts'
import { openDispatch, withTurn } from '../hooks/dispatches.ts'
import { createStore, type Child } from '../hooks/store.ts'
import { emptyRun } from '../hooks/timeline.ts'

const ANSWER = 'DONE. I will wait for the completion notice.'
const PLUGIN_NOTE =
  'relais plugin: the worker ended its turn with background work still running; it was stopped before the snapshot, and its output was not read.'

// Lets every pending promise chain run.
const flush = async () => {
  for (let i = 0; i < 20; i++) await Promise.resolve()
}

function rig() {
  const state = { now: 0, status: 'running', stops: 0 }
  const timers: { at: number; fn: () => void; isLive: boolean }[] = []
  let releaseStop: () => void = () => {}
  const stopAnswered = new Promise<void>(resolve => (releaseStop = resolve))
  const stopped: any[] = []
  const fx: any = {
    clock: {
      now: async () => state.now,
      after: (ms: number, fn: () => void) => {
        const timer = { at: state.now + ms, fn, isLive: true }
        timers.push(timer)
        return { cancel: () => (timer.isLive = false) }
      },
    },
    agent: { list: async () => [{ id: 'agent-1', status: state.status }] },
    tool: {
      call: async (request: any) => {
        if (request.tool !== 'TaskStop') return {}
        state.stops += 1
        await stopAnswered
        return { result: 'stopped' }
      },
    },
    process: {
      run: async (argv: string[], init: any) => {
        if (argv[2] === 'stopped') stopped.push(JSON.parse(init.stdin))
        return { exitCode: 0 }
      },
    },
    env: { home: async () => '/home' },
  }
  const store = createStore()
  const child: Child = { session: 's', cwd: '/work', argv: '[]', exited: false, key: 'run-1' }
  const dispatch = withTurn(openDispatch('d1', 'agent-1'), { answer: ANSWER, reason: 'answer' })
  store.dispatches.set('d1', { ...dispatch, isBound: true })
  store.dispatchChild.set('d1', child)
  store.agentDispatch.set('agent-1', 'd1')
  store.models = { 'run-1': emptyRun('run-1', 0) }
  // One look at `ms` from now: due timers fire first, as the engine's clock would.
  const lookAt = async (ms: number) => {
    state.now += ms
    for (const timer of timers.filter(t => t.isLive && t.at <= state.now)) {
      timer.isLive = false
      timer.fn()
    }
    await flush()
    await check(fx, store)
    await flush()
  }
  const notes = () => store.models['run-1'].events.filter(e => e.kind === 'stderr').map(e => e.text)
  return { state, lookAt, releaseStop, stopped, notes }
}

test('a stop held past 30 s is noted once, and when it takes late the dispatch ends completed with the answer', async () => {
  const { state, lookAt, releaseStop, stopped, notes } = rig()
  await lookAt(0)
  await lookAt(6000)
  expect(state.stops).toBe(1)
  await lookAt(31000)
  expect(stopped.length).toBe(0)
  const late = notes().filter(text => text.includes('no answer in 30 s; it may still take'))
  expect(late).toEqual([
    'relais plugin: the stop of agent agent-1 after its answer had no answer in 30 s; it may still take, else the run ends when its background work ends or at the wall time.',
  ])
  // The call was not cancelled: the engine carries the stop out late.
  releaseStop()
  state.status = 'killed'
  await lookAt(2000)
  expect(stopped.length).toBe(1)
  expect(stopped[0].status).toBe('completed')
  expect(stopped[0].answer).toBe(`${ANSWER}\n\n${PLUGIN_NOTE}`)
  expect(state.stops).toBe(1)
  expect(notes().filter(text => text.includes('still runs 30 s')).length).toBe(0)
})

test('an agent still running 30 s after its stop answered is noted once, and nothing is sent', async () => {
  const { state, lookAt, releaseStop, stopped, notes } = rig()
  releaseStop()
  await lookAt(0)
  await lookAt(6000)
  expect(state.stops).toBe(1)
  for (let i = 0; i < 4; i++) await lookAt(10000)
  const still = notes().filter(text => text.includes('still runs 30 s after its stop'))
  expect(still).toEqual([
    'relais plugin: agent agent-1 still runs 30 s after its stop; it ends with its shell or at the wall time. To end it now, stop it in Claude Code.',
  ])
  expect(stopped.length).toBe(0)
  expect(state.stops).toBe(1)
})
