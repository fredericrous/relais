// The session router through the plugin's hooks: what reaches the engine's
// `turn.step` and `agent.spawn`, and what is recorded.

import { expect, test } from 'claude-code/testing'
import { nativeCalls, settle, spawnLine, startQueued } from './support.ts'
import { CLASSIFIER_SYSTEM, createRouterMemory, newTask, outcomeOf, sha256Hex, unmatchedChecksOf, withUnmatchedChecks } from '../hooks/router.ts'
import { noteRelaisLaunch } from '../hooks/routing.ts'
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
  // A plain string: no block is marked for the prompt cache.
  expect(request.system).toBe(CLASSIFIER_SYSTEM)
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

test('the spawn’s decision record and its subagent task share the pre-allocated id', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'rename foo to bar, tests exist')
  engine.labels.set('look up', label({ kind: 'question', difficulty: 1 }))
  await $.agent.spawn({ prompt: 'look up where foo is defined', description: 'find foo', subagentType: 'Explore', model: 'opus' })
  await $.turn.complete({ agentId: 'agent-1', answer: 'done', durationMs: 1, isAborted: false, turnId: 's1', reason: 'answer' })
  await endSession($, engine)
  const decision = observed(engine, 'decision').find((d: any) => d.relation === 'subagent')
  const task = observed(engine, 'task').find((t: any) => t.agent_id === 'agent-1')
  expect(decision.task_id).toBe(task.task_id)
  expect(decision.task_id).toMatch(/^task-\d+-\d+$/)
  // A research spawn: the draw ran and nothing could be lowered.
  expect([decision.explored, decision.propensity]).toEqual([false, 1])
  expect(typeof decision.draw).toBe('number')
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

test('mode_effective follows router-state: on routes with no plugin store record and no toast', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  const { steps } = await personTurn($, engine, 'rename foo to bar, tests exist')
  expect(steps[0].model).toBe(HAIKU)
  await engine.clock.advance(100)
  await settle(engine)
  expect(engine.calls.statuses[engine.calls.statuses.length - 1]).toBe('relais · research (haiku-5-5)')
  expect(engine.calls.toasts.filter(t => t.includes('routing')).length).toBe(0)
  await endSession($, engine)
  expect(observed(engine, 'decision').every((d: any) => d.mode_effective === 'on')).toBe(true)
})

