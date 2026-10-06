import { expect, test } from 'claude-code/testing'
import { nativeCalls, settle, spawnLine, startedRun, startQueued } from './support.ts'

test('the run tool starts relais with the protocol flag, the host and the session', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  const [child] = engine.calls.spawn
  expect(child.argv).toEqual(['relais', 'run', '--task', 'fix it', '--protocol'])
  expect(child.cwd).toBe('/repo')
  expect(child.env).toEqual({ RELAIS_HOST: 'claude-code-mod', RELAIS_SESSION_ID: 'session-1' })
})

test('the run tool returns at once with what it started', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  const reply = await $.tool.call({ tool: 'mcp__relais__run', task: 'other', cwd: '/repo' })
  expect(reply.result).toContain('Started relais run for: other')
  await startQueued(engine)
  expect(engine.calls.spawn.length).toBe(2)
})

test('a spawn line spawns an agent in the hook-chosen cwd, then binds it', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  engine.stream.push('stdout', spawnLine('d1'))
  await settle(engine)
  expect(engine.calls.spawned.length).toBe(1)
  const spawned = engine.calls.spawned[0]
  expect(spawned.cwd).toBe('/work/d1')
  expect(spawned.description).toBe('relais d1')
  expect(spawned.subagent_type).toBe('relais:relais-worker-sonnet-medium')
  const bound = nativeCalls(engine.calls, 'bound')
  expect(bound.length).toBe(1)
  expect(bound[0].argv).toEqual(['relais', 'native', 'bound', '--dispatch', 'd1', '--agent', 'agent-1'])
})

test('two spawns in flight each get their own cwd', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  engine.stream.push('stdout', spawnLine('d1', { cwd: '/work/attempt-1' }) + spawnLine('d2', { cwd: '/work/review' }))
  await settle(engine)
  const cwds = engine.calls.spawned.map((s: any) => [s.description, s.cwd])
  expect(cwds).toEqual([
    ['relais d1', '/work/attempt-1'],
    ['relais d2', '/work/review'],
  ])
})

test("a spawn that is not the plugin's own or names no pending dispatch keeps its cwd", async ($: any, on: any) => {
  const engine = await startedRun($, on)
  await $.agent.spawn({ prompt: 'p', description: 'relais d9', subagentType: 'Explore', cwd: '/elsewhere' })
  await $.agent.spawn({ prompt: 'p', description: 'plain task', subagentType: 'Explore' })
  const [first, second] = engine.calls.spawned
  expect(first.cwd).toBe('/elsewhere')
  expect(second.cwd).toBe(undefined)
})

test('a stop line stops the agent through TaskStop', async ($: any, on: any) => {
  const engine = await startedRun($, on)
  engine.stream.push('stdout', spawnLine('d1'))
  await settle(engine)
  engine.stream.push('stdout', JSON.stringify({ relais: 'stop', dispatch: 'd1', agent: 'agent-1' }) + '\n')
  await settle(engine)
  const stops = engine.calls.tool.filter((t: any) => t.tool === 'TaskStop')
  expect(stops.length).toBe(1)
  expect(stops[0].task_id).toBe('agent-1')
})
