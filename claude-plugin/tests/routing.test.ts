// The session router through the plugin's hooks: what reaches the engine's
// `turn.step` and `agent.spawn`, and what is recorded.

import { expect, test } from 'claude-code/testing'
import { nativeCalls, settle, spawnLine, startQueued } from './support.ts'
import {
  answered,
  bash,
  edit,
  endSession,
  failingCheck,
  label,
  nextStep,
  observed,
  passingCheck,
  personTurn,
  routedSession,
  step,
  toolCall,
} from './routing-support.ts'

const HAIKU = 'claude-haiku-5-5'
const SONNET = 'claude-sonnet-5-5'
const OPUS = 'claude-opus-5-5'

test('the engine’s turn.step receives the routed full model id, and later steps keep it', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  const { steps } = await personTurn($, engine, 'rename foo to bar, tests exist', 2)
  expect(steps.map((s: any) => s.model)).toEqual([HAIKU, HAIKU])
  expect(engine.calls.completes.length).toBe(1)
  const request = engine.calls.completes[0]
  expect(request.effort).toBe('low')
  expect(request.maxTokens).toBe(160)
  expect(request.timeoutMs).toBe(3000)
})

test('a failed verification after an edit escalates one tier from the next request', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  const { turnId, steps } = await personTurn($, engine, 'rename foo to bar, tests exist')
  expect(steps[0].model).toBe(HAIKU)
  await toolCall($, engine, edit('/repo/src/a.rs'), { result: {}, text: 'ok' })
  await toolCall($, engine, bash('cargo test'), failingCheck)
  const next = await nextStep($, engine, turnId, 1)
  expect(next.model).toBe(SONNET)
  await endSession($, engine)
  const reassess = observed(engine, 'reassess')
  expect(reassess.map((r: any) => [r.event, r.tier_from, r.tier_to, r.escalating])).toEqual([
    ['failed_verification', 'research', 'implementation', true],
  ])
})

test('a red run before any edit gives no escalation', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  const { turnId } = await personTurn($, engine, 'fix the parser, tests exist')
  await toolCall($, engine, bash('cargo test'), failingCheck)
  expect((await nextStep($, engine, turnId, 1)).model).toBe(HAIKU)
  await endSession($, engine)
  expect(observed(engine, 'reassess').length).toBe(0)
})

test('two failed repairs plus a correction give a strictly stronger model, no "try again" needed', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  const { turnId } = await personTurn($, engine, 'fix the parser, tests exist')
  const seen: [string, string | undefined][] = []
  for (let i = 1; i <= 2; i++) {
    await toolCall($, engine, edit('/repo/src/parser.rs'), { result: {}, text: 'ok' })
    await toolCall($, engine, bash('cargo test'), failingCheck)
    const s = await nextStep($, engine, turnId, i)
    seen.push([s.model, s.effort])
  }
  engine.labels.set('still wrong', label({ relation: 'correction', explicit: { value: 'correct', quote: 'still wrong' } }))
  const { steps } = await personTurn($, engine, 'still wrong')
  seen.push([steps[0].model, steps[0].effort])
  expect(seen).toEqual([
    [SONNET, undefined],
    [OPUS, undefined],
    [OPUS, 'xhigh'],
  ])
  await endSession($, engine)
  const [task] = observed(engine, 'task')
  expect(task.outcome).toBe('corrected')
  expect(task.escalations).toBe(2)
  expect(task.exhausted).toBe(true)
})

test('at most two escalations per task, then exhausted', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  const { turnId } = await personTurn($, engine, 'fix the parser, tests exist')
  for (let i = 1; i <= 4; i++) {
    await toolCall($, engine, edit('/repo/src/parser.rs'), { result: {}, text: 'ok' })
    await toolCall($, engine, bash('cargo test'), failingCheck)
    await nextStep($, engine, turnId, i)
  }
  expect(engine.calls.steps[engine.calls.steps.length - 1].model).toBe(OPUS)
  await endSession($, engine)
  const [task] = observed(engine, 'task')
  expect(task.escalations).toBe(2)
  expect(task.exhausted).toBe(true)
  expect(observed(engine, 'reassess').map((r: any) => r.tier_to)).toEqual(['implementation', 'escalation', 'escalation', 'escalation'])
})

