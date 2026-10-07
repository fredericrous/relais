import { expect, test } from 'claude-code/testing'
import { event, line, settle, startedRun, startQueued } from './support.ts'

const RUN = 'run-65d322006dd13-c35c'

const phase = (seq: number, state: string) => event(RUN, seq, { kind: 'phase', state, reason: 'ok', detail: {} })
// The shape relais writes: the summary is an object, `trial` only for a replay.
const done = (outcome = 'accepted', extra: Record<string, unknown> = {}) =>
  line({
    relais: 'done',
    run: RUN,
    outcome,
    receipt: '/runs/r1/receipt.json',
    summary: { files_changed: 1, insertions: 3, deletions: 1 },
    ...extra,
  })

const lastStatus = (engine: any) => engine.calls.statuses[engine.calls.statuses.length - 1]

const tick = async (engine: any, ms = 200) => {
  await engine.clock.advance(ms)
  await settle(engine)
}

test('while a run is live the status line carries the count, phase, attempt, elapsed time and the command', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  engine.stream.push(
    'stdout',
    phase(0, 'running') +
      event(RUN, 1, { kind: 'dispatch_started', dispatch: 'd1', agent_kind: 'worker', attempt: 2, model: 'sonnet', effort: null }),
  )
  await settle(engine)
  await tick(engine, 5000)
  const status = lastStatus(engine)
  expect(status).toContain('1 run')
  expect(status).toContain('running')
  expect(status).toContain('attempt 2')
  expect(/0:0[45]/.test(status)).toBe(true)
  expect(status).toContain('/relais-status')
})

test('done toasts the outcome, tells the model, and the status line keeps the outcome', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  engine.stream.push('stdout', phase(0, 'running') + phase(1, 'accepted') + done())
  await settle(engine)
  await tick(engine)
  expect(engine.calls.toasts.some((t: string) => t.includes('accepted') && t.includes('receipt.json'))).toBe(true)
  expect(engine.calls.prompts.length).toBe(1)
  const text = engine.calls.prompts[0].text
  expect(text).toContain('accepted')
  expect(text).toContain('/runs/r1/receipt.json')
  expect(text).toContain(`relais run ${RUN} finished: accepted.`)
  expect(text).toContain('Changed: 1 file, +3 -1')
  expect(text).not.toContain('Replay trial')
  expect(lastStatus(engine)).toContain('accepted')
  // It stays: nothing wipes it while time passes. Ten seconds outlasts
  // the 3 s toast merge and a 2 s poll; a full minute of 100 ms flushes
  // costs seconds of real time and proves nothing more.
  await tick(engine, 10000)
  expect(lastStatus(engine)).toContain('accepted')
})

test('a replay\'s done names the replay and its trial', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  engine.stream.push('stdout', phase(0, 'running') + done('accepted', { trial: 'trial-65d3-r' }))
  await settle(engine)
  await tick(engine)
  const text = engine.calls.prompts[0].text
  expect(text).toContain(`relais replay ${RUN} finished: accepted.`)
  expect(text).toContain('Replay trial: trial-65d3-r')
})

test('a verdict whose submit fails stays queued and is sent on the next tick', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  engine.script.submitFailures = 2
  engine.stream.push('stdout', phase(0, 'running') + done())
  await settle(engine)
  await tick(engine)
  await tick(engine)
  await tick(engine)
  expect(engine.calls.prompts.length).toBe(1)
  expect(engine.calls.prompts[0].text).toContain('finished: accepted')
  // The failed attempt was said, once, in the run's timeline.
  const reply = await $.tool.call({ tool: 'mcp__relais__status', run: RUN })
  // Two failed ticks, one line: said once per streak.
  expect(reply.result.split('the outcome message could not be sent yet').length - 1).toBe(1)
})

test('a status relais cannot give says why instead of "no run"', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  engine.script.runResult = (argv: string[]) =>
    argv[2] === 'status'
      ? { exitCode: 3, stdout: '', stderr: 'no such run', isStdoutTruncated: false, isStderrTruncated: false }
      : { exitCode: 0, stdout: '', stderr: '', isStdoutTruncated: false, isStderrTruncated: false }
  const reply = await $.tool.call({ tool: 'mcp__relais__status', run: 'run-missing' })
  expect(reply.result).toContain('could not be read')
  expect(reply.result).toContain('no such run')
})

test('the kept outcome goes when the pane is opened', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  engine.stream.push('stdout', phase(0, 'running') + done())
  await settle(engine)
  await tick(engine)
  expect(lastStatus(engine)).toContain('accepted')
  await $.command.run({ command: 'relais-status' })
  await tick(engine)
  expect(lastStatus(engine)).toBe(undefined)
})

test('the kept outcome goes when the next run starts', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  engine.stream.push('stdout', phase(0, 'running') + done())
  await settle(engine)
  await tick(engine)
  expect(lastStatus(engine)).toContain('accepted')
  await $.tool.call({ tool: 'mcp__relais__run', task: 'next', cwd: '/repo' })
  await startQueued(engine)
  await tick(engine)
  expect(lastStatus(engine)).not.toContain('accepted')
  expect(lastStatus(engine)).toContain('1 run')
})

