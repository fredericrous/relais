// f24: a cancel of a run that has ended stops the agents its dispatches
// bound that still run. A worker's `pkill -f <worktree>` once killed its own
// driver; the run ended `interrupted` and the worker kept editing in place.

import { expect, test } from 'claude-code/testing'
import { continueLine, event, nativeCalls, settle, spawnLine, startedRun, startQueued } from './support.ts'
import { STOP_CONFIRM_MS } from '../hooks/dispatches.ts'

// The late case moves the scripted clock by STOP_CONFIRM_MS: not a 5 s test.
const SLOW = { timeoutMs: 60000 }

const fakeCancelOutput = (stdout: string, exitCode = 0) => ({ exitCode, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false })
const ENDED_LINE = 'run run-c already ended: interrupted'
const named = (run: string) => event(run, 0, { kind: 'phase', state: 'running', reason: 'x', detail: {} })

// A run named `run-c` with one dispatch per id, each with its own agent.
async function runWith($: any, on: any, dispatches: string[]) {
  const engine = await startedRun($, on)
  engine.stream.push('stdout', named('run-c'))
  for (const d of dispatches) engine.stream.push('stdout', spawnLine(d))
  await settle(engine)
  return engine
}

const answering = (engine: any, line: string, exitCode = 0) => {
  engine.script.runResult = (argv: string[]) => (argv[2] === 'cancel' ? fakeCancelOutput(line + '\n', exitCode) : fakeCancelOutput(''))
}
const stops = (engine: any) => engine.calls.tool.filter((t: any) => t.tool === 'TaskStop').map((t: any) => t.task_id)
const cancel = async ($: any) => (await $.tool.call({ tool: 'mcp__relais__cancel', run: 'run-c' })).result as string

test('cancel of an ended run stops its agent still running, after the CLI line', async ($: any, on: any) => {
  const engine = await runWith($, on, ['d1'])
  answering(engine, ENDED_LINE)
  const reply = await cancel($)
  expect(stops(engine)).toEqual(['agent-1'])
  expect(reply.split('\n')).toEqual([ENDED_LINE, 'stop sent to agent agent-1', 'exit status: 0'])
})

test('an agent that has completed is not stopped', async ($: any, on: any) => {
  const engine = await runWith($, on, ['d1'])
  engine.agents[0].status = 'completed'
  answering(engine, ENDED_LINE)
  const reply = await cancel($)
  expect(stops(engine)).toEqual([])
  expect(reply).toContain('no agent of run run-c is still running')
})

test('a second cancel still stops the agent the first one marked stopped', async ($: any, on: any) => {
  const engine = await runWith($, on, ['d1'])
  answering(engine, ENDED_LINE)
  engine.script.toolResult = (e: any) => (e.tool === 'TaskStop' ? { deny: 'not now' } : { result: 'ok', text: 'ok' })
  expect(await cancel($)).toContain('could not stop agent agent-1: not now')
  engine.script.toolResult = () => ({ result: 'ok', text: 'ok' })
  expect(await cancel($)).toContain('stop sent to agent agent-1')
  expect(stops(engine)).toEqual(['agent-1', 'agent-1'])
})

test('an agent rebound to another run’s dispatch is that run’s, and kept', async ($: any, on: any) => {
  const engine = await runWith($, on, ['d1'])
  await $.tool.call({ tool: 'mcp__relais__run', task: 'other.json', cwd: '/repo' })
  await startQueued(engine)
  const other = engine.feed('other.json')
  other.push('stdout', named('run-o'))
  other.push('stdout', continueLine('d9', 'agent-1'))
  await settle(engine)
  answering(engine, ENDED_LINE)
  const reply = await cancel($)
  expect(stops(engine)).toEqual([])
  expect(reply).toContain('no agent of run run-c is known to this plugin')
})

test('an agent rebound while the agent list is awaited is kept', async ($: any, on: any) => {
  const engine = await runWith($, on, ['d1'])
  await $.tool.call({ tool: 'mcp__relais__run', task: 'other.json', cwd: '/repo' })
  await startQueued(engine)
  const other = engine.feed('other.json')
  other.push('stdout', named('run-o'))
  await settle(engine)
  answering(engine, ENDED_LINE)
  let release = () => undefined as void
  engine.script.listHold = new Promise<void>(resolve => (release = resolve))
  let reply: string | undefined
  const pending = cancel($).then(text => (reply = text))
  await settle(engine)
  other.push('stdout', continueLine('d9', 'agent-1'))
  await settle(engine)
  engine.script.listHold = null
  release()
  await pending
  expect(stops(engine)).toEqual([])
  expect(reply).toContain('agent agent-1 now belongs to another run; not stopped')
})

