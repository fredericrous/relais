import { expect, test } from 'claude-code/testing'
import { launchableIntents } from '../hooks/runs.ts'
import { DECIDE_STEP, widenedText } from '../hooks/runs.ts'
import { event, line, nativeCalls, scriptedEngine, settle, startedRun } from './support.ts'

const result = (stdout: string, exitCode = 0) => ({ exitCode, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false })

const INTENT = { intent_id: 'intent-1', repo: '/repo', state: 'pending' }

// What `relais decide ... --json` prints for an answer that wrote an intent.
const decided = (intent: Record<string, unknown> = INTENT) =>
  JSON.stringify({
    run: 'run-1',
    resolution: 'decision_revised',
    actor: 'orchestrator',
    from_state: 'needs_decision',
    to_state: 'failed',
    advice: null,
    successor_intent: intent,
  })

// The runs `relais run --intent` started.
const launches = (engine: ReturnType<typeof scriptedEngine>) =>
  engine.calls.spawn.filter((e: any) => e.argv[1] === 'run' && e.argv.includes('--intent'))

const decide = ($: any, extra: Record<string, unknown> = {}) =>
  $.tool.call({ tool: 'mcp__relais__decide', run: 'run-1', answer: 'revise', widen_scope: true, ...extra })

test('launchableIntents reads the intents a status lists and nothing else', () => {
  expect(launchableIntents(JSON.stringify({ runs: [], intents: [INTENT] }))).toEqual([{ intent_id: 'intent-1', repo: '/repo' }])
  expect(launchableIntents(JSON.stringify({ runs: [] }))).toEqual([])
  expect(launchableIntents('not json')).toEqual([])
  // An intent that does not say where it runs cannot be launched.
  expect(launchableIntents(JSON.stringify({ intents: [{ intent_id: 'intent-2' }, { repo: '/r' }] }))).toEqual([])
})

test('a decide that returns a successor_intent launches it; a repeat launches it again and the relais claim refuses the duplicate', async ($: any, on: any) => {
  const engine = scriptedEngine(on)
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await settle(engine)
  engine.script.runResult = (argv: string[]) => (argv[1] === 'decide' ? result(decided()) : result(''))

  const first = await decide($)
  await settle(engine)
  expect(first.result).toContain('Started relais successor intent intent-1')
  const decideCall = engine.calls.run.find(c => c.argv[1] === 'decide')!
  expect(decideCall.argv).toContain('--widen-scope')
  expect(launches(engine).map((e: any) => e.argv)).toEqual([['relais', 'run', '--intent', 'intent-1', '--protocol']])
  expect(launches(engine)[0].cwd).toBe('/repo')

  // The same decide again launches the same intent; relais's claim, not the
  // plugin, refuses the duplicate.
  const again = await decide($)
  await settle(engine)
  expect(again.result).toContain('Started relais successor intent intent-1')
  expect(launches(engine).every((e: any) => e.argv[3] === 'intent-1')).toBe(true)
})

test('a pending intent found on start is launched, and a poll that still lists it launches it again', async ($: any, on: any) => {
  const engine = scriptedEngine(on)
  engine.script.runResult = (argv: string[]) =>
    argv[1] === 'native' && argv[2] === 'status' && argv.includes('--session')
      ? result(JSON.stringify({ runs: [], intents: [INTENT] }))
      : result('')
  await $.session.start({ cwd: '/elsewhere', surface: 'terminal', isInteractive: true })
  await settle(engine)
  // Launched where relais says the decided run worked, not where the
  // session happens to be.
  expect(launches(engine).map((e: any) => [e.argv[3], e.cwd])).toEqual([['intent-1', '/repo']])

  await $.tool.call({ tool: 'mcp__relais__status' })
  await settle(engine)
  // relais still lists it, so its successor is not running: relais's claim
  // decides, not the plugin.
  expect(launches(engine).length).toBe(2)
  expect(nativeCalls(engine.calls, 'status').some(c => c.argv.includes('--session'))).toBe(true)
})