test('escalations are toasted, and decisions within 3 s merge into one toast', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  const decision = (seq: number, what: string, reason: string) => event(RUN, seq, { kind: 'decision', what, reason })
  engine.stream.push(
    'stdout',
    decision(0, 'repair', 'checks_failed') + decision(1, 'escalate', 'tier_too_low') + decision(2, 'escalate', 'still_red'),
  )
  await settle(engine)
  const before = engine.calls.toasts.length
  await tick(engine, 1000)
  expect(engine.calls.toasts.length).toBe(before)
  await tick(engine, 2500)
  const merged = engine.calls.toasts.slice(before)
  expect(merged.length).toBe(1)
  expect(merged[0]).toContain('tier_too_low')
  expect(merged[0]).toContain('still_red')
  expect(merged[0]).not.toContain('checks_failed')
})

test('after /clear the timeline is reloaded from relais native status', async ($: any, on: any) => {
  on('classic.SessionStart', () => ({}))
  const engine = await startedRun($, on)
  // The shape `relais native status` prints: the summary, and the event
  // lines themselves (as `events.jsonl` holds them) to rebuild from.
  const ev = (seq: number, event: Record<string, unknown>) => ({
    relais: 'event',
    run: 'run-old',
    seq,
    at: `2026-10-06T11:00:0${seq}Z`,
    event,
  })
  const timeline = {
    run: 'run-old',
    phases: [{ at: '2026-10-06T11:00:01Z', state: 'running', reason: 'worker_dispatched' }],
    decisions: [{ at: '2026-10-06T11:00:05Z', what: 'repair', reason: 'checks_failed' }],
    cost: [],
    outcome: null,
    output: [],
    events: [
      ev(0, { kind: 'step', name: 'preflight', detail: 'route implementation · sonnet@medium', max_attempts: 3 }),
      ev(1, { kind: 'phase', state: 'running', reason: 'worker_dispatched', detail: {} }),
      ev(2, { kind: 'dispatch_started', dispatch: 'd1', agent_kind: 'worker', attempt: 1, model: 'sonnet', effort: 'medium' }),
      ev(3, { kind: 'dispatch_ended', dispatch: 'd1', agent: 'agent-x', outcome: 'exit 0', usage: null, cost: null }),
      ev(5, { kind: 'decision', what: 'repair', reason: 'checks_failed' }),
    ],
  }
  engine.script.runResult = (argv: string[]) =>
    argv[2] === 'status'
      ? { exitCode: 0, stdout: JSON.stringify(timeline), stderr: '', isStdoutTruncated: false, isStderrTruncated: false }
      : { exitCode: 0, stdout: '', stderr: '', isStdoutTruncated: false, isStderrTruncated: false }
  await $.classic.SessionStart({ source: 'clear' })
  await settle(engine)
  const reply = await $.tool.call({ tool: 'mcp__relais__status', run: 'run-old' })
  const [run] = JSON.parse(reply.result)
  expect(run.run).toBe('run-old')
  expect(run.decisions).toEqual(['repair · checks_failed'])
  expect(run.phases.map((p: any) => p.title)).toEqual(['preflight', 'attempt 1 · worker'])
  expect(run.maxAttempts).toBe(3)
  expect(engine.calls.run.some((c: any) => c.argv.join(' ') === 'relais native status')).toBe(true)
})

test('a child that exits with its run unfinished ends that run as interrupted', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  engine.stream.push('stdout', phase(0, 'running'))
  await settle(engine)
  engine.stream.end(137)
  await settle(engine)
  await tick(engine)
  const reply = await $.tool.call({ tool: 'mcp__relais__status' })
  const [run] = JSON.parse(reply.result)
  expect(run.outcome.state).toBe('interrupted')
})

test('a run refused before it started names why and the tool that fixes it', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  engine.stream.push(
    'stdout',
    line({ relais: 'done', run: null, outcome: 'blocked', receipt: null, summary: null, code: 'no_policy', detail: 'no relais.toml in the repository at /repo' }),
  )
  engine.stream.end(2)
  await settle(engine)
  await tick(engine)
  expect(engine.calls.prompts.length).toBe(1)
  const text = engine.calls.prompts[0].text
  expect(text).toContain('relais run blocked (no_policy): no relais.toml in the repository at /repo. Nothing ran.')
  expect(text).toContain('mcp__relais__onboard')
  expect(text).not.toContain('starting-')
})

test('a missing trust grant points the model at the trust tool', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  engine.stream.push(
    'stdout',
    phase(0, 'running') + done('blocked', { receipt: null, summary: null, code: 'missing_trust_grant', detail: 'no trust grant for this execution declaration' }),
  )
  engine.stream.end(3)
  await settle(engine)
  await tick(engine)
  expect(engine.calls.prompts.length).toBe(1)
  const text = engine.calls.prompts[0].text
  expect(text).toContain(`relais run ${RUN} finished: blocked (missing_trust_grant): no trust grant`)
  expect(text).toContain('mcp__relais__trust')
})

test('a child that exits without a done line still tells the model, once, with its last output', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  engine.stream.push('stderr', 'one\ntwo\nthree\nfour\nfive\nsix\nthread main panicked at src/main.rs:1\n')
  engine.stream.end(101)
  await settle(engine)
  await tick(engine)
  expect(engine.calls.prompts.length).toBe(1)
  const text = engine.calls.prompts[0].text
  expect(text).toContain('relais exited with code 101 without an outcome')
  expect(text).toContain('thread main panicked')
  expect(text).not.toContain('one\n')
})

test('a done line followed by the exit sends exactly one message', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  engine.stream.push('stdout', phase(0, 'running') + phase(1, 'accepted') + done())
  engine.stream.end(0)
  await settle(engine)
  await tick(engine)
  await tick(engine)
  expect(engine.calls.prompts.length).toBe(1)
})