test('an agent list that never answers ends at the bound and stops nothing', SLOW, async ($: any, on: any) => {
  const engine = await runWith($, on, ['d1'])
  answering(engine, ENDED_LINE)
  let release = () => undefined as void
  engine.script.listHold = new Promise<void>(resolve => (release = resolve))
  let reply: string | undefined
  const pending = cancel($).then(text => (reply = text))
  await settle(engine)
  await engine.clock.advance(STOP_CONFIRM_MS)
  await settle(engine)
  await pending
  expect(reply).toContain(`the agent list could not be read (no answer in ${STOP_CONFIRM_MS / 1000} s)`)
  engine.script.listHold = null
  release()
  await settle(engine)
  expect(stops(engine)).toEqual([])
})

test('a live run’s cancel request stops no agent from the plugin', async ($: any, on: any) => {
  const engine = await runWith($, on, ['d1'])
  answering(engine, 'cancel requested for run run-c')
  const reply = await cancel($)
  expect(stops(engine)).toEqual([])
  expect(reply).toBe('cancel requested for run run-c\nexit status: 0')
})

test('a driverless run that cancel reaps has its agent stopped', async ($: any, on: any) => {
  const engine = await runWith($, on, ['d1'])
  answering(engine, 'run run-c had no live driver; marked cancelled')
  const reply = await cancel($)
  expect(stops(engine)).toEqual(['agent-1'])
  expect(reply).toContain('stop sent to agent agent-1')
})

test('a run this plugin never dispatched for says so and stops nothing', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  answering(engine, ENDED_LINE)
  const reply = await cancel($)
  expect(stops(engine)).toEqual([])
  expect(reply.split('\n')).toEqual([
    ENDED_LINE,
    'no agent of run run-c is known to this plugin; if one still runs, ask the person to stop it in Claude Code',
    'exit status: 0',
  ])
})

test('an agent list that cannot be read is said, and nothing is stopped', async ($: any, on: any) => {
  const engine = await runWith($, on, ['d1'])
  answering(engine, ENDED_LINE)
  const failing = Promise.reject(new Error('list down'))
  failing.catch(() => undefined)
  engine.script.listHold = failing
  const reply = await cancel($)
  expect(stops(engine)).toEqual([])
  expect(reply).toContain('the agent list could not be read (')
  expect(reply).toContain('); no agent of run run-c was stopped')
})

test('a slow list leaves the stop the rest of the window, and the late line names the window', SLOW, async ($: any, on: any) => {
  const engine = await runWith($, on, ['d1'])
  answering(engine, ENDED_LINE)
  engine.script.toolResult = (e: any) => (e.tool === 'TaskStop' ? new Promise(() => undefined) : { result: 'ok', text: 'ok' })
  let release = () => undefined as void
  engine.script.listHold = new Promise<void>(resolve => (release = resolve))
  let reply: string | undefined
  const pending = cancel($).then(text => (reply = text))
  await settle(engine)
  await engine.clock.advance(STOP_CONFIRM_MS - 10000)
  engine.script.listHold = null
  release()
  await settle(engine)
  await engine.clock.advance(10000)
  await settle(engine)
  await pending
  expect(stops(engine)).toEqual(['agent-1'])
  expect(reply).toContain(`agent agent-1: TaskStop had no answer within the ${STOP_CONFIRM_MS / 1000} s window; it may still take`)
})

test('two agents, one stop unanswered: one bound in all, both lines said', SLOW, async ($: any, on: any) => {
  const engine = await runWith($, on, ['d1', 'd2'])
  answering(engine, ENDED_LINE)
  const never = new Promise(() => undefined)
  engine.script.toolResult = (e: any) => (e.tool === 'TaskStop' && e.task_id === 'agent-2' ? never : { result: 'ok', text: 'ok' })
  let reply: string | undefined
  const pending = cancel($).then(text => (reply = text))
  await settle(engine)
  await engine.clock.advance(STOP_CONFIRM_MS - 1)
  await settle(engine)
  expect(reply).toBe(undefined)
  await engine.clock.advance(1)
  await settle(engine)
  await pending
  expect(nativeCalls(engine.calls, 'cancel').length).toBe(1)
  expect(reply!.split('\n')).toEqual([
    ENDED_LINE,
    'stop sent to agent agent-1',
    `agent agent-2: TaskStop had no answer in ${STOP_CONFIRM_MS / 1000} s; it may still take`,
    'exit status: 0',
  ])
})