test('mode_effective: router-state in shadow (no envelope) routes nothing and says nothing', async ($: any, on: any) => {
  const engine = await routedSession($, on, { state: { mode: 'shadow', mode_reason: 'no envelope', envelope: null } })
  const { steps } = await personTurn($, engine, 'rename foo to bar, tests exist')
  expect(steps[0].model).toBe(SONNET)
  expect(engine.calls.toasts.length).toBe(0)
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
    'classification:',
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

// router-state as relais would serve it after each router-envelope call.
function followEnvelope(engine: any) {
  const base = engine.script.runResult
  engine.script.runResult = (argv: string[], init: any) => {
    if (argv[1] === 'native' && argv[2] === 'router-envelope') {
      const off = argv.includes('--off')
      engine.state.mode = off ? 'shadow' : 'on'
      engine.state.envelope = off ? null : { by: 'x', epsilon_max: 0.1, source: 'plugin-ask', granted_at: 'x' }
    }
    return base(argv, init)
  }
}

test('/relais-routing records the envelope on the yes, and routing follows', async ($: any, on: any) => {
  const engine = await routedSession($, on, { state: { mode: 'shadow', mode_reason: 'no envelope', envelope: null } })
  followEnvelope(engine)
  engine.script.ask = () => 'Not now'
  const presentation = { layout: 'main', columns: 120 }
  const declined = await $.command.run({ command: 'relais-routing', args: '', origin: { kind: 'composer' }, presentation })
  expect(declined.text).toContain('not granted')
  expect(nativeCalls(engine.calls, 'router-envelope').length).toBe(0)
  expect((await personTurn($, engine, 'rename foo to bar, tests exist')).steps[0].model).toBe(SONNET)
  engine.script.ask = () => 'Allow session routing'
  const granted = await $.command.run({ command: 'relais-routing', args: '', origin: { kind: 'composer' }, presentation })
  expect(granted.text).toContain('Mode now: on')
  const [call] = nativeCalls(engine.calls, 'router-envelope')
  expect(call.argv).toEqual([
    'relais', 'native', 'router-envelope',
    '--by', 'the person in Claude Code session session-1',
    '--epsilon-max', '0.1',
    '--source', 'plugin-ask',
  ])
  expect(engine.calls.asked[0].question).toContain('ε ≤ 0.1')
  expect(engine.calls.asked[0].question).toContain('/relais-routing off')
  const { steps } = await personTurn($, engine, 'rename foo to bar, tests exist')
  expect(steps[0].model).toBe(HAIKU)
})

test('/relais-routing off removes the envelope without asking, and routing stops', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  followEnvelope(engine)
  expect((await personTurn($, engine, 'rename foo to bar, tests exist')).steps[0].model).toBe(HAIKU)
  const presentation = { layout: 'main', columns: 120 }
  const reply = await $.command.run({ command: 'relais-routing', args: ' off ', origin: { kind: 'composer' }, presentation })
  expect(reply.text).toContain('Mode now: shadow')
  expect(engine.calls.asked.length).toBe(0)
  const [call] = nativeCalls(engine.calls, 'router-envelope')
  expect(call.argv).toEqual([
    'relais', 'native', 'router-envelope', '--off',
    '--by', 'the person in Claude Code session session-1',
    '--source', 'plugin-ask',
  ])
  expect((await personTurn($, engine, 'rename baz to qux, tests exist')).steps[0].model).toBe(SONNET)
})

test('the model’s Bash calls of router-envelope and relais install --claude are refused; the store is not guarded', async ($: any, on: any) => {
  await routedSession($, on)
  const check = (tool: string, input: Record<string, unknown>) => $.tool.check({ tool, input, ...input })
  const refused = async (tool: string, input: Record<string, unknown>) => (await check(tool, input)).decision
  expect(await refused('Bash', { command: 'relais native router-envelope --by me --epsilon-max 0.1' })).toBe('deny')
  expect(await refused('Bash', { command: 'relais native router-envelope --off --by me' })).toBe('deny')
  expect(await refused('Bash', { command: 'relais install --claude' })).toBe('deny')
  expect(await refused('Bash', { command: 'relais  install --write --claude --hooks' })).toBe('deny')
  expect(await refused('Bash', { command: 'cd /x && relais install --claude --write' })).toBe('deny')
  expect(await refused('Write', { file_path: '/Users/p/.config/relais/machine.toml', content: '' })).toBe('deny')
  expect(await refused('Bash', { command: 'relais native router-state --session s' })).not.toBe('deny')
  expect(await refused('Bash', { command: 'relais uninstall --claude' })).not.toBe('deny')
  expect(await refused('Write', { file_path: '/Users/p/.claude/plugins/store/relais_x.json', content: '{}' })).not.toBe('deny')
})

test('a revert is recorded with the hashed files, and the task carries its files hashed', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'rename foo to bar, tests exist')
  await toolCall($, engine, edit('/repo/src/lib.rs'), { result: 'ok', text: 'ok' })
  await toolCall($, engine, bash('git restore src/lib.rs'), { result: { stdout: '', stderr: '' }, text: '' })
  await toolCall($, engine, bash('git checkout main'), { result: { stdout: '', stderr: '' }, text: '' })
  await endSession($, engine)
  const reverts = observed(engine, 'reassess').filter((r: any) => r.event === 'revert')
  expect(reverts.length).toBe(1)
  expect(reverts[0].files).toEqual(['b1a35a68f14e6962'])
  expect(reverts[0].escalating).toBe(false)
  const [task] = observed(engine, 'task')
  expect(task.files).toEqual(['b1a35a68f14e6962'])
  expect(task.completed_at_end).toBe(task.outcome)
})

test('a classifier answer that fails to parse, or no router-state, routes nothing', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  engine.fallback = () => ({ isAnswered: true, text: 'not json', usage: answered({}).usage })
  const { steps } = await personTurn($, engine, 'hello')
  expect(steps[0].model).toBe(SONNET)
})

// --- classifier calls kept for training (learning-loop Phase 5) -------------------------

