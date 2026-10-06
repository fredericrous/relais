import { expect, test } from 'claude-code/testing'
import { continueLine, nativeCalls, settle, spawnLine, startedRun, turn } from './support.ts'

const POLL_MS = 2000

async function spawned($: any, on: any) {
  const engine = await startedRun($, on)
  engine.stream.push('stdout', spawnLine('d1'))
  await settle(engine)
  return engine
}

const poll = async (engine: any) => {
  await engine.clock.advance(POLL_MS)
  await settle(engine)
}

test('turn.complete of a relais agent adds its usage; another agent’s turn adds nothing', async ($: any, on: any) => {
  const engine = await spawned($, on)
  await $.turn.complete(turn('agent-1', { input_tokens: 100, output_tokens: 10 }))
  await $.turn.complete(turn('somebody-else', { input_tokens: 5000, output_tokens: 5000 }))
  engine.agents[0].status = 'completed'
  await poll(engine)
  const [stopped] = nativeCalls(engine.calls, 'stopped')
  expect(stopped.payload.usage.input_tokens).toBe(100)
  expect(stopped.payload.usage.output_tokens).toBe(10)
})

test('a status that leaves running ends the dispatch with one stopped, usage summed', async ($: any, on: any) => {
  const engine = await spawned($, on)
  await $.turn.complete(turn('agent-1', { input_tokens: 100, output_tokens: 10, cache_read_input_tokens: 7 }, 'first'))
  await $.turn.complete(turn('agent-1', { input_tokens: 20, output_tokens: 5, cache_creation_input_tokens: 3 }, 'the answer'))
  await settle(engine)
  expect(nativeCalls(engine.calls, 'stopped').length).toBe(0)
  engine.agents[0].status = 'completed'
  await poll(engine)
  await poll(engine)
  const stopped = nativeCalls(engine.calls, 'stopped')
  expect(stopped.length).toBe(1)
  expect(stopped[0].argv).toEqual(['relais', 'native', 'stopped', '--dispatch', 'd1'])
  expect(stopped[0].payload).toEqual({
    agent: 'agent-1',
    status: 'completed',
    usage: {
      input_tokens: 120,
      output_tokens: 15,
      cache_read_input_tokens: 7,
      cache_creation_input_tokens: 3,
      model: 'sonnet',
    },
    answer: 'the answer',
  })
})

test('three turns before the status leaves running are reported once, summed', async ($: any, on: any) => {
  const engine = await spawned($, on)
  for (const n of [1, 2, 3]) await $.turn.complete(turn('agent-1', { input_tokens: n * 10 }))
  engine.agents[0].status = 'completed'
  await poll(engine)
  await poll(engine)
  const stopped = nativeCalls(engine.calls, 'stopped')
  expect(stopped.length).toBe(1)
  expect(stopped[0].payload.usage.input_tokens).toBe(60)
})

test('a continue then a stale completed is no end until the repair’s first turn', async ($: any, on: any) => {
  const engine = await spawned($, on)
  await $.turn.complete(turn('agent-1', { input_tokens: 100 }))
  engine.agents[0].status = 'completed'
  await poll(engine)
  expect(nativeCalls(engine.calls, 'stopped').length).toBe(1)

  engine.stream.push('stdout', continueLine('d2', 'agent-1'))
  await settle(engine)
  // The list still shows the previous run's `completed`.
  await poll(engine)
  await poll(engine)
  expect(nativeCalls(engine.calls, 'stopped').length).toBe(1)

  engine.agents[0].status = 'running'
  await poll(engine)
  await $.turn.complete(turn('agent-1', { input_tokens: 7 }, 'repaired'))
  engine.agents[0].status = 'completed'
  await poll(engine)
  const stopped = nativeCalls(engine.calls, 'stopped')
  expect(stopped.length).toBe(2)
  expect(stopped[1].argv).toEqual(['relais', 'native', 'stopped', '--dispatch', 'd2'])
  expect(stopped[1].payload.usage.input_tokens).toBe(7)
  expect(stopped[1].payload.answer).toBe('repaired')
  const sent = engine.calls.tool.filter((t: any) => t.tool === 'SendMessage')
  expect(sent.length).toBe(1)
  expect(sent[0].to).toBe('agent-1')
})

test('a continue after a failed run is no end until the status moves', async ($: any, on: any) => {
  const engine = await spawned($, on)
  engine.agents[0].status = 'failed'
  await poll(engine)
  // d1 ended with no turn: failed, and the status moved from the spawn's.
  expect(nativeCalls(engine.calls, 'stopped').length).toBe(1)

  engine.stream.push('stdout', continueLine('d2', 'agent-1'))
  await settle(engine)
  await poll(engine)
  await poll(engine)
  expect(nativeCalls(engine.calls, 'stopped').length).toBe(1)

  engine.agents[0].status = 'running'
  await poll(engine)
  expect(nativeCalls(engine.calls, 'stopped').length).toBe(1)
  engine.agents[0].status = 'failed'
  await poll(engine)
  const stopped = nativeCalls(engine.calls, 'stopped')
  expect(stopped.length).toBe(2)
  expect(stopped[1].argv).toEqual(['relais', 'native', 'stopped', '--dispatch', 'd2'])
  expect(stopped[1].payload.status).toBe('failed')
  expect(stopped[1].payload.usage.input_tokens).toBe(0)
})

test('a rejected SendMessage is stopped as failed', async ($: any, on: any) => {
  const engine = await spawned($, on)
  engine.script.toolResult = (e: any) => (e.tool === 'SendMessage' ? { deny: 'agent is gone' } : { result: 'ok', text: 'ok' })
  engine.stream.push('stdout', continueLine('d2', 'agent-1'))
  await settle(engine)
  const stopped = nativeCalls(engine.calls, 'stopped')
  expect(stopped.length).toBe(1)
  expect(stopped[0].argv).toEqual(['relais', 'native', 'stopped', '--dispatch', 'd2'])
  expect(stopped[0].payload.status).toBe('failed')
  expect(stopped[0].payload.answer).toContain('agent is gone')
  // Nothing further for it: it is over.
  await poll(engine)
  expect(nativeCalls(engine.calls, 'stopped').length).toBe(1)
})

test('bound and stopped are each sent once per dispatch', async ($: any, on: any) => {
  const engine = await spawned($, on)
  await $.turn.complete(turn('agent-1', { input_tokens: 1 }))
  engine.agents[0].status = 'completed'
  for (let i = 0; i < 4; i++) await poll(engine)
  expect(nativeCalls(engine.calls, 'bound').length).toBe(1)
  expect(nativeCalls(engine.calls, 'stopped').length).toBe(1)
})
