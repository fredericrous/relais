import { expect, test } from 'claude-code/testing'
import { launchableIntents } from '../hooks/runs.ts'
import { nativeCalls, scriptedEngine, settle } from './support.ts'

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

test('a decide that returns a successor_intent claims and launches it once; a repeat does not launch again', async ($: any, on: any) => {
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

  // The same decide again: relais returns the same intent, the plugin has
  // already launched it.
  const again = await decide($)
  await settle(engine)
  expect(again.result).toContain('already launched')
  expect(launches(engine).length).toBe(1)
})

test('a pending intent found on start is launched once, and a later status poll does not launch it again', async ($: any, on: any) => {
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
  expect(launches(engine).length).toBe(1)
  expect(nativeCalls(engine.calls, 'status').some(c => c.argv.includes('--session'))).toBe(true)
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

test('a status relais cannot give launches nothing', async ($: any, on: any) => {
  const engine = scriptedEngine(on)
  engine.script.runResult = () => result('', 1)
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await settle(engine)
  expect(launches(engine).length).toBe(0)
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