test('each classifier call is a classification record: the prompt sent, the parsed answer, the prompt version', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  engine.labels.set('rename foo', label({ explicit: { value: 'correct', quote: 'that was wrong' }, user_model: 'opus' }))
  await personTurn($, engine, 'rename foo to bar, tests exist')
  await endSession($, engine)
  const records = observed(engine, 'classification')
  expect(records.length).toBe(1)
  const [c] = records
  expect(c.purpose).toBe('prompt')
  expect(c.model).toBe(HAIKU)
  expect(c.prompt_version).toMatch(/^[0-9a-f]{64}$/)
  expect(c.prompt_version).toBe(sha256Hex(CLASSIFIER_SYSTEM))
  expect(c.input).toBe(String(engine.calls.completes[0].prompt))
  expect(c.input).toContain('rename foo to bar')
  expect(c.parse_ok).toBe(true)
  expect(c.latency_ms).toBeGreaterThanOrEqual(0)
  expect(c.answer).toEqual({
    relation: 'new_task',
    kind: 'edit',
    difficulty: 2,
    scope: 'local',
    uncertainty: 'low',
    verifiable: true,
    explicit: { value: 'correct', quote: 'that was wrong' },
    user_model: 'opus',
    user_model_for: null,
    confidence: 0.9,
  })
})

test('a reply that does not parse is a classification record with no answer; the input is at most 3 KB', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  engine.fallback = () => ({ isAnswered: true, text: 'not json', usage: answered({}).usage })
  await personTurn($, engine, 'é'.repeat(5000))
  await endSession($, engine)
  const [c] = observed(engine, 'classification')
  expect(c.parse_ok).toBe(false)
  expect(c.answer).toBeNull()
  expect(new TextEncoder().encode(c.input).length).toBeLessThanOrEqual(3072)
})

test('a subagent spawn classified is a classification record of purpose spawn', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await $.agent.spawn({ prompt: 'look up where foo is defined', description: 'find foo', subagentType: 'Explore' })
  await endSession($, engine)
  const spawns = observed(engine, 'classification').filter((r: any) => r.purpose === 'spawn')
  expect(spawns.length).toBe(1)
  expect(spawns[0].input).toContain('look up where foo is defined')
  expect(spawns[0].parse_ok).toBe(true)
})

// --- evidence for the router (learning-loop Phase 3) ----------------------------------

const SESSION_START_EPOCH = Date.parse('2026-10-06T12:00:00Z') / 1000
const succeeded = { exitCode: 0, stderr: '', isStdoutTruncated: false, isStderrTruncated: false }

// The world around the edited files: `/repo` is a git repository whose tree
// carries `stamp` (undefined: no gate-stamp answer at all). Returns the
// gate-stamp calls made.
function repositoryWithStamp(engine: any, stamp: unknown) {
  const base = engine.script.runResult
  const stamped: string[][] = []
  engine.script.runResult = (argv: string[], init: any) => {
    if (argv[0] === 'git' && argv[1] === 'rev-parse') return { ...succeeded, stdout: '/repo\n' }
    if (argv[1] === 'native' && argv[2] === 'gate-stamp') {
      stamped.push(argv)
      return stamp === undefined ? { ...succeeded, exitCode: 1, stdout: '', stderr: 'git failed' } : { ...succeeded, stdout: JSON.stringify(stamp) }
    }
    return base(argv, init)
  }
  return stamped
}

const treeStamp = (passed: number[], failed: number[]) => ({
  stamped: true,
  tree: '0123456789abcdef0123456789abcdef01234567',
  passed: passed.map(epoch => ({ gate: 'pre-push', epoch })),
  failed: failed.map(epoch => ({ gate: 'pre-push', epoch })),
})

test('a passing amont stamp inside the task window is a passing amont-gate check of kind gate', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  const stamped = repositoryWithStamp(engine, treeStamp([SESSION_START_EPOCH], []))
  await personTurn($, engine, 'rename foo to bar, tests exist')
  await toolCall($, engine, edit('/repo/src/a.rs'), { result: {}, text: 'ok' })
  await endSession($, engine)
  expect(stamped.map(argv => argv.slice(1))).toEqual([['native', 'gate-stamp', '--cwd', '/repo']])
  const [task] = observed(engine, 'task')
  expect(task.checks).toEqual([{ name: 'amont-gate@0123456', kind: 'gate', passed: true }])
  expect(task.outcome).toBe('completed_verified')
})

