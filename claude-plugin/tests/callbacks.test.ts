import { expect, test } from 'claude-code/testing'
import { nativeCalls, scriptedEngine, settle, spawnLine, startedRun, turn } from './support.ts'

// Each step moves a mocked clock and lets the plugin settle: not a 5 s test.
const SLOW = { timeoutMs: 60000 }

const failed = { exitCode: 1, stdout: '', stderr: 'no coordinator', isStdoutTruncated: false, isStderrTruncated: false }
const acknowledged = { exitCode: 0, stdout: '', stderr: '', isStdoutTruncated: false, isStderrTruncated: false }

async function endedDispatch($: any, on: any) {
  const engine = await startedRun($, on)
  engine.stream.push('stdout', spawnLine('d1'))
  await settle(engine)
  await $.turn.complete(turn('agent-1', { input_tokens: 42 }))
  engine.agents[0].status = 'completed'
  return engine
}

const advance = async (engine: any, ms: number) => {
  await engine.clock.advance(ms)
  await settle(engine)
}

test('a failed stopped call is retried with backoff until it succeeds, the payload on stdin', SLOW, async ($: any, on: any) => {
  const engine = await endedDispatch($, on)
  let attempts = 0
  engine.script.runResult = (argv: string[]) => (argv[2] === 'stopped' && ++attempts <= 3 ? failed : acknowledged)
  await advance(engine, 2000) // the poll sees the end: attempt 1 fails
  expect(nativeCalls(engine.calls, 'stopped').length).toBe(1)
  await advance(engine, 999)
  expect(nativeCalls(engine.calls, 'stopped').length).toBe(1)
  await advance(engine, 1) // +1 s: attempt 2
  expect(nativeCalls(engine.calls, 'stopped').length).toBe(2)
  await advance(engine, 2000) // +2 s: attempt 3
  expect(nativeCalls(engine.calls, 'stopped').length).toBe(3)
  await advance(engine, 4000) // +4 s: attempt 4 succeeds
  const calls = nativeCalls(engine.calls, 'stopped')
  expect(calls.length).toBe(4)
  await advance(engine, 60000)
  expect(nativeCalls(engine.calls, 'stopped').length).toBe(4)
  for (const call of calls) {
    expect(call.payload.usage.input_tokens).toBe(42)
    expect(call.payload.agent).toBe('agent-1')
    expect(typeof call.init.timeoutMs).toBe('number')
    expect(call.argv).toEqual(['relais', 'native', 'stopped', '--dispatch', 'd1'])
  }
})

test('the backoff is 1, 2, 4, 8 s, then every 15 s', SLOW, async ($: any, on: any) => {
  const engine = await endedDispatch($, on)
  engine.script.runResult = (argv: string[]) => (argv[2] === 'stopped' ? failed : acknowledged)
  await advance(engine, 2000)
  const gaps = [1000, 2000, 4000, 8000, 15000, 15000]
  let seen = 1
  for (const gap of gaps) {
    await advance(engine, gap - 1)
    expect(nativeCalls(engine.calls, 'stopped').length).toBe(seen)
    await advance(engine, 1)
    seen += 1
    expect(nativeCalls(engine.calls, 'stopped').length).toBe(seen)
  }
})

test('retries stop when the run’s child exits', SLOW, async ($: any, on: any) => {
  const engine = await endedDispatch($, on)
  engine.script.runResult = (argv: string[]) => (argv[2] === 'stopped' ? failed : acknowledged)
  await advance(engine, 2000)
  expect(nativeCalls(engine.calls, 'stopped').length).toBe(1)
  engine.stream.end(1)
  await settle(engine)
  await advance(engine, 120000)
  expect(nativeCalls(engine.calls, 'stopped').length).toBe(1)
})

test('a failed bound call is retried too', SLOW, async ($: any, on: any) => {
  const engine = await startedRun($, on)
  let attempts = 0
  engine.script.runResult = (argv: string[]) => (argv[2] === 'bound' && ++attempts <= 1 ? failed : acknowledged)
  engine.stream.push('stdout', spawnLine('d1'))
  await settle(engine)
  expect(nativeCalls(engine.calls, 'bound').length).toBe(1)
  await advance(engine, 1000)
  expect(nativeCalls(engine.calls, 'bound').length).toBe(2)
  await advance(engine, 60000)
  expect(nativeCalls(engine.calls, 'bound').length).toBe(2)
})

test('hello goes out at session.start and every 30 s', SLOW, async ($: any, on: any) => {
  const engine = scriptedEngine(on)
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await settle(engine)
  expect(nativeCalls(engine.calls, 'hello').length).toBe(1)
  expect(nativeCalls(engine.calls, 'hello')[0].argv).toEqual(['relais', 'native', 'hello', '--session', 'session-1'])
  await advance(engine, 30000)
  expect(nativeCalls(engine.calls, 'hello').length).toBe(2)
  await advance(engine, 30000)
  expect(nativeCalls(engine.calls, 'hello').length).toBe(3)
})

test('after the module unloads there is no hello', SLOW, async ($: any, on: any) => {
  const engine = scriptedEngine(on)
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await settle(engine)
  await advance(engine, 30000)
  const before = nativeCalls(engine.calls, 'hello').length
  await $.session.end({ reason: 'prompt_input_exit', sessionId: 'session-1', resume: { id: 'session-1' } })
  await advance(engine, 120000)
  expect(nativeCalls(engine.calls, 'hello').length).toBe(before)
})