test('three Explore spawns plus a growing scope with green tests give 0 escalations', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  const { turnId } = await personTurn($, engine, 'rename foo to bar everywhere, tests exist')
  for (const n of [1, 2, 3]) {
    await $.agent.spawn({ prompt: `find the uses of foo, part ${n}`, description: `find foo ${n}`, subagentType: 'Explore', model: 'opus' })
  }
  await toolCall($, engine, edit('/repo/src/a.rs'), { result: {}, text: 'ok' })
  await toolCall($, engine, edit('/repo/tests/b.rs'), { result: {}, text: 'ok' })
  await toolCall($, engine, bash('cargo test'), passingCheck)
  expect((await nextStep($, engine, turnId, 1)).model).toBe(HAIKU)
  await endSession($, engine)
  const reassess = observed(engine, 'reassess')
  expect(reassess.map((r: any) => r.event).sort()).toEqual(['scope_growth', 'spawn', 'spawn', 'spawn'])
  expect(reassess.every((r: any) => r.escalating === false)).toBe(true)
  const main = observed(engine, 'task').find((t: any) => t.agent_id === null)
  expect(main.escalations).toBe(0)
  expect(main.outcome).toBe('completed_verified')
})

test('a classifier timeout on "continue" inside an escalated task keeps the escalation', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  const { turnId } = await personTurn($, engine, 'fix the parser, tests exist')
  await toolCall($, engine, edit('/repo/src/parser.rs'), { result: {}, text: 'ok' })
  await toolCall($, engine, bash('cargo test'), failingCheck)
  await nextStep($, engine, turnId, 1)
  engine.labels.set('continue', () => new Promise(() => {}))
  await $.prompt.submit({ text: 'continue', wait: false, origin: { kind: 'composer' } })
  const pending = step($, { turnId: 'turn-late', index: 0 })
  await engine.clock.advance(2000)
  await pending
  expect(engine.calls.steps[engine.calls.steps.length - 1].model).toBe(SONNET)
  await endSession($, engine)
  const late = observed(engine, 'decision').find((d: any) => d.turn_id === 'turn-late')
  expect(late.reason).toBe('timeout_kept')
  expect(late.tier).toBe('implementation')
})

test('the classifier timing out with no task means next(e)', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  engine.fallback = () => new Promise(() => {})
  await $.prompt.submit({ text: 'hello', wait: false, origin: { kind: 'composer' } })
  const pending = step($, { turnId: 'turn-x', index: 0, model: 'claude-sonnet-5-5', effort: 'medium' })
  await engine.clock.advance(2000)
  await pending
  const [request] = engine.calls.steps
  expect(request.model).toBe(SONNET)
  expect(request.effort).toBe('medium')
})

test('after /clear, "continue" is classified as a new task', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'fix the parser, tests exist')
  await endSession($, engine, 'clear')
  engine.labels.set('continue', label({ relation: 'continuation' }))
  await personTurn($, engine, 'continue')
  await endSession($, engine)
  const decisions = observed(engine, 'decision')
  expect(decisions.map((d: any) => d.relation)).toEqual(['new_task', 'new_task'])
  expect(decisions[0].task_id === decisions[1].task_id).toBe(false)
  // Router-state is read again for the session that follows the clear.
  expect(nativeCalls(engine.calls, 'router-state').length >= 2).toBe(true)
})

test('a continuation inherits the task: an easier follow-up keeps its class and tier', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  engine.labels.set('subtle', label({ difficulty: 3, uncertainty: 'medium' }))
  engine.labels.set('yes, do it', label({ relation: 'continuation', difficulty: 1, kind: 'question' }))
  const first = await personTurn($, engine, 'a subtle change in the parser')
  const second = await personTurn($, engine, 'yes, do it')
  expect(first.steps[0].model).toBe(SONNET)
  expect(second.steps[0].model).toBe(SONNET)
  await endSession($, engine)
  const [, follow] = observed(engine, 'decision')
  expect(follow.relation).toBe('continuation')
  expect(follow.class.difficulty).toBe(3)
  expect(follow.class.kind).toBe('edit')
})