test('a failing amont stamp inside the window is a failed check; runs outside the window are ignored', async ($: any, on: any) => {
  const failing = await routedSession($, on)
  repositoryWithStamp(failing, treeStamp([], [SESSION_START_EPOCH]))
  await personTurn($, failing, 'rename foo to bar, tests exist')
  await toolCall($, failing, edit('/repo/src/a.rs'), { result: {}, text: 'ok' })
  await endSession($, failing)
  const [task] = observed(failing, 'task')
  expect(task.checks).toEqual([{ name: 'amont-gate@0123456', kind: 'gate', passed: false }])
  expect(task.outcome).toBe('unknown')
})

async function editedWithStamp($: any, on: any, stamp: unknown) {
  const engine = await routedSession($, on)
  repositoryWithStamp(engine, stamp)
  await personTurn($, engine, 'rename foo to bar, tests exist')
  await toolCall($, engine, edit('/repo/src/a.rs'), { result: {}, text: 'ok' })
  await endSession($, engine)
  return observed(engine, 'task')[0]
}

test('a stamp from before the task records no gate check', async ($: any, on: any) => {
  const task = await editedWithStamp($, on, treeStamp([SESSION_START_EPOCH - 3600], [SESSION_START_EPOCH - 60]))
  expect(task.checks).toEqual([])
  expect(task.outcome).toBe('unknown')
})

test('a repository with no stamp records no gate check', async ($: any, on: any) => {
  expect((await editedWithStamp($, on, { stamped: false })).checks).toEqual([])
})

test('a gate-stamp that fails leaves the task recorded without it', async ($: any, on: any) => {
  const task = await editedWithStamp($, on, undefined)
  expect(task.checks).toEqual([])
  expect(task.outcome).toBe('unknown')
})

test('a task that edited nothing never asks for a stamp', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  const stamped = repositoryWithStamp(engine, treeStamp([SESSION_START_EPOCH], []))
  await personTurn($, engine, 'where is foo defined')
  await endSession($, engine)
  expect(stamped).toEqual([])
  expect(observed(engine, 'task')[0].checks).toEqual([])
})

test('a lint run is recorded on the task with its kind', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'rename foo to bar, tests exist')
  await toolCall($, engine, bash('golangci-lint run ./...'), passingCheck)
  await toolCall($, engine, bash('go vet ./...'), failingCheck)
  await endSession($, engine)
  expect(observed(engine, 'task')[0].checks).toEqual([
    { name: 'golangci-lint run ./...', kind: 'lint', passed: true },
    { name: 'go vet ./...', kind: 'lint', passed: false },
  ])
})

test('a subagent that ended before its parent is amended with the parent’s outcome', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'rename foo to bar, tests exist')
  await $.agent.spawn({ prompt: 'rename in the module', description: 'rename foo', subagentType: 'general-purpose' })
  await $.turn.complete({ agentId: 'agent-1', answer: 'done', durationMs: 1, isAborted: false, turnId: 'a1', reason: 'answer' })
  await settle(engine)
  await toolCall($, engine, edit('/repo/src/a.rs'), { result: {}, text: 'ok' })
  await toolCall($, engine, bash('cargo test'), passingCheck)
  await endSession($, engine)
  const records = nativeCalls(engine.calls, 'router-observe').flatMap((c: any) => c.payload.records)
  const sub = records.find((r: any) => r.kind === 'task' && r.agent_id === 'agent-1')
  expect(sub.outcome).toBe('answered')
  const outcomes = records.filter((r: any) => r.kind === 'task_outcome')
  expect(outcomes.map((r: any) => [r.task_id, r.agent_id, r.outcome])).toEqual([[sub.task_id, 'agent-1', 'completed_verified']])
  expect(records.indexOf(outcomes[0])).toBeGreaterThan(records.indexOf(sub))
})

test('a question task with no edit is recorded answered', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'what does this function do?')
  await toolCall($, engine, bash('grep -rn foo src'), { result: {}, text: 'ok' })
  await endSession($, engine)
  expect(observed(engine, 'task').map((r: any) => r.outcome)).toEqual(['answered'])
})

test('a sed -i keeps a task with no recorded edit unknown', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'what does this function do?')
  await toolCall($, engine, bash("sed -i 's/a/b/' src/a.rs"), { result: {}, text: 'ok' })
  await endSession($, engine)
  expect(observed(engine, 'task').map((r: any) => r.outcome)).toEqual(['unknown'])
})

