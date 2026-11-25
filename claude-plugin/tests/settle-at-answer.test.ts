// A worker that answered but is held open by its own background work is
// stopped by the plugin and reported completed, its answer kept.

import { expect, test } from 'claude-code/testing'
import { event, nativeCalls, settle, spawnLine, startedRun, turn } from './support.ts'

const POLL_MS = 2000
// The `last_assistant_message` of a real SubagentStop that still had a shell running.
const F12_ANSWER =
  "I'm working f12: a session started by a /clear relay now keeps its ticket instead of losing it to another session. PR #561 is open with reviews passed and local checks green; next I merge it once CI goes green."
const PLUGIN_NOTE =
  'relais plugin: the worker ended its turn with background work still running; it was stopped before the snapshot, and its output was not read.'

async function spawned($: any, on: any) {
  const engine = await startedRun($, on)
  engine.stream.push('stdout', event('run-1', 0, { kind: 'phase', state: 'running', reason: 'ok', detail: {} }) + spawnLine('d1'))
  await settle(engine)
  return engine
}

// One poll of 2 s per step with a light settle: the runner gives a test 5 s.
const poll = async (engine: any, times = 1) => {
  for (let i = 0; i < times; i++) {
    await engine.clock.advance(POLL_MS)
    for (let j = 0; j < 3; j++) await engine.clock.settle()
  }
  await settle(engine)
}

const taskStops = (engine: any) => engine.calls.tool.filter((t: any) => t.tool === 'TaskStop')
const stoppedOf = (engine: any) => nativeCalls(engine.calls, 'stopped')
// What the run's timeline says, as the status reply shows it.
const timelineOf = async ($: any) => (await $.tool.call({ tool: 'mcp__relais__status', run: 'run-1' })).result as string

// Resolves when `release` is called.
const gate = () => {
  let release: () => void = () => {}
  const open = new Promise<void>(resolve => (release = resolve))
  return { open, release }
}

test('an answer that settles within the grace is never stopped', async ($: any, on: any) => {
  const engine = await spawned($, on)
  await $.turn.complete(turn('agent-1', {}, F12_ANSWER))
  engine.agents[0].status = 'completed'
  await poll(engine, 2)
  expect(taskStops(engine).length).toBe(0)
  expect(stoppedOf(engine).length).toBe(1)
  expect(stoppedOf(engine)[0].payload.answer).toBe(F12_ANSWER)
})

test('an answered worker still running is stopped once, and ends completed with the answer and the note', async ($: any, on: any) => {
  const engine = await spawned($, on)
  const held = gate()
  engine.script.duringTool = async (_$: any, e: any) => {
    if (e.tool === 'TaskStop') await held.open
  }
  await $.turn.complete(turn('agent-1', {}, F12_ANSWER))
  await poll(engine, 3)
  expect(taskStops(engine).length).toBe(1)
  expect(taskStops(engine)[0].task_id).toBe('agent-1')
  // A turn-triggered check and a poll overlap the pending TaskStop.
  await $.turn.complete({ ...turn('agent-1', {}, ''), reason: 'aborted', isAborted: true })
  await poll(engine, 2)
  expect(taskStops(engine).length).toBe(1)
  expect(stoppedOf(engine).length).toBe(0)
  held.release()
  await settle(engine)
  engine.agents[0].status = 'killed'
  await poll(engine)
  expect(taskStops(engine).length).toBe(1)
  const stopped = stoppedOf(engine)
  expect(stopped.length).toBe(1)
  expect(stopped[0].payload.status).toBe('completed')
  expect(stopped[0].payload.answer).toBe(`${F12_ANSWER}\n\n${PLUGIN_NOTE}`)
})

