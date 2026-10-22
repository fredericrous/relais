import { expect, test } from 'claude-code/testing'
import { nativeCalls, scriptedEngine, settle, startedRun } from './support.ts'
import { LEARNING_ENVELOPE_MESSAGE } from '../hooks/guards.ts'

const start = ($: any) => $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
const lastOf = (engine: any, row: unknown) => {
  engine.script.runResult = (argv: string[]) =>
    argv[1] === 'learn' && argv[2] === 'last'
      ? { exitCode: 0, stdout: JSON.stringify({ schema: 1, last: row }), stderr: '' }
      : { exitCode: 0, stdout: '', stderr: '' }
}
const CYCLE = ['relais', 'learn', 'cycle', '--if-due', '--log', '/Users/p/.local/state/relais/learn-cycle.log']

test('session start and end spawn the cycle detached: no run, no announcement, no outcome', async ($: any, on: any) => {
  const engine = scriptedEngine(on)
  await start($)
  await settle(engine)
  await start($)
  await settle(engine)
  expect(engine.calls.learn.length).toBe(2)
  for (const spawn of engine.calls.learn) {
    expect(spawn.argv).toEqual(CYCLE)
    expect(spawn.argv).not.toContain('--protocol')
  }
  expect(engine.calls.spawn.length).toBe(0)
  expect(engine.calls.prompts.length).toBe(0)
  expect(engine.calls.tool.length).toBe(0)
  await $.session.end({ reason: 'prompt_input_exit', sessionId: 'session-1', resume: { id: 'session-1' } })
  await settle(engine)
  expect(engine.calls.learn.length).toBe(3)
  expect(engine.calls.learn[2].argv).toEqual(CYCLE)
  expect(engine.calls.spawn.length).toBe(0)
  expect(engine.calls.prompts.length).toBe(0)
})

test('session start sweeps orphaned runs detached: no run, no announcement, no outcome', async ($: any, on: any) => {
  const engine = scriptedEngine(on)
  await start($)
  await settle(engine)
  expect(engine.calls.resume.length).toBe(1)
  expect(engine.calls.resume[0].argv).toEqual([
    'relais',
    'resume',
    '--retire',
    '--all',
    '--log',
    '/Users/p/.local/state/relais/resume.log',
  ])
  expect(engine.calls.spawn.length).toBe(0)
  expect(engine.calls.prompts.length).toBe(0)
  expect(engine.calls.toasts.length).toBe(0)
})

for (const [action, names] of [
  ['promoted', ['relais report', 'relais rollback']],
  ['rolled_back', ['relais report']],
  ['error', ['relais report']],
] as const) {
  test(`a new ${action} row earns one line, and not again`, async ($: any, on: any) => {
    const engine = scriptedEngine(on)
    lastOf(engine, { id: 7, action })
    await start($)
    await settle(engine)
    expect(engine.calls.toasts.length).toBe(1)
    for (const name of names) expect(engine.calls.toasts[0]).toContain(name)
    await start($)
    await settle(engine)
    expect(engine.calls.toasts.length).toBe(1)
    lastOf(engine, { id: 8, action })
    await start($)
    await settle(engine)
    expect(engine.calls.toasts.length).toBe(2)
  })
}

test('a previous promoted row is noted before this session starts its own cycle', async ($: any, on: any) => {
  const engine = scriptedEngine(on)
  lastOf(engine, { id: 5, action: 'promoted' })
  await start($)
  await settle(engine)
  expect(engine.calls.toasts.length).toBe(1)
  expect(engine.calls.learn.length).toBe(1)
  expect(engine.calls.toastsAtLearnSpawn).toEqual([1])
})

test('a cycle that cannot be spawned leaves one pane note', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  engine.script.learnSpawnFails = true
  await start($)
  await settle(engine)
  const status = await $.tool.call({ tool: 'mcp__relais__status', run: JSON.parse((await $.tool.call({ tool: 'mcp__relais__status' })).result).runs[0].run })
  expect(JSON.stringify(status)).toContain('learning cycle could not be started')
})