test('a relais worker is never routed: its spawn keeps the rung model and is not classified', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await $.tool.call({ tool: 'mcp__relais__run', task: 'fix-it.json', cwd: '/repo' })
  await startQueued(engine)
  engine.stream.push('stdout', spawnLine('d1'))
  await settle(engine)
  expect(engine.calls.spawned[0].model).toBe('sonnet')
  expect(engine.calls.completes.length).toBe(0)
  // Its requests pass untouched: it has no router task.
  await step($, { turnId: 'w1', index: 0, agentId: 'agent-1', model: 'claude-sonnet-5-5' })
  expect(engine.calls.steps[0].model).toBe(SONNET)
})

test('a relais worker’s requests are not booked against the person’s task', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'rename foo to bar, tests exist')
  await $.tool.call({ tool: 'mcp__relais__run', task: 'fix-it.json', cwd: '/repo' })
  await startQueued(engine)
  engine.stream.push('stdout', spawnLine('d1'))
  await settle(engine)
  await step($, { turnId: 'w1', index: 0, agentId: 'agent-1', model: 'claude-sonnet-5-5' })
  await personTurn($, engine, 'and baz, tests exist')
  await settle(engine)
  const usage = observed(engine).filter((r: any) => r.kind === 'usage' && r.source === 'step')
  expect(usage.length).toBeGreaterThan(0)
  expect(usage.some((r: any) => r.agent_id === 'agent-1')).toBe(false)
})

test('a subagent is routed only when the router created it; the parent’s model is overridden', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'rename foo to bar, tests exist')
  engine.labels.set('look up', label({ kind: 'question', difficulty: 1 }))
  await $.agent.spawn({ prompt: 'look up where foo is defined', description: 'find foo', subagentType: 'Explore', model: 'opus' })
  const spawned = engine.calls.spawned[0]
  expect(spawned.model).toBe(HAIKU)
  await step($, { turnId: 's1', index: 0, agentId: 'agent-1', model: OPUS })
  await step($, { turnId: 's2', index: 0, agentId: 'stranger', model: OPUS })
  const [routed, stranger] = engine.calls.steps.slice(-2)
  expect(routed.model).toBe(HAIKU)
  expect(stranger.model).toBe(OPUS)
})

test('a subagent’s failed check escalates only that subagent', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  const { turnId } = await personTurn($, engine, 'rename foo to bar, tests exist')
  for (const n of [1, 2]) {
    await $.agent.spawn({ prompt: 'rename in the module', description: 'rename foo', subagentType: 'general-purpose' })
    expect(engine.calls.spawned[n - 1].model).toBe(HAIKU)
  }
  await toolCall($, engine, edit('/repo/src/a.rs', 'agent-1'), { result: {}, text: 'ok' })
  await toolCall($, engine, bash('cargo test', 'agent-1'), failingCheck)
  await step($, { turnId: 'a1', index: 1, agentId: 'agent-1' })
  await step($, { turnId: 'a2', index: 1, agentId: 'agent-2' })
  const main = await nextStep($, engine, turnId, 1)
  const [one, two] = engine.calls.steps.slice(-3, -1)
  expect(one.model).toBe(SONNET)
  expect(two.model).toBe(HAIKU)
  expect(main.model).toBe(HAIKU)
  // Its completion ends its task.
  await $.turn.complete({ agentId: 'agent-1', answer: 'done', durationMs: 1, isAborted: false, turnId: 'a1', reason: 'answer' })
  await endSession($, engine)
  const sub = observed(engine, 'task').find((t: any) => t.agent_id === 'agent-1')
  expect(sub.escalations).toBe(1)
})

