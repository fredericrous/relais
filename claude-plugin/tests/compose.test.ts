import { expect, test } from 'claude-code/testing'
import { scriptedEngine, settle } from './support.ts'

// The hook, run by the engine: the routing rule is composed into the system
// prompt of a Claude session and left out of any other model's.
const ids = async ($: any, on: any, promptModel: string) => {
  const engine = scriptedEngine(on)
  // The engine's own composition, beneath the plugin's hook.
  on('prompt.compose', () => {
    const sections = [{ id: 'base', text: 'You are an agent.', scope: 'shared' }]
    return { sections, value: { sections } }
  })
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await settle(engine)
  const composed = await $.prompt.compose({ promptModel, outputStyle: null, tools: [], traits: [], surfaces: [] })
  return composed?.sections?.map((s: any) => s.id) ?? []
}

test('the compose hook adds the routing rule for a Claude model', async ($: any, on: any) => {
  expect(await ids($, on, 'claude-sonnet-5-5')).toContain('relais-routing')
})

test('the compose hook leaves the routing rule out for a proxy model', async ($: any, on: any) => {
  expect(await ids($, on, 'gpt-5')).not.toContain('relais-routing')
})
