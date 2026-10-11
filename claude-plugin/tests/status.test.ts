import { expect, test } from 'claude-code/testing'
import { formatCost, rebuildRuns, applyEvent, emptyRun } from '../hooks/timeline.ts'
import { wrapWords } from '../hooks/pane.ts'
import { nativeCalls, scriptedEngine, settle, spawnLine, startedRun, turn } from './support.ts'

const result = (stdout: string, exitCode = 0) => ({ exitCode, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false })

const ev = (run: string, seq: number, event: Record<string, unknown>) => ({
  relais: 'event',
  run,
  seq,
  at: `2026-10-06T11:00:0${seq}Z`,
  event,
})

const timelineOf = (run: string, current: Record<string, unknown> | undefined, events: unknown[] = []) => ({
  run,
  events: events.length > 0 ? events : [ev(run, 0, { kind: 'phase', state: 'running', reason: 'worker_dispatched', detail: {} })],
  ...(current ? { current } : {}),
})

const NOW = Date.parse('2026-10-06T12:00:00Z')

test('rebuildRuns with {runs:[...]} builds one model per run', () => {
  const models = rebuildRuns(
    { runs: [timelineOf('run-a', { state: 'running', session: 's1' }), timelineOf('run-b', { state: 'running', session: 's1' })] },
    NOW,
  )
  expect(Object.keys(models).sort()).toEqual(['run-a', 'run-b'])
  expect(models['run-a'].session).toBe('s1')
  expect(rebuildRuns({ runs: [] }, NOW)).toEqual({})
})

test('a terminal current.state with no outcome event is folded as ended, with its reason', () => {
  const models = rebuildRuns(
    { runs: [timelineOf('run-a', { state: 'interrupted', reason: 'run run-a interrupted: its driver (pid 7) is gone' })] },
    NOW,
  )
  expect(models['run-a'].outcome?.state).toBe('interrupted')
  expect(models['run-a'].outcome?.reason).toContain('driver (pid 7) is gone')
  expect(models['run-a'].endedAt).toBe(NOW)
})

test('a stalled current is a plain-text hint on a live run', () => {
  const models = rebuildRuns({ runs: [timelineOf('run-a', { state: 'running', stalled: true, silent_seconds: 725 })] }, NOW)
  expect(models['run-a'].stalled).toBe('no events for 12m')
})

test('statusOf never returns another session’s run unless asked by id, and then marks it foreign', async ($: any, on: any) => {
  const engine = scriptedEngine(on)
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await settle(engine)
  engine.script.runResult = (argv: string[]) => {
    if (argv[2] !== 'status') return result('')
    if (argv[3] === '--session') {
      expect(argv[4]).toBe('session-1')
      return result(JSON.stringify({ runs: [timelineOf('run-mine', { state: 'accepted', session: 'session-1' })] }))
    }
    return result(JSON.stringify(timelineOf('run-other', { state: 'accepted', session: 'session-2' })))
  }
  const list = JSON.parse((await $.tool.call({ tool: 'mcp__relais__status' })).result)
  expect(list.map((r: any) => r.run)).toEqual(['run-mine'])
  const asked = JSON.parse((await $.tool.call({ tool: 'mcp__relais__status', run: 'run-other' })).result)
  expect(asked[0].run).toBe('run-other')
  expect(asked[0].foreign).toBe(true)
  expect(asked[0].session).toBe('session-2')
  const again = JSON.parse((await $.tool.call({ tool: 'mcp__relais__status' })).result)
  expect(again.map((r: any) => r.run)).toEqual(['run-mine'])
  expect(list[0].foreign).toBeUndefined()
})

test('statusOf refreshes a live model from the ledger before it replies', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  engine.stream.push('stdout', JSON.stringify(ev('run-live', 0, { kind: 'phase', state: 'running', reason: 'x', detail: {} })) .replace('"relais":"event"', '"relais":"event"') + '\n')
  await settle(engine)
  engine.script.runResult = (argv: string[]) =>
    argv[2] === 'status' ? result(JSON.stringify(timelineOf('run-live', { state: 'interrupted', reason: 'its driver is gone' }))) : result('')
  const [run] = JSON.parse((await $.tool.call({ tool: 'mcp__relais__status' })).result)
  expect(run.outcome.state).toBe('interrupted')
  expect(run.outcome.reason).toBe('its driver is gone')
})