test('two concurrent spawns with one description and no agent id are both left unkeyed', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'rename foo to bar, tests exist')
  engine.script.spawnHasAgentId = false
  await $.agent.spawn({ prompt: 'part one', description: 'rename foo', subagentType: 'general-purpose' })
  await $.agent.spawn({ prompt: 'part two', description: 'rename foo', subagentType: 'general-purpose' })
  await step($, { turnId: 'u1', index: 0, agentId: 'agent-1', model: OPUS })
  await step($, { turnId: 'u2', index: 0, agentId: 'agent-2', model: OPUS })
  expect(engine.calls.steps.slice(-2).map((s: any) => s.model)).toEqual([OPUS, OPUS])
})

test('a spawn without an agent id is keyed at its first request when its description is unique', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'rename foo to bar, tests exist')
  engine.script.spawnHasAgentId = false
  await $.agent.spawn({ prompt: 'only one', description: 'rename bar', subagentType: 'general-purpose' })
  await step($, { turnId: 'k1', index: 0, agentId: 'agent-1', model: OPUS })
  expect(engine.calls.steps[engine.calls.steps.length - 1].model).toBe(HAIKU)
})

test('a model named in the prompt is honoured, and an agent definition’s pin is kept', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  engine.labels.set('with opus', label({ difficulty: 1, user_model: 'opus' }))
  const { steps } = await personTurn($, engine, 'rename foo with opus')
  expect(steps[0].model).toBe(OPUS)
  await $.agent.spawn({ prompt: 'p', description: 'pinned work', subagentType: 'pinned-agent', model: 'sonnet' })
  expect(engine.calls.spawned[0].model).toBe('sonnet')
  // A pinned type is not even classified.
  expect(engine.calls.completes.length).toBe(1)
})

test('a model the person names for a subagent goes to that subagent, never to the session', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  engine.labels.set('Explore agent with opus', label({ difficulty: 1, kind: 'question', user_model: 'opus', user_model_for: 'subagent' }))
  const { steps } = await personTurn($, engine, 'use an Explore agent with opus to list the files')
  expect(steps[0].model).toBe(HAIKU)
  await $.agent.spawn({ prompt: 'list the files', description: 'list files', subagentType: 'Explore' })
  expect(engine.calls.spawned[engine.calls.spawned.length - 1].model).toBe(OPUS)
})

test('an isOwn or notification prompt is never classified', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await $.prompt.submit({ text: '<task-notification><task-id>x</task-id></task-notification>', wait: false, origin: { kind: 'task-notification' } })
  await $.prompt.submit({ text: 'scheduled', wait: false, origin: { kind: 'scheduled-trigger' } })
  // The plugin's own verdict message, submitted by its timer.
  await $.tool.call({ tool: 'mcp__relais__run', task: 'fix-it.json', cwd: '/repo' })
  await startQueued(engine)
  engine.stream.push('stdout', JSON.stringify({ relais: 'done', run: 'run-1', outcome: 'accepted' }) + '\n')
  await settle(engine)
  await engine.clock.advance(100)
  await settle(engine)
  expect(engine.calls.prompts.some((p: any) => String(p.text).includes('finished: accepted'))).toBe(true)
  expect(engine.calls.completes.length).toBe(0)
})

async function appliesNothing($: any, on: any, state: Record<string, unknown>) {
  const engine = await routedSession($, on, { state })
  const { steps } = await personTurn($, engine, 'rename foo to bar, tests exist')
  expect(steps[0].model).toBe(SONNET)
  await $.agent.spawn({ prompt: 'p', description: 'look', subagentType: 'Explore', model: 'opus' })
  expect(engine.calls.spawned[engine.calls.spawned.length - 1].model).toBe('opus')
  await endSession($, engine)
  const decisions = observed(engine, 'decision')
  expect(decisions.length).toBe(2)
  expect(decisions.every((d: any) => d.applied === false)).toBe(true)
  expect(decisions[0].tier).toBe('research')
  return decisions
}

test('a shadow session decides and records, and applies nothing', async ($: any, on: any) => {
  const decisions = await appliesNothing($, on, { mode: 'shadow' })
  expect(decisions[0].mode_effective).toBe('shadow')
})

test('a held-out session decides and records, and applies nothing', async ($: any, on: any) => {
  const decisions = await appliesNothing($, on, { holdout: true })
  expect(decisions[0].mode_effective).toBe('on')
  expect(decisions[0].holdout).toBe(true)
})