test('a subagent the router did not create keeps the parent task unknown', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'what does this function do?')
  await $.agent.spawn({ prompt: 'look around', description: 'look', subagentType: 'pinned-agent' })
  await endSession($, engine)
  expect(observed(engine, 'task').find((r: any) => r.agent_id === null).outcome).toBe('unknown')
})

test('a relais run or replay launched from the task keeps it unknown', () => {
  const store: any = { router: createRouterMemory() }
  store.router.task = newTask('t', null, 'what is this?', { kind: 'question', difficulty: 1, scope: 'local', uncertainty: 'low', verifiable: false, confidence: 0.9 } as any, 'research', 0)
  expect(outcomeOf(store.router.task)).toBe('answered')
  noteRelaisLaunch(store)
  expect(store.router.task.maybeWrote).toBe(true)
  expect(outcomeOf(store.router.task)).toBe('unknown')
})

test('the run tool launched in a routed session ends the task unknown', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'what does this function do?')
  const reply = await $.tool.call({ tool: 'mcp__relais__run', task: 'fix-it.json', cwd: '/repo' })
  expect(reply.result).toContain('Started relais run')
  await startQueued(engine)
  engine.stream.push('stdout', JSON.stringify({ relais: 'done', run: 'run-1', outcome: 'blocked' }) + '\n')
  await settle(engine)
  await engine.clock.advance(100)
  await personTurn($, engine, 'and this one?')
  await settle(engine)
  expect(observed(engine, 'task').map((r: any) => r.outcome)).toEqual(['unknown'])
})

test('a run call refused for its workspace leaves the task answered', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'what does this function do?')
  const reply = await $.tool.call({ tool: 'mcp__relais__run', task: 'fix-it.json', cwd: '/repo', workspace: 'everywhere' })
  expect(reply.deny).toContain('auto, fork, in-place, isolated')
  await endSession($, engine)
  expect(observed(engine, 'task').map((r: any) => r.outcome)).toEqual(['answered'])
})

test('an answered parent sends no task_outcome', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'rename foo to bar, tests exist')
  await $.agent.spawn({ prompt: 'rename in the module', description: 'rename foo', subagentType: 'general-purpose' })
  await endSession($, engine)
  expect(observed(engine, 'task').find((r: any) => r.agent_id === null).outcome).toBe('answered')
  expect(observed(engine, 'task_outcome')).toEqual([])
})

test('a router-state whose accepts lacks answered gets the v0.35 form: answered is sent as unknown, and no version is asked', async ($: any, on: any) => {
  const engine = await routedSession($, on, { state: { accepts: [] } })
  await personTurn($, engine, 'what does this function do?')
  await personTurn($, engine, 'and this one?')
  await endSession($, engine)
  const tasks = observed(engine, 'task')
  expect(tasks.length).toBeGreaterThan(0)
  expect(tasks.map((r: any) => r.outcome)).toEqual(tasks.map(() => 'unknown'))
  expect(tasks.every((r: any) => r.completed_at_end !== 'answered')).toBe(true)
  const asked = engine.calls.run.filter((c: any) => c.argv?.[1] === '--version')
  expect(asked.length).toBe(0)
})

test('a router-state from a relais that predates accepts (0.38.0 and older) gets the v0.35 form', async ($: any, on: any) => {
  const engine = await routedSession($, on, { state: { accepts: undefined } })
  await personTurn($, engine, 'what does this function do?')
  await toolCall($, engine, bash('make check'), { result: {}, text: 'ok' })
  await endSession($, engine)
  const tasks = observed(engine, 'task')
  expect(tasks.length).toBeGreaterThan(0)
  for (const record of tasks) {
    expect(record.outcome).not.toBe('answered')
    expect('parent_task_id' in record).toBe(false)
    expect('unmatched_checks' in record).toBe(false)
  }
})

test('a parent whose outcome is unknown sends no task_outcome', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'rename foo to bar, tests exist')
  await $.agent.spawn({ prompt: 'rename in the module', description: 'rename foo', subagentType: 'general-purpose' })
  await toolCall($, engine, bash("sed -i 's/foo/bar/' src/a.rs"), { result: {}, text: 'ok' })
  await endSession($, engine)
  expect(observed(engine, 'task').find((r: any) => r.agent_id === null).outcome).toBe('unknown')
  expect(observed(engine, 'task_outcome')).toEqual([])
})