test('an intent whose successor died is relaunched by a later poll in the same session, and not while it lives', async ($: any, on: any) => {
  const engine = scriptedEngine(on)
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await settle(engine)
  let listed = [INTENT]
  engine.script.runResult = (argv: string[]) =>
    argv.includes('--session') ? result(JSON.stringify({ runs: [], intents: listed })) : result('')
  await $.tool.call({ tool: 'mcp__relais__status' })
  await settle(engine)
  expect(launches(engine).length).toBe(1)
  // The successor lives: relais lists nothing, so nothing is launched.
  listed = []
  await $.tool.call({ tool: 'mcp__relais__status' })
  await settle(engine)
  expect(launches(engine).length).toBe(1)
  // It died: relais lists the intent again and the same session relaunches.
  listed = [INTENT]
  await $.tool.call({ tool: 'mcp__relais__status' })
  await settle(engine)
  expect(launches(engine).length).toBe(2)
})

test('a status poll launches an intent that appeared after start', async ($: any, on: any) => {
  const engine = scriptedEngine(on)
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await settle(engine)
  expect(launches(engine).length).toBe(0)
  engine.script.runResult = (argv: string[]) =>
    argv.includes('--session') ? result(JSON.stringify({ runs: [], intents: [INTENT] })) : result('')
  await $.tool.call({ tool: 'mcp__relais__status' })
  await settle(engine)
  expect(launches(engine).length).toBe(1)
})

test('a poll launches no run for an intent refused inside its backoff, and does after it', async ($: any, on: any) => {
  const engine = scriptedEngine(on)
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await settle(engine)
  const refused = (retryAfter: string) => ({
    ...INTENT,
    refusal: 'pricing: no price for model x',
    refusal_count: 1,
    retry_after: retryAfter,
  })
  let listed = [refused(new Date(Date.now() + 60 * 60_000).toISOString())]
  engine.script.runResult = (argv: string[]) =>
    argv.includes('--session') ? result(JSON.stringify({ runs: [], intents: listed })) : result('')
  await $.tool.call({ tool: 'mcp__relais__status' })
  await settle(engine)
  expect(launches(engine).length).toBe(0)
  listed = [refused(new Date(Date.now() - 1000).toISOString())]
  await $.tool.call({ tool: 'mcp__relais__status' })
  await settle(engine)
  expect(launches(engine).length).toBe(1)
})

test('a status relais cannot give launches nothing', async ($: any, on: any) => {
  const engine = scriptedEngine(on)
  engine.script.runResult = () => result('', 1)
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await settle(engine)
  expect(launches(engine).length).toBe(0)
})

const failing = (script: (argv: string[]) => any) => async ($: any, on: any) => {
  const engine = scriptedEngine(on)
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await settle(engine)
  engine.script.runResult = script
  const out = await $.tool.call({ tool: 'mcp__relais__status' })
  await settle(engine)
  expect(launches(engine).length).toBe(0)
  return String(out.result ?? out.deny)
}

test('a non-zero status exit is a typed failure and launches nothing', async ($: any, on: any) => {
  const text = await failing((argv: string[]) =>
    argv.includes('--session') ? { ...result('', 3), stderr: 'boom' } : result(''))($, on)
  expect(text).toContain('status_exit 3')
  expect(text).toContain('boom')
})

test('a thrown status error is a typed failure and launches nothing', async ($: any, on: any) => {
  const text = await failing((argv: string[]) => {
    if (argv.includes('--session')) throw new Error('spawn failed')
    return result('')
  })($, on)
  expect(text).toContain('status_threw')
})

test('unparsable status JSON is a typed failure and launches nothing', async ($: any, on: any) => {
  const text = await failing((argv: string[]) => (argv.includes('--session') ? result('not json') : result('')))($, on)
  expect(text).toContain('status_unparsable')
})

test('widen_scope and successor_contract belong to revise and exclude each other', async ($: any, on: any) => {
  const engine = scriptedEngine(on)
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await settle(engine)
  const wrong = await decide($, { answer: 'approve' })
  expect(wrong.deny ?? wrong.result).toContain('belong to answer revise')
  const both = await decide($, { successor_contract: 'next.json' })
  expect(both.deny ?? both.result).toContain('pass one')
  expect(engine.calls.run.filter(c => c.argv[1] === 'decide').length).toBe(0)
})