test('mode_effective: an envelope without the plugin’s consent record stays shadow, with one toast', async ($: any, on: any) => {
  const engine = await routedSession($, on, { store: { r3_consent: { id: 'r3-1' } } })
  const { steps } = await personTurn($, engine, 'rename foo to bar, tests exist')
  expect(steps[0].model).toBe(SONNET)
  await personTurn($, engine, 'rename baz too, tests exist')
  await settle(engine)
  const toasts = engine.calls.toasts.filter(t => t.includes('/relais-routing'))
  expect(toasts.length).toBe(1)
  await endSession($, engine)
  expect(observed(engine, 'decision').every((d: any) => d.mode_effective === 'shadow')).toBe(true)
})

test('mode_effective: an R3 provenance row without the matching /relais-r3 record stays shadow', async ($: any, on: any) => {
  const engine = await routedSession($, on, {
    store: { envelope_consent: { answer: 'Allow session routing', at: 'x', session: 's' }, r3_consent: { id: 'r3-0' } },
  })
  const { steps } = await personTurn($, engine, 'rename foo to bar, tests exist')
  expect(steps[0].model).toBe(SONNET)
})

test('mode_effective: both records and router-state on switch routing on, shown on the status line', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  const { steps } = await personTurn($, engine, 'rename foo to bar, tests exist')
  expect(steps[0].model).toBe(HAIKU)
  await engine.clock.advance(100)
  await settle(engine)
  expect(engine.calls.statuses[engine.calls.statuses.length - 1]).toBe('relais · research (haiku-5-5)')
})

test('observations: one router-observe per prompt; flushed at session.end', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'rename foo to bar, tests exist', 2)
  expect(nativeCalls(engine.calls, 'router-observe').length).toBe(0)
  await personTurn($, engine, 'and baz, tests exist')
  await settle(engine)
  const calls = nativeCalls(engine.calls, 'router-observe')
  expect(calls.length).toBe(1)
  expect(calls[0].payload.schema).toBe(1)
  expect(calls[0].payload.session).toBe('session-1')
  expect(calls[0].payload.records.map((r: any) => `${r.kind}:${r.source ?? ''}`)).toEqual([
    'usage:classifier',
    'decision:',
    'usage:step',
    'usage:step',
  ])
  await endSession($, engine)
  expect(nativeCalls(engine.calls, 'router-observe').length).toBe(2)
  const kinds = observed(engine).map((r: any) => r.kind)
  expect(kinds.filter(k => k === 'task').length).toBe(2)
})

test('observations: exit 2 is dropped and noted; exit 1 is retried at most five times', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'rename foo to bar, tests exist')
  engine.observeExits.push(2)
  await personTurn($, engine, 'and baz, tests exist')
  await settle(engine)
  await engine.clock.advance(60_000)
  await settle(engine)
  expect(nativeCalls(engine.calls, 'router-observe').length).toBe(1)

  engine.observeExits.push(1, 1, 1, 1, 1, 1, 1)
  await personTurn($, engine, 'and qux, tests exist')
  await settle(engine)
  for (let i = 0; i < 8; i++) {
    await engine.clock.advance(16_000)
    await settle(engine)
  }
  // The first try and five retries, then dropped.
  expect(nativeCalls(engine.calls, 'router-observe').length).toBe(1 + 6)
})

test('/relais-flag marks the task corrected and moves its next request up', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  const { turnId } = await personTurn($, engine, 'rename foo to bar, tests exist')
  const reply = await $.command.run({ command: 'relais-flag', args: '', origin: { kind: 'composer' }, presentation: { layout: 'main', columns: 120 } })
  expect(reply.text).toContain('corrected')
  expect((await nextStep($, engine, turnId, 1)).model).toBe(SONNET)
  await endSession($, engine)
  expect(observed(engine, 'task')[0].outcome).toBe('corrected')
  expect(observed(engine, 'reassess')[0].event).toBe('flag')
})