test('a correction is recorded as a corrected task when it happens, not only when the task ends', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'fix the parser, tests exist')
  engine.labels.set('still wrong', label({ relation: 'correction', explicit: { value: 'correct', quote: 'still wrong' } }))
  await personTurn($, engine, 'still wrong')
  // The session dies here: no turn ends the task, no session.end. The next
  // prompt's flush is the last thing that reaches relais.
  await $.prompt.submit({ text: 'anything', wait: false, origin: { kind: 'composer' } })
  await settle(engine)
  expect(observed(engine, 'reassess').map((r: any) => r.event)).toEqual(['correction'])
  const tasks = observed(engine, 'task')
  expect(tasks.length).toBe(1)
  expect(tasks[0].outcome).toBe('corrected')
  expect(tasks[0].completed_at_end).toBe(null)
})

test('/relais-flag records the corrected task at once too', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'fix the parser, tests exist')
  await $.command.run({ command: 'relais-flag', args: '', origin: { kind: 'composer' }, presentation: { layout: 'main', columns: 120 } })
  await $.prompt.submit({ text: 'anything', wait: false, origin: { kind: 'composer' } })
  await settle(engine)
  expect(observed(engine, 'reassess').map((r: any) => r.event)).toEqual(['flag'])
  expect(observed(engine, 'task').map((t: any) => t.outcome)).toEqual(['corrected'])
})

test('every decision carries the capability table’s version', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'rename foo to bar, tests exist')
  await $.agent.spawn({ prompt: 'rename in the module', description: 'rename foo', subagentType: 'general-purpose' })
  await endSession($, engine)
  const decisions = observed(engine, 'decision')
  expect(decisions.length).toBe(2)
  expect(decisions.map((d: any) => d.table_version)).toEqual([1, 1])
})

test('a router-created subagent record carries its parent, a main task has none, and every record says the link was captured', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'rename foo to bar, tests exist')
  await $.agent.spawn({ prompt: 'rename in the module', description: 'rename foo', subagentType: 'general-purpose' })
  await endSession($, engine)
  const tasks = observed(engine, 'task')
  const main = tasks.find((r: any) => r.agent_id === null)
  const sub = tasks.find((r: any) => r.agent_id === 'agent-1')
  expect(main.parent_task_id).toBeNull()
  expect(sub.parent_task_id).toBe(main.task_id)
  expect(tasks.every((r: any) => r.parent_link_recorded === true)).toBe(true)
})

test('a spawn with no active parent task records a null parent', () => {
  const task = newTask('t', 'a', 'x', { kind: 'question' } as any, 'research', 0)
  expect(task.parentTaskId).toBeUndefined()
  expect(newTask('t', 'a', 'x', { kind: 'question' } as any, 'research', 0, 'p').parentTaskId).toBe('p')
})

test('unmatched build and test commands keep only the argv prefix a person would paste into checks', () => {
  const checks = [['cargo', 'test']]
  const cases: [string, string[]][] = [
    ['make check', ['make check']],
    ['cargo nextest run', ['cargo nextest']],
    ['npm run test', ['npm run test']],
    ['uv run pytest', ['uv run pytest']],
    ['pytest tests/x.py', ['pytest']],
    ['make -C /p check', ['make']],
    ['cargo +nightly build', ['cargo']],
    ['cargo +nightly test', ['cargo']],
    ['FOO=x make', ['make']],
    ['cargo test --all', []],
    ['ls -la', []],
    ['cd /p && make lint; just fmt', ['make lint', 'just fmt']],
  ]
  for (const [command, expected] of cases) expect(unmatchedChecksOf(command, checks)).toEqual(expected)
})