test('a successor_contract object is saved first and its path passed to decide', async ($: any, on: any) => {
  const engine = scriptedEngine(on)
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await settle(engine)
  engine.script.runResult = (argv: string[]) =>
    argv[2] === 'contract' ? result('/repo/.relais/tasks/next.json\n') : argv[1] === 'decide' ? result(decided()) : result('')
  const contract = { schema_version: 1, kind: 'verify', candidate: { run: 'run-1' } }
  const out = await $.tool.call({
    tool: 'mcp__relais__decide',
    run: 'run-1',
    answer: 'revise',
    successor_contract: contract,
    cwd: '/repo',
  })
  await settle(engine)
  expect(out.result).toContain('Started relais successor intent intent-1')
  const saved = nativeCalls(engine.calls, 'contract')
  expect(saved.length).toBe(1)
  expect(saved[0].payload).toEqual(contract)
  const call = engine.calls.run.find(c => c.argv[1] === 'decide')!
  expect(call.argv.slice(call.argv.indexOf('--successor-contract'))[1]).toBe('/repo/.relais/tasks/next.json')
  expect(launches(engine).length).toBe(1)
})

test('a review-parts answer is passed to relais as the answer and launches the successor it records', async ($: any, on: any) => {
  const engine = scriptedEngine(on)
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await settle(engine)
  engine.script.runResult = (argv: string[]) => (argv[1] === 'decide' ? result(decided()) : result(''))

  const reply = await $.tool.call({ tool: 'mcp__relais__decide', run: 'run-1', answer: 'review-parts' })
  await settle(engine)
  expect(reply.result).toContain('Started relais successor intent intent-1')
  const decideCall = engine.calls.run.find(c => c.argv[1] === 'decide')!
  expect(decideCall.argv.slice(0, 6)).toEqual(['relais', 'decide', 'run-1', '--answer', 'review-parts', '--actor'])
  expect(launches(engine).map((e: any) => e.argv)).toEqual([['relais', 'run', '--intent', 'intent-1', '--protocol']])
})

test('widen_scope does not go with review-parts', async ($: any, on: any) => {
  const engine = scriptedEngine(on)
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await settle(engine)
  const wrong = await decide($, { answer: 'review-parts' })
  expect(wrong.deny ?? wrong.result).toContain('belong to answer revise')
})

const WIDENED_RUN = 'run-widened-1'
const widenedPaths = ['tests/a_test.rs', ...Array.from({ length: 11 }, (_, i) => `tests/b${i}.rs`), 'x'.repeat(400)]
const successor = { intent_id: 'intent-w', repo: '/repo', widened_paths: widenedPaths }

test('one outcome line carrying a successor intent launches exactly one child and sends no decide prompt', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  engine.stream.push(
    'stdout',
    event(WIDENED_RUN, 0, { kind: 'outcome', state: 'cancelled', receipt: null, reason: 'widened', successor_intent: successor }) +
      line({ relais: 'done', run: WIDENED_RUN, outcome: 'cancelled', receipt: null, code: 'scope_widened_for_tests', detail: 'widened', successor_intent: successor }),
  )
  await settle(engine)
  await engine.clock.advance(200)
  await settle(engine)
  expect(launches(engine).map((e: any) => [e.argv, e.cwd])).toEqual([[['relais', 'run', '--intent', 'intent-w', '--protocol'], '/repo']])
  const prompts = engine.calls.prompts.map((p: any) => p.text).join('\n')
  expect(prompts).toContain('cancelled (scope_widened_for_tests)')
  expect(prompts).toContain("+3 more")
  expect(prompts).not.toContain(DECIDE_STEP)
  expect(prompts).not.toContain('tests/b9.rs')
})

test('the widened paths are quoted data, each cut to MAX_LINE, at most ten, then +k more', () => {
  const text = widenedText({ intent_id: 'i', repo: '/r', widened_paths: widenedPaths })
  const quoted = text.split('\n').filter(l => l.startsWith('  "'))
  expect(quoted.length).toBe(10)
  expect(text).toContain('  +3 more')
  expect(text).not.toContain('x'.repeat(300))
})

test('models rebuilt from status launch no child', async ($: any, on: any) => {
  const engine = scriptedEngine(on)
  // The status carries a finished run whose outcome names a successor
  // intent: rebuilding it must not launch that intent again (the outcome
  // line already did, or the status-poll reconcile owns it).
  const widened = {
    run: WIDENED_RUN,
    revision: 1,
    events: [
      { relais: 'event', run: WIDENED_RUN, seq: 0, at: '2026-10-23T12:00:00Z', event: { kind: 'outcome', state: 'cancelled', receipt: null, reason: 'widened', successor_intent: successor } },
    ],
    current: { state: 'cancelled', session: undefined },
  }
  engine.script.runResult = () => result(JSON.stringify({ runs: [widened], intents: [] }))
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await settle(engine)
  await $.tool.call({ tool: 'mcp__relais__status' })
  await settle(engine)
  expect(launches(engine)).toEqual([])
})