test('/relais-routing writes the envelope through relais and the plugin’s record only on the yes', async ($: any, on: any) => {
  const engine = await routedSession($, on, { store: { r3_consent: { id: 'r3-1' } } })
  engine.script.ask = () => 'Not now'
  const presentation = { layout: 'main', columns: 120 }
  const declined = await $.command.run({ command: 'relais-routing', args: '', origin: { kind: 'composer' }, presentation })
  expect(declined.text).toContain('not granted')
  expect(nativeCalls(engine.calls, 'router-envelope').length).toBe(0)
  expect((await personTurn($, engine, 'rename foo to bar, tests exist')).steps[0].model).toBe(SONNET)
  engine.script.ask = () => 'Allow session routing'
  const granted = await $.command.run({ command: 'relais-routing', args: '', origin: { kind: 'composer' }, presentation })
  expect(granted.text).toContain('granted')
  const [call] = nativeCalls(engine.calls, 'router-envelope')
  expect(call.argv).toEqual([
    'relais', 'native', 'router-envelope',
    '--by', 'the person in Claude Code session session-1',
    '--epsilon-max', '0.1',
    '--source', 'plugin-ask',
  ])
  expect(engine.calls.asked[0].question).toContain('ε ≤ 0.1')
  // The plugin's record, with the R3 one already there, switches routing on.
  const { steps } = await personTurn($, engine, 'rename foo to bar, tests exist')
  expect(steps[0].model).toBe(HAIKU)
})

test('/relais-r3 records a pass only when it passed and the person agreed', async ($: any, on: any) => {
  const engine = await routedSession($, on, {
    store: { envelope_consent: { answer: 'Allow session routing', at: 'x', session: 's' } },
  })
  const base = engine.script.runResult
  engine.script.runResult = (argv: string[], init: any) =>
    argv[1] === 'router' && argv.includes('--eval')
      ? { exitCode: 0, stdout: JSON.stringify({ id: 'r3-1', passed: true, tier_accuracy_lower: 0.8 }), stderr: '' }
      : base(argv, init)
  const answers = ['/repo/labels.jsonl', 'Record this pass']
  engine.script.ask = () => answers.shift()
  const presentation = { layout: 'main', columns: 120 }
  const reply = await $.command.run({ command: 'relais-r3', args: '', origin: { kind: 'composer' }, presentation })
  expect(reply.text).toContain('r3-1 recorded')
  const runs = engine.calls.run.map(c => c.argv.join(' '))
  expect(runs).toContain('relais router r3 --eval /repo/labels.jsonl --json')
  expect(runs).toContain('relais router r3 --record --id r3-1 --passed --source plugin-ask')
  expect(engine.calls.asked[1].question).toContain('tier_accuracy_lower: 0.8')
  // The plugin's record names the pass router-state reports: routing is on.
  const { steps } = await personTurn($, engine, 'rename foo to bar, tests exist')
  expect(steps[0].model).toBe(HAIKU)
})

test('the model’s Bash calls of router-envelope and r3 --record, and writes to the plugin store, are refused', async ($: any, on: any) => {
  await routedSession($, on)
  const check = (tool: string, input: Record<string, unknown>) => $.tool.check({ tool, input, ...input })
  const refused = async (tool: string, input: Record<string, unknown>) => (await check(tool, input)).decision
  expect(await refused('Bash', { command: 'relais native router-envelope --by me --epsilon-max 0.1' })).toBe('deny')
  expect(await refused('Bash', { command: 'relais router r3 --record --id x --passed' })).toBe('deny')
  expect(await refused('Bash', { command: 'echo {} > ~/.claude/plugins/store/relais_x.json' })).toBe('deny')
  expect(await refused('Write', { file_path: '/Users/p/.claude/plugins/store/relais_x.json', content: '{}' })).toBe('deny')
  expect(await refused('Bash', { command: 'relais router r3 --eval labels.jsonl --json' })).not.toBe('deny')
})

test('a classifier answer that fails to parse, or no router-state, routes nothing', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  engine.fallback = () => ({ isAnswered: true, text: 'not json', usage: answered({}).usage })
  const { steps } = await personTurn($, engine, 'hello')
  expect(steps[0].model).toBe(SONNET)
})