test('the cancel tool runs native cancel with --run and --session and returns its line', async ($: any, on: any) => {
  const engine = scriptedEngine(on)
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await settle(engine)
  engine.script.runResult = (argv: string[]) => (argv[2] === 'cancel' ? result('run run-x cancelled\n') : result(''))
  const reply = await $.tool.call({ tool: 'mcp__relais__cancel', run: 'run-x' })
  const [call] = nativeCalls(engine.calls, 'cancel')
  expect(call.argv).toEqual(['relais', 'native', 'cancel', '--run', 'run-x', '--session', 'session-1'])
  expect(reply.result).toContain('run run-x cancelled')
  expect(reply.result).toContain('exit status: 0')
})

test('a cancelled run’s dispatches are stopped in the store: no late stopped is sent', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  engine.stream.push('stdout', JSON.stringify({ relais: 'event', run: 'run-c', seq: 0, at: '2026-10-06T12:00:00Z', event: { kind: 'phase', state: 'running', reason: 'x', detail: {} } }) + '\n')
  engine.stream.push('stdout', spawnLine('d1'))
  await settle(engine)
  engine.script.runResult = (argv: string[]) => (argv[2] === 'cancel' ? result('run run-c cancelled\n') : result(''))
  await $.tool.call({ tool: 'mcp__relais__cancel', run: 'run-c' })
  // A turn makes the agent's end an end: without one, nothing is sent
  // either way and the test could not tell the two cases apart.
  await $.turn.complete(turn('agent-1', { input_tokens: 1, output_tokens: 1 }))
  engine.agents[0].status = 'completed'
  await engine.clock.advance(4000)
  await settle(engine)
  expect(nativeCalls(engine.calls, 'stopped').length).toBe(0)
})

test('a cancel still pending leaves the dispatches open: the driver still gets its stopped', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  engine.stream.push('stdout', JSON.stringify({ relais: 'event', run: 'run-c', seq: 0, at: '2026-10-06T12:00:00Z', event: { kind: 'phase', state: 'running', reason: 'x', detail: {} } }) + '\n')
  engine.stream.push('stdout', spawnLine('d1'))
  await settle(engine)
  engine.script.runResult = (argv: string[]) =>
    argv[2] === 'cancel' ? result('cancel requested; the driver has not ended yet\n') : result('')
  await $.tool.call({ tool: 'mcp__relais__cancel', run: 'run-c' })
  // A turn makes the agent's end an end: without one, nothing is sent
  // either way and the test could not tell the two cases apart.
  await $.turn.complete(turn('agent-1', { input_tokens: 1, output_tokens: 1 }))
  engine.agents[0].status = 'completed'
  await engine.clock.advance(4000)
  await settle(engine)
  expect(nativeCalls(engine.calls, 'stopped').length).toBe(1)
})

test('onTurn sends the dispatch’s cumulative usage to native usage', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  engine.stream.push('stdout', spawnLine('d1'))
  await settle(engine)
  await $.turn.complete(turn('agent-1', { input_tokens: 100, output_tokens: 10 }))
  await $.turn.complete(turn('agent-1', { input_tokens: 20, output_tokens: 5 }))
  await settle(engine)
  const sent = nativeCalls(engine.calls, 'usage')
  expect(sent.length).toBe(2)
  expect(sent[1].argv).toEqual(['relais', 'native', 'usage', '--dispatch', 'd1'])
  expect(sent[1].payload.agent).toBe('agent-1')
  expect(sent[1].payload.turns).toBe(2)
  expect(sent[1].payload.usage.input_tokens).toBe(120)
  expect(sent[1].payload.usage.output_tokens).toBe(15)
  expect(Array.isArray(sent[1].payload.message_ids)).toBe(true)
})

test('formatCost shows unpriced (<model>) when the cost event carries an unpriced reason', () => {
  const m = applyEvent(emptyRun('r', 0), { kind: 'cost', booked: null, completeness: 'unknown', unpriced_reason: 'gpt-x has no [pricing.models] entry' }, 1)
  expect(formatCost(m.cost)).toBe('unpriced (gpt-x)')
  const hostile = applyEvent(emptyRun('r', 0), { kind: 'cost', booked: null, completeness: 'unknown', unpriced_reason: `\u001b[31m${'m'.repeat(60)} has no entry` }, 1)
  expect(formatCost(hostile.cost)).toBe(`unpriced (${'m'.repeat(40)})`)
  expect(formatCost({ booked: null, completeness: 'unknown' })).toBe('unknown')
})

test('a long reason wraps at a word boundary within 80 columns', () => {
  const reason = 'run run-x interrupted: its driver (pid 4242) is gone; the worktree is kept, `relais resume --retire run-x` removes it'
  const rows = wrapWords(reason, 80, '        ')
  expect(rows.length).toBeGreaterThan(1)
  for (const row of rows) expect(row.length).toBeLessThanOrEqual(80)
  expect(rows.map(r => r.trim()).join(' ')).toBe(reason)
})