test('the line goes to the pane when a run is there', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  lastOf(engine, { id: 3, action: 'promoted' })
  await start($)
  await settle(engine)
  const status = await $.tool.call({ tool: 'mcp__relais__status', run: JSON.parse((await $.tool.call({ tool: 'mcp__relais__status' })).result).runs[0].run })
  expect(JSON.stringify(status)).toContain('relais rollback')
})

test('none, awaiting_promotion, interrupted, no row and a failing read earn no line', async ($: any, on: any) => {
  const engine = scriptedEngine(on)
  for (const row of [
    { id: 1, action: 'none' },
    { id: 2, action: 'awaiting_promotion' },
    { id: 3, action: 'interrupted' },
    null,
  ]) {
    lastOf(engine, row)
    await start($)
    await settle(engine)
  }
  engine.script.runResult = () => ({ exitCode: 1, stdout: '', stderr: 'boom' })
  await start($)
  await settle(engine)
  expect(engine.calls.toasts.length).toBe(0)
})

test('/relais-learning on asks, then records the envelope; off removes it unasked', async ($: any, on: any) => {
  const engine = scriptedEngine(on)
  await start($)
  await settle(engine)
  const presentation = { layout: 'main', columns: 120 }
  engine.script.ask = () => 'Not now'
  const declined = await $.command.run({ command: 'relais-learning', args: 'on', origin: { kind: 'composer' }, presentation })
  expect(declined.text).toContain('not granted')
  expect(nativeCalls(engine.calls, 'learning-envelope').length).toBe(0)
  engine.script.ask = () => 'Allow automatic promotion'
  const granted = await $.command.run({ command: 'relais-learning', args: 'on', origin: { kind: 'composer' }, presentation })
  expect(granted.text).toContain('granted')
  const [call] = nativeCalls(engine.calls, 'learning-envelope')
  expect(call.argv).toEqual([
    'relais', 'native', 'learning-envelope',
    '--by', 'the person in Claude Code session session-1',
    '--source', 'plugin-ask',
  ])
  expect(engine.calls.asked[engine.calls.asked.length - 1].question).toContain('/relais-learning off')
  const asked = engine.calls.asked.length
  const off = await $.command.run({ command: 'relais-learning', args: ' off ', origin: { kind: 'composer' }, presentation })
  expect(off.text).toContain('off')
  expect(engine.calls.asked.length).toBe(asked)
  const removed = nativeCalls(engine.calls, 'learning-envelope')[1]
  expect(removed.argv).toEqual([
    'relais', 'native', 'learning-envelope', '--off',
    '--by', 'the person in Claude Code session session-1',
    '--source', 'plugin-ask',
  ])
})

test('the model’s learning-envelope calls and [learning] writes are refused', async ($: any, on: any) => {
  const engine = scriptedEngine(on)
  await start($)
  await settle(engine)
  const check = (tool: string, input: Record<string, unknown>) => $.tool.check({ tool, input, ...input })
  const reason = async (tool: string, input: Record<string, unknown>) => (await check(tool, input)).reason
  const refused = async (tool: string, input: Record<string, unknown>) => (await check(tool, input)).decision
  expect(await reason('Bash', { command: 'relais native learning-envelope --by me' })).toBe(LEARNING_ENVELOPE_MESSAGE)
  expect(LEARNING_ENVELOPE_MESSAGE).toContain('/relais-learning')
  expect(await reason('Write', { file_path: '/Users/p/.config/relais/machine.toml', content: '[learning]\nauto_promote = {}' })).toBe(
    LEARNING_ENVELOPE_MESSAGE,
  )
  expect(await reason('Edit', { file_path: '/Users/p/.config/relais/machine.toml', old_string: 'a', new_string: '[learning]' })).toBe(
    LEARNING_ENVELOPE_MESSAGE,
  )
  expect(await refused('Bash', { command: "echo '[learning]' >> ~/.config/relais/machine.toml" })).toBe('deny')
  expect(await refused('Bash', { command: 'relais native learning-envelope --off --by me' })).toBe('deny')
  expect(await refused('Bash', { command: 'relais learn last --json' })).not.toBe('deny')
  expect(await refused('Write', { file_path: '/repo/notes.md', content: '[learning]' })).not.toBe('deny')
})