test('a flag from a subdirectory stores one git-root entry, never the subdirectory name', async ($: any, on: any) => {
  const engine = await routedSession($, on, { cwd: '/repo/packages/app' })
  const base = engine.script.runResult
  engine.script.runResult = (argv: string[], init: any) =>
    argv[0] === 'git' && argv[1] === 'rev-parse' ? { ...succeeded, stdout: '/repo\n' } : base(argv, init)
  await personTurn($, engine, 'what does this function do?')
  await toolCall($, engine, bash('make check'), { result: {}, text: 'ok' })
  await $.command.run({ command: 'relais-flag', args: '', origin: { kind: 'composer' }, presentation: { layout: 'main', columns: 120 } })
  await endSession($, engine)
  const entries = observed(engine, 'task').flatMap((record: any) => record.unmatched_checks)
  expect(entries.length).toBeGreaterThan(0)
  expect(new Set(entries)).toEqual(new Set(['repo: make check']))
})

test('a task keeps at most 5 distinct unmatched checks', () => {
  let task = newTask('t', null, 'x', { kind: 'edit' } as any, 'research', 0)
  task = withUnmatchedChecks(task, ['make a', 'make a', 'make b', 'make c', 'make d', 'make e', 'make f'])
  expect(task.unmatchedChecks).toEqual(['make a', 'make b', 'make c', 'make d', 'make e'])
  expect(withUnmatchedChecks(task, ['make g']).unmatchedChecks.length).toBe(5)
})

test('the task record names unmatched checks with the repository and nothing else', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  const base = engine.script.runResult
  engine.script.runResult = (argv: string[], init: any) =>
    argv[0] === 'git' && argv[1] === 'rev-parse' ? { ...succeeded, stdout: '/repo\n' } : base(argv, init)
  await personTurn($, engine, 'what does this function do?')
  await toolCall($, engine, bash('make -C /secret/path check --token abc'), { result: {}, text: 'ok' })
  await toolCall($, engine, bash('cargo nextest run'), { result: {}, text: 'ok' })
  await endSession($, engine)
  const [task] = observed(engine, 'task')
  expect(task.unmatched_checks).toEqual(['repo: make', 'repo: cargo nextest'])
})

test('a git root whose name has a space sends no unmatched entry', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  const base = engine.script.runResult
  engine.script.runResult = (argv: string[], init: any) =>
    argv[0] === 'git' && argv[1] === 'rev-parse' ? { ...succeeded, stdout: '/my repo\n' } : base(argv, init)
  await personTurn($, engine, 'what does this function do?')
  await toolCall($, engine, bash('make check'), { result: {}, text: 'ok' })
  await endSession($, engine)
  const tasks = observed(engine, 'task')
  expect(tasks.length).toBeGreaterThan(0)
  for (const record of tasks) expect(record.unmatched_checks).toEqual([])
})

test('a 41-byte script name ends the prefix', () => {
  const longest = 'a'.repeat(40)
  expect(unmatchedChecksOf(`npm run ${longest}`, [])).toEqual([`npm run ${longest}`])
  expect(unmatchedChecksOf(`make ${'a'.repeat(41)}`, [])).toEqual(['make'])
  expect(unmatchedChecksOf(`npm run ${'a'.repeat(41)}`, [])).toEqual(['npm run'])
})

test('a failing rev-parse from a subdirectory stores no unmatched entry', async ($: any, on: any) => {
  const engine = await routedSession($, on, { cwd: '/repo/packages/app' })
  const base = engine.script.runResult
  engine.script.runResult = (argv: string[], init: any) =>
    argv[0] === 'git' && argv[1] === 'rev-parse' ? { ...succeeded, exitCode: 128, stdout: '', stderr: 'fatal' } : base(argv, init)
  await personTurn($, engine, 'what does this function do?')
  await toolCall($, engine, bash('make check'), { result: {}, text: 'ok' })
  await endSession($, engine)
  const tasks = observed(engine, 'task')
  expect(tasks.length).toBeGreaterThan(0)
  for (const record of tasks) expect(record.unmatched_checks).toEqual([])
})

test('a router-state whose accepts lacks answered receives none of the parent or unmatched fields', async ($: any, on: any) => {
  const engine = await routedSession($, on, { state: { accepts: [] } })
  await personTurn($, engine, 'what does this function do?')
  await toolCall($, engine, bash('make check'), { result: {}, text: 'ok' })
  await endSession($, engine)
  const tasks = observed(engine, 'task')
  expect(tasks.length).toBeGreaterThan(0)
  for (const record of tasks) {
    expect('parent_task_id' in record).toBe(false)
    expect('parent_link_recorded' in record).toBe(false)
    expect('unmatched_checks' in record).toBe(false)
  }
})