test('a refused TaskStop is noted, not retried, and the run then ends completed without the note', async ($: any, on: any) => {
  const engine = await spawned($, on)
  engine.script.toolResult = (e: any) => (e.tool === 'TaskStop' ? { deny: 'not allowed' } : { result: 'ok', text: 'ok' })
  await $.turn.complete(turn('agent-1', {}, F12_ANSWER))
  await poll(engine, 8)
  expect(taskStops(engine).length).toBe(1)
  expect(stoppedOf(engine).length).toBe(0)
  const timeline = await timelineOf($)
  expect(timeline).toContain(
    'could not stop agent agent-1 after its answer (not allowed); the run ends when its background work ends or at the wall time.',
  )
  engine.agents[0].status = 'completed'
  await poll(engine)
  const stopped = stoppedOf(engine)
  expect(stopped.length).toBe(1)
  expect(stopped[0].payload.status).toBe('completed')
  expect(stopped[0].payload.answer).toBe(F12_ANSWER)
  expect(await timelineOf($)).not.toContain('still runs 30 s')
})

test('a TaskStop with no answer in 30 s is noted once, and when it takes late the run still ends completed with the answer', async ($: any, on: any) => {
  const engine = await spawned($, on)
  const held = gate()
  engine.script.duringTool = async (_$: any, e: any) => {
    if (e.tool === 'TaskStop') await held.open
  }
  await $.turn.complete(turn('agent-1', {}, F12_ANSWER))
  await poll(engine, 19)
  expect(taskStops(engine).length).toBe(1)
  expect(stoppedOf(engine).length).toBe(0)
  const late =
    'the stop of agent agent-1 after its answer had no answer in 30 s; it may still take, else the run ends when its background work ends or at the wall time.'
  expect(await timelineOf($)).toContain(late)
  // The call was not cancelled: the engine carries the stop out late.
  held.release()
  engine.agents[0].status = 'killed'
  await poll(engine)
  const stopped = stoppedOf(engine)
  expect(stopped.length).toBe(1)
  expect(stopped[0].payload.status).toBe('completed')
  expect(stopped[0].payload.answer).toBe(`${F12_ANSWER}\n\n${PLUGIN_NOTE}`)
  expect(taskStops(engine).length).toBe(1)
  const timeline = await timelineOf($)
  expect(timeline.split(late).length - 1).toBe(1)
  expect(timeline).not.toContain('still runs 30 s')
})

test('a worker still running across 16 polls after its stop is noted once and not stopped', async ($: any, on: any) => {
  const engine = await spawned($, on)
  await $.turn.complete(turn('agent-1', {}, F12_ANSWER))
  await poll(engine, 3)
  expect(taskStops(engine).length).toBe(1)
  await poll(engine, 16)
  await settle(engine)
  expect(taskStops(engine).length).toBe(1)
  expect(stoppedOf(engine).length).toBe(0)
  const note = 'agent agent-1 still runs 30 s after its stop; it ends with its shell or at the wall time. To end it now, stop it in Claude Code.'
  const timeline = await timelineOf($)
  expect((timeline.match(/still runs 30 s after its stop/g) ?? []).length).toBe(1)
  expect(timeline).toContain(note)
})

test('a list begun before TaskStop answered confirms nothing, the next poll does', async ($: any, on: any) => {
  const engine = await spawned($, on)
  const heldStop = gate()
  const heldList = gate()
  engine.script.duringTool = async (_$: any, e: any) => {
    if (e.tool === 'TaskStop') await heldStop.open
  }
  await $.turn.complete(turn('agent-1', {}, F12_ANSWER))
  await poll(engine, 3)
  expect(taskStops(engine).length).toBe(1)
  engine.script.listHold = heldList.open
  await poll(engine)
  heldStop.release()
  await settle(engine)
  engine.script.listHold = null
  engine.agents[0].status = 'killed'
  heldList.release()
  await settle(engine)
  expect(stoppedOf(engine).length).toBe(0)
  await poll(engine)
  expect(stoppedOf(engine).length).toBe(1)
  expect(stoppedOf(engine)[0].payload.status).toBe('completed')
})
