import { expect, test } from 'claude-code/testing'
import { continueLine, RELAIS_AGENT_TYPE, settle, spawnLine, startedRun } from './support.ts'

const offer = (agent: string) => ({
  agent,
  description: '',
  source: 'plugin',
  provider: { plugin: 'relais', tier: 'user' },
})

const notification = (id: string) =>
  `<task-notification>\n<task-id>${id}</task-id>\n<status>completed</status>\n</task-notification>`

const prompt = (text: string, kind: string) => ({ text, wait: false, origin: { kind } })

async function spawned($: any, on: any) {
  const engine = await startedRun($, on)
  engine.stream.push('stdout', spawnLine('d1'))
  await settle(engine)
  return engine
}

test('agent.offer hides relais:* from the model and offers the rest', async ($: any, on: any) => {
  await startedRun($, on)
  expect((await $.agent.offer(offer(RELAIS_AGENT_TYPE))).isOffered).toBe(false)
  expect((await $.agent.offer(offer('relais:planner'))).isOffered).toBe(false)
  expect((await $.agent.offer(offer('Explore'))).isOffered).toBe(true)
})

test('agent.offer lets a relais type through while the plugin resumes its own agent', async ($: any, on: any) => {
  const engine = await spawned($, on)
  const during: boolean[] = []
  engine.script.duringTool = async (_hook$: any, e: any) => {
    if (e.tool === 'SendMessage') during.push((await $.agent.offer(offer(RELAIS_AGENT_TYPE))).isOffered)
  }
  engine.stream.push('stdout', continueLine('d2', 'agent-1'))
  await settle(engine)
  expect(during).toEqual([true])
  expect((await $.agent.offer(offer(RELAIS_AGENT_TYPE))).isOffered).toBe(false)
})

test('a model SendMessage or TaskStop aimed at a relais agent is denied; the plugin’s own goes through', async ($: any, on: any) => {
  const engine = await spawned($, on)
  const message = await $.tool.call({ tool: 'SendMessage', to: 'agent-1', message: 'hello' })
  expect(typeof message.deny).toBe('string')
  const stop = await $.tool.call({ tool: 'TaskStop', task_id: 'agent-1' })
  expect(typeof stop.deny).toBe('string')
  expect(engine.calls.tool.filter((t: any) => t.tool === 'SendMessage' || t.tool === 'TaskStop').length).toBe(0)

  // Another agent is none of relais's business.
  const other = await $.tool.call({ tool: 'SendMessage', to: 'someone-else', message: 'hello' })
  expect(other.deny).toBe(undefined)
  // The plugin's own continue passes the same guard.
  engine.stream.push('stdout', continueLine('d2', 'agent-1'))
  await settle(engine)
  const sent = engine.calls.tool.filter((t: any) => t.tool === 'SendMessage')
  expect(sent.map((t: any) => t.to)).toEqual(['someone-else', 'agent-1'])
})

test('a relais agent’s task-notification is dropped; an unrelated one that mentions its id is kept', async ($: any, on: any) => {
  const engine = await spawned($, on)
  const dropped = await $.prompt.submit(prompt(notification('agent-1'), 'task-notification'))
  expect(typeof dropped.drop).toBe('string')

  const unrelated = `<task-notification>\n<task-id>other-agent</task-id>\n<summary>agent-1 helped</summary>\n</task-notification>`
  const kept = await $.prompt.submit(prompt(unrelated, 'task-notification'))
  expect(kept.drop).toBe(undefined)
  const mention = await $.prompt.submit(prompt('what did agent-1 do?', 'composer'))
  expect(mention.drop).toBe(undefined)
  // Typed by the person, a prompt that looks like a notification is theirs.
  const typed = await $.prompt.submit(prompt(notification('agent-1'), 'composer'))
  expect(typed.drop).toBe(undefined)
  expect(engine.calls.prompts.map((p: any) => p.text)).toEqual([
    unrelated,
    'what did agent-1 do?',
    notification('agent-1'),
  ])
})
