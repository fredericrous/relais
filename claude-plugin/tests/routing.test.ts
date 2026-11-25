// The session router through the plugin's hooks: what reaches the engine's
// `turn.step` and `agent.spawn`, and what is recorded.

import { expect, test } from 'claude-code/testing'
import { nativeCalls, settle, spawnLine, startQueued } from './support.ts'
import { CLASSIFIER_SYSTEM, createRouterMemory, newTask, outcomeOf, scopedFileHash, sha256Hex, unmatchedChecksOf, withUnmatchedChecks } from '../hooks/router.ts'
import { hashFiles, noteRelaisLaunch, resolveRoot, settledWithin } from '../hooks/routing.ts'
import {
  answered,
  bash,
  edit,
  endSession,
  failingCheck,
  isRootProbe,
  label,
  nextStep,
  observed,
  passingCheck,
  personTurn,
  repositoriesAt,
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

// A pinned spawn of `rev` in a session whose model is `sessionModel`: the
// row the router records, and the call the engine received.
async function pinRow(
  $: any,
  on: any,
  setup: { pins: Record<string, string>; pin_sources?: Record<string, string>; sessionModel?: string; callModel?: string; state?: Record<string, unknown>; script?: Record<string, unknown> },
) {
  const state: Record<string, unknown> = { ...(setup.state ?? {}), pins: setup.pins }
  if (setup.pin_sources) state.pin_sources = setup.pin_sources
  const engine = await routedSession($, on, { state })
  await personTurn($, engine, 'rename foo to bar, tests exist')
  engine.script.sessionModel = setup.sessionModel
  Object.assign(engine.script, setup.script ?? {})
  const spawnedBefore = engine.calls.spawned.length
  const call = { prompt: 'review it', description: 'review', subagentType: 'rev', ...(setup.callModel ? { model: setup.callModel } : {}) }
  const spawning = $.agent.spawn(call)
  await engine.clock.advance(5000)
  await spawning
  await endSession($, engine)
  const row = observed(engine, 'decision').find((d: any) => d.relation === 'subagent')
  return { row, spawned: engine.calls.spawned.slice(spawnedBefore) }
}

const PREFERRED = { pins: { rev: 'opus' }, pin_sources: { rev: 'preferred' } }
const PIN_ROW_CASES: [string, Parameters<typeof pinRow>[2], string, string][] = [
  ['a preference with no call model records the session’s model', { ...PREFERRED, sessionModel: SONNET }, SONNET, 'implementation'],
  ['a call model is recorded over the preference', { ...PREFERRED, sessionModel: SONNET, callModel: 'opus' }, OPUS, 'escalation'],
  ['an own model is recorded when the call names none', { pins: { rev: 'sonnet' }, pin_sources: { rev: 'own' }, sessionModel: OPUS }, SONNET, 'implementation'],
  ['a machine pin records the session’s model', { pins: { rev: 'inherit' }, pin_sources: { rev: 'machine' }, sessionModel: HAIKU }, HAIKU, 'research'],
  ['a mistyped preference never reaches the row while the session’s model is known', { pins: { rev: 'opsu' }, pin_sources: { rev: 'preferred' }, sessionModel: HAIKU }, HAIKU, 'research'],
  ['an own model that does not resolve is kept as written', { pins: { rev: 'opsu' }, pin_sources: { rev: 'own' }, sessionModel: OPUS }, 'opsu', 'implementation'],
  ['a relais that serves no pin sources gets the value recorded before', { pins: { rev: 'opus' }, sessionModel: SONNET }, 'opus', 'escalation'],
  ['a pin source the plugin does not know is dropped', { pins: { rev: 'opus' }, pin_sources: { rev: 'someday' }, sessionModel: SONNET }, 'opus', 'escalation'],
  ['with no session model and no call model the pin’s string is kept', { ...PREFERRED, sessionModel: undefined }, 'opus', 'escalation'],
  ['a session model with a context suffix is recorded without it', { ...PREFERRED, sessionModel: `${SONNET}[1m]` }, SONNET, 'implementation'],
  ['a call model that does not resolve falls through to the source’s rule', { ...PREFERRED, sessionModel: HAIKU, callModel: 'opsu' }, HAIKU, 'research'],
]

for (const [name, setup, model, tier] of PIN_ROW_CASES) {
  test(`pin row: ${name}`, async ($: any, on: any) => {
    const { row, spawned } = await pinRow($, on, setup)
    expect([row.reason, row.applied, row.model, row.tier]).toEqual(['pin', false, model, tier])
    expect(spawned.length).toBe(1)
    expect(spawned[0].model).toBe(setup.callModel)
  })
}

const SONNET_5_TIERS = {
  research: { model: HAIKU, effort: null },
  implementation: { model: 'claude-sonnet-5', effort: null },
  escalation: { model: OPUS, effort: null },
}

test('a pin row resolves an alias by the default ids, not a tier’s configured model', async ($: any, on: any) => {
  const own = await pinRow($, on, { pins: { rev: 'sonnet' }, pin_sources: { rev: 'own' }, sessionModel: OPUS, state: { tiers: SONNET_5_TIERS } })
  expect(own.row.model).toBe(SONNET)
})

test('a pin row keeps a full id the call names', async ($: any, on: any) => {
  const called = await pinRow($, on, { ...PREFERRED, sessionModel: OPUS, callModel: 'claude-sonnet-5', state: { tiers: SONNET_5_TIERS } })
  expect(called.row.model).toBe('claude-sonnet-5')
})

for (const [name, script] of [['fails', { sessionModelThrows: true }], ['never answers', { sessionModelNever: true }]] as const) {
  test(`a session model that ${name} leaves the spawn as it was and the row on the pin`, async ($: any, on: any) => {
    const { row, spawned } = await pinRow($, on, { ...PREFERRED, sessionModel: SONNET, script })
    expect(spawned.length).toBe(1)
    expect(spawned[0].model).toBeUndefined()
    expect([row.reason, row.model]).toEqual(['pin', 'opus'])
  })
}

test('with no time left, a session model already in is taken and a pending one is not', async () => {
  // The spawn may return after the hook's budget is spent: the read asked
  // for before it still counts once settled.
  const fx: any = {}
  expect(await settledWithin(fx, Promise.resolve(HAIKU), 0)).toBe(HAIKU)
  expect(await settledWithin(fx, Promise.resolve(HAIKU), -50)).toBe(HAIKU)
  expect(typeof (await settledWithin(fx, new Promise<string>(() => {}), 0))).toBe('symbol')
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

const LIB_HASH = scopedFileHash('/repo/.git', 'src/lib.rs')

test('a revert is recorded with the hashed files, and the task carries its files hashed', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'rename foo to bar, tests exist')
  await toolCall($, engine, edit('/repo/src/lib.rs'), { result: 'ok', text: 'ok' })
  await toolCall($, engine, bash('git restore src/lib.rs'), { result: { stdout: '', stderr: '' }, text: '' })
  await toolCall($, engine, bash('git checkout main'), { result: { stdout: '', stderr: '' }, text: '' })
  await endSession($, engine)
  const reverts = observed(engine, 'reassess').filter((r: any) => r.event === 'revert')
  expect(reverts.length).toBe(1)
  expect(reverts[0].files).toEqual([LIB_HASH])
  expect(reverts[0].escalating).toBe(false)
  const [task] = observed(engine, 'task')
  expect(task.files).toEqual([LIB_HASH])
  expect(task.completed_at_end).toBe(task.outcome)
})

const UNSET = ['env', '-u', 'GIT_DIR', '-u', 'GIT_WORK_TREE', '-u', 'GIT_COMMON_DIR']

// `git revert` through a fake git: `--git-common-dir` answers `common`, `show` answers `files`
// (NUL-separated), or both fail. Returns the git calls made.
function gitWithRevertedFiles(engine: any, common: string, files: string[] | undefined) {
  const base = engine.script.runResult
  const calls: { argv: string[]; init: any }[] = []
  engine.script.runResult = (argv: string[], init: any) => {
    if (!argv.includes('--no-pager')) return base(argv, init)
    calls.push({ argv, init })
    if (files === undefined) return { ...succeeded, exitCode: 128, stdout: '', stderr: 'fatal' }
    if (argv.includes('--git-common-dir')) return { ...succeeded, stdout: `${common}\n` }
    if (argv.includes('show')) return { ...succeeded, stdout: files.map(file => `${file}\0`).join('') }
    return base(argv, init)
  }
  return calls
}

const hashes = (files: string[]) => files.map(file => scopedFileHash('/repo/.git', file.replace('/repo/', ''))).sort()
const revertRows = (engine: any) => observed(engine, 'reassess').filter((r: any) => r.event === 'revert')

async function taskEditing($: any, on: any) {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'rename foo to bar, tests exist')
  await toolCall($, engine, edit('/repo/src/lib.rs'), { result: 'ok', text: 'ok' })
  return engine
}

test('a revert resolves paths against the command directory, to the hash the edit has', async ($: any, on: any) => {
  const engine = await taskEditing($, on)
  await toolCall($, engine, bash('cd /repo/src && git restore lib.rs'), { result: {}, text: '' })
  await toolCall($, engine, bash('cd /repo && git restore $P docs/new.md .'), { result: {}, text: '' })
  await endSession($, engine)
  const [first, second] = revertRows(engine)
  expect(first.files).toEqual(hashes(['/repo/src/lib.rs']))
  expect(second.files).toEqual(hashes(['/repo/docs/new.md', '/repo/src/lib.rs']))
  expect(first.files).toEqual(observed(engine, 'task')[0].files)
})

test('git revert reads the commit files in the command directory, off the hook, and records them', async ($: any, on: any) => {
  const engine = await taskEditing($, on)
  const calls = gitWithRevertedFiles(engine, '/repo/.git', ['src/lib.rs', 'docs/a.md'])
  await toolCall($, engine, bash('cd /repo/sub && git revert --no-edit abc123'), { result: {}, text: '' })
  await settle(engine)
  expect(calls.map(c => c.argv)).toEqual([
    [...UNSET, 'git', '--no-pager', 'rev-parse', '--path-format=absolute', '--git-common-dir'],
    [...UNSET, 'git', '--no-pager', 'show', '-z', '--name-only', '--no-renames', '--no-relative', '--format=', '--end-of-options', 'abc123'],
  ])
  for (const call of calls) expect(call.init).toEqual({ cwd: '/repo/sub', env: { GIT_TERMINAL_PROMPT: '0', LC_ALL: 'C' }, timeoutMs: 1000 })
  await endSession($, engine)
  expect(revertRows(engine).map((r: any) => r.files)).toEqual([hashes(['/repo/src/lib.rs', '/repo/docs/a.md'])])
})

test('git revert -m 1 reads the first-parent diff; another mainline and a HEAD~1 read nothing', async ($: any, on: any) => {
  const engine = await taskEditing($, on)
  const calls = gitWithRevertedFiles(engine, '/repo/.git', ['src/lib.rs'])
  await toolCall($, engine, bash('git revert -m 1 abc'), { result: {}, text: '' })
  await settle(engine)
  expect(calls[1].argv).toContain('--diff-merges=first-parent')
  const lookups = () => calls.filter(c => c.argv.includes('show') || c.argv.includes('--git-common-dir')).length
  const before = lookups()
  await toolCall($, engine, bash('git revert -m 2 abc'), { result: {}, text: '' })
  await toolCall($, engine, bash('git revert HEAD~1'), { result: {}, text: '' })
  await toolCall($, engine, bash('git revert --abort'), { result: {}, text: '' })
  await endSession($, engine)
  expect(lookups()).toBe(before)
  expect(revertRows(engine).map((r: any) => r.files)).toEqual([hashes(['/repo/src/lib.rs']), [], []])
})

test('a failing git still queues the revert row, with no files', async ($: any, on: any) => {
  const engine = await taskEditing($, on)
  gitWithRevertedFiles(engine, '/repo/.git', undefined)
  await toolCall($, engine, bash('git revert abc'), { result: {}, text: '' })
  await endSession($, engine)
  expect(revertRows(engine).map((r: any) => r.files)).toEqual([[]])
})

test('a lookup that resolves after the session end starts is in the final batch, under its session', async ($: any, on: any) => {
  const engine = await taskEditing($, on)
  gitWithRevertedFiles(engine, '/repo/.git', ['src/lib.rs'])
  let release: () => void = () => {}
  engine.script.runGate = (argv: string[]) => (argv.includes('show') ? new Promise<void>(resolve => (release = resolve)) : undefined)
  await toolCall($, engine, bash('git revert abc'), { result: {}, text: '' })
  await settle(engine)
  const ending = $.session.end({ reason: 'other', sessionId: 'session-1', resume: { command: 'claude --resume session-1' } })
  await Promise.resolve()
  release()
  await ending
  await settle(engine)
  const batches = nativeCalls(engine.calls, 'router-observe')
  const last = batches[batches.length - 1]
  expect(last.payload.records.filter((r: any) => r.event === 'revert').map((r: any) => r.files)).toEqual([hashes(['/repo/src/lib.rs'])])
  expect(last.payload.records.some((r: any) => r.kind === 'task')).toBe(true)
})

test('a lookup that never resolves delays the session end by the gate timeout at most, and queues no files', async ($: any, on: any) => {
  const engine = await taskEditing($, on)
  gitWithRevertedFiles(engine, '/repo/.git', ['src/lib.rs'])
  engine.script.runGate = (argv: string[]) => (argv.includes('show') ? new Promise<void>(() => {}) : undefined)
  await toolCall($, engine, bash('git revert abc'), { result: {}, text: '' })
  await settle(engine)
  let ended = false
  const ending = $.session
    .end({ reason: 'other', sessionId: 'session-1', resume: { command: 'claude --resume session-1' } })
    .then(() => (ended = true))
  await settle(engine)
  expect(ended).toBe(false)
  await engine.clock.advance(1000)
  await ending
  await settle(engine)
  expect(revertRows(engine).map((r: any) => r.files)).toEqual([[]])
  expect(observed(engine, 'task').length).toBe(1)
})

test('a lookup whose spawn throws queues the row with no files, and the session end still sends the task', async ($: any, on: any) => {
  const engine = await taskEditing($, on)
  const base = engine.script.runResult
  engine.script.runResult = (argv: string[], init: any) => {
    if (argv.includes('--no-pager') && argv.includes('show')) throw new Error('spawn refused')
    return base(argv, init)
  }
  await toolCall($, engine, bash('git revert abc'), { result: {}, text: '' })
  await endSession($, engine)
  expect(revertRows(engine).map((r: any) => r.files)).toEqual([[]])
  expect(observed(engine, 'task').length).toBe(1)
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

test('a passing gate stamp inside the task window is a passing greengate-gate check of kind gate', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  const stamped = repositoryWithStamp(engine, treeStamp([SESSION_START_EPOCH], []))
  await personTurn($, engine, 'rename foo to bar, tests exist')
  await toolCall($, engine, edit('/repo/src/a.rs'), { result: {}, text: 'ok' })
  await endSession($, engine)
  expect(stamped.map(argv => argv.slice(1))).toEqual([['native', 'gate-stamp', '--cwd', '/repo']])
  const [task] = observed(engine, 'task')
  expect(task.checks).toEqual([{ name: 'greengate-gate@0123456', kind: 'gate', passed: true }])
  expect(task.outcome).toBe('completed_verified')
})

test('a failing amont stamp inside the window is a failed check; runs outside the window are ignored', async ($: any, on: any) => {
  const failing = await routedSession($, on)
  repositoryWithStamp(failing, treeStamp([], [SESSION_START_EPOCH]))
  await personTurn($, failing, 'rename foo to bar, tests exist')
  await toolCall($, failing, edit('/repo/src/a.rs'), { result: {}, text: 'ok' })
  await endSession($, failing)
  const [task] = observed(failing, 'task')
  expect(task.checks).toEqual([{ name: 'greengate-gate@0123456', kind: 'gate', passed: false }])
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

test('a router-state whose accepts lacks outcome_uncorrected (0.77.0) gets task records without it', async ($: any, on: any) => {
  const engine = await routedSession($, on, { state: { accepts: ['answered', 'parent_task_id', 'unmatched_checks'] } })
  await personTurn($, engine, 'fix the parser, tests exist')
  await toolCall($, engine, edit('/repo/src/x.rs'), { result: {}, text: 'ok' })
  engine.labels.set('still wrong', label({ relation: 'correction', explicit: { value: 'correct', quote: 'still wrong' } }))
  await personTurn($, engine, 'still wrong')
  await toolCall($, engine, edit('/repo/src/z.rs'), { result: {}, text: 'ok' })
  await endSession($, engine)
  const tasks = observed(engine, 'task')
  expect(tasks.length).toBeGreaterThan(0)
  expect(tasks.every((r: any) => !('outcome_uncorrected' in r))).toBe(true)
  expect(observed(engine, 'reassess').filter((r: any) => r.event === 'correction' && r.files).length).toBe(1)
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

test('a router-state that accepts answered alone gets the v0.35 form: the three fields go together', async ($: any, on: any) => {
  const engine = await routedSession($, on, { state: { accepts: ['answered'] } })
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

// File hashes scoped to one repository.

const ROOT_ARGV = [
  'env', '-u', 'GIT_DIR', '-u', 'GIT_WORK_TREE', '-u', 'GIT_COMMON_DIR',
  'git', 'rev-parse', '--path-format=absolute', '--git-common-dir', '--show-toplevel', '--show-prefix',
]
const NOT_A_REPOSITORY = { exitCode: 128, stdout: '', stderr: 'fatal: not a git repository (or any of the parent directories): .git\n' }
const stdoutOf = (stdout: string) => ({ exitCode: 0, stdout, stderr: '' })

// The effects `resolveRoot` and `hashFiles` take, with a clock the test moves.
function fakeFx(answer: (argv: string[], init: any) => unknown) {
  const timers: { at: number; callback: () => void; live: boolean }[] = []
  let now = 0
  const calls: { argv: string[]; init: any }[] = []
  const fx: any = {
    process: {
      run: async (argv: string[], init: any) => {
        calls.push({ argv, init })
        return answer(argv, init)
      },
    },
    clock: {
      now: async () => now,
      after: (ms: number, callback: () => void) => {
        const timer = { at: now + ms, callback, live: true }
        timers.push(timer)
        return { cancel: () => (timer.live = false) }
      },
    },
  }
  const flush = () => new Promise<void>(resolve => setTimeout(resolve, 0))
  const advance = async (ms: number) => {
    now += ms
    for (const timer of timers) {
      if (timer.live && timer.at <= now) {
        timer.live = false
        timer.callback()
      }
    }
    await flush()
  }
  const store: any = { router: createRouterMemory() }
  store.router.cwd = '/repo'
  return { fx, store, calls, advance, flush }
}

test('resolveRoot asks git once, with the scrubbed environment, and reads the prefix untrimmed', async () => {
  const world = fakeFx(() => stdoutOf('/r/.git\n/r\nsrc/\n'))
  const [first, second] = await Promise.all([resolveRoot(world.fx, world.store, '/r/src'), resolveRoot(world.fx, world.store, '/r/src')])
  expect(first).toEqual({ kind: 'root', root: { common: '/r/.git', toplevel: '/r', prefix: 'src/' } })
  expect(second).toBe(first)
  expect(world.calls.length).toBe(1)
  expect(world.calls[0].argv).toEqual(ROOT_ARGV)
  expect(world.calls[0].init).toEqual({ cwd: '/r/src', env: { GIT_TERMINAL_PROMPT: '0', LC_ALL: 'C' }, timeoutMs: 1000 })
  expect(await resolveRoot(world.fx, world.store, '/r/src')).toBe(first)
  expect(world.calls.length).toBe(1)
})

test('resolveRoot at the repository root has an empty prefix', async () => {
  const world = fakeFx(() => stdoutOf('/r/.git\n/r\n\n'))
  expect(await resolveRoot(world.fx, world.store, '/r')).toEqual({ kind: 'root', root: { common: '/r/.git', toplevel: '/r', prefix: '' } })
})

test('"not a git repository" is cached as null for 60 s, then asked again', async () => {
  const world = fakeFx(() => NOT_A_REPOSITORY)
  expect(await resolveRoot(world.fx, world.store, '/n')).toEqual({ kind: 'none' })
  expect(await resolveRoot(world.fx, world.store, '/n')).toEqual({ kind: 'none' })
  expect(world.calls.length).toBe(1)
  await world.advance(59_000)
  expect(await resolveRoot(world.fx, world.store, '/n')).toEqual({ kind: 'none' })
  expect(world.calls.length).toBe(1)
  await world.advance(1000)
  expect(await resolveRoot(world.fx, world.store, '/n')).toEqual({ kind: 'none' })
  expect(world.calls.length).toBe(2)
})

test('any other answer is unresolved and not cached: another exit, 129, a wrong shape, a throw, a timeout', async () => {
  const answers: unknown[] = [
    { exitCode: 128, stdout: '', stderr: 'fatal: detected dubious ownership in repository' },
    { exitCode: 128, stdout: '', stderr: "fatal: cannot change to '/gone': No such file or directory" },
    { exitCode: 129, stdout: '', stderr: "error: unknown option `path-format=absolute'\nnot a git repository" },
    stdoutOf('/r/.git\n/r\n'),
    stdoutOf('/r/.git\n/r\nsrc/\nextra\n'),
    stdoutOf(''),
  ]
  for (const answer of answers) {
    const world = fakeFx(() => answer)
    expect(await resolveRoot(world.fx, world.store, '/d')).toEqual({ kind: 'unresolved' })
    expect(await resolveRoot(world.fx, world.store, '/d')).toEqual({ kind: 'unresolved' })
    expect(world.calls.length).toBe(2)
  }
  const throwing = fakeFx(() => {
    throw new Error('spawn refused')
  })
  expect(await resolveRoot(throwing.fx, throwing.store, '/d')).toEqual({ kind: 'unresolved' })
  expect(await resolveRoot(throwing.fx, throwing.store, '/d')).toEqual({ kind: 'unresolved' })
  expect(throwing.calls.length).toBe(2)
  const hanging = fakeFx(() => new Promise(() => {}))
  const first = resolveRoot(hanging.fx, hanging.store, '/d')
  await hanging.flush()
  await hanging.advance(1000)
  expect(await first).toEqual({ kind: 'unresolved' })
  const second = resolveRoot(hanging.fx, hanging.store, '/d')
  await hanging.flush()
  await hanging.advance(1000)
  expect(await second).toEqual({ kind: 'unresolved' })
  expect(hanging.calls.length).toBe(2)
})

test('the root cache holds 512 directories and is cleared when the session cwd changes', async () => {
  const world = fakeFx(() => stdoutOf('/r/.git\n/r\n\n'))
  for (let index = 0; index < 513; index++) await resolveRoot(world.fx, world.store, `/r/d${index}`)
  expect(world.store.router.roots.size).toBe(512)
  await resolveRoot(world.fx, world.store, '/r/d0')
  expect(world.calls.length).toBe(513)
  await resolveRoot(world.fx, world.store, '/r/d512')
  expect(world.calls.length).toBe(514)
  world.store.router.cwd = '/elsewhere'
  await resolveRoot(world.fx, world.store, '/r/d0')
  expect(world.calls.length).toBe(515)
  expect(world.store.router.roots.size).toBe(1)
})

test('hashFiles: a path in no repository gives no hash; the same path under two common dirs gives two', async () => {
  const world = fakeFx((_argv, init) => {
    if (init.cwd.startsWith('/a/')) return stdoutOf(`/a/.git\n/a\n${init.cwd.slice(3)}/\n`)
    if (init.cwd.startsWith('/b/')) return stdoutOf(`/b/.git\n/b\n${init.cwd.slice(3)}/\n`)
    return NOT_A_REPOSITORY
  })
  const within = await hashFiles(world.fx, world.store, ['/a/src/lib.rs', '/home/p/.claude/memory/a.md'])
  expect(within).toEqual([scopedFileHash('/a/.git', 'src/lib.rs')])
  const both = await hashFiles(world.fx, world.store, ['/b/src/lib.rs', '/a/src/lib.rs', '/a/src/lib.rs'])
  expect(both.length).toBe(2)
  expect(both).toEqual([scopedFileHash('/a/.git', 'src/lib.rs'), scopedFileHash('/b/.git', 'src/lib.rs')].sort())
  expect(await hashFiles(world.fx, world.store, ['/nowhere/x.md'])).toEqual([])
})

test('hashFiles: a worktree and its main checkout sharing a common dir hash one file once', async () => {
  const world = fakeFx((_argv, init) => {
    const prefix = init.cwd.replace(/^\/(main|wt)/, '').replace(/^\//, '')
    return stdoutOf(`/main/.git\n${init.cwd.startsWith('/wt') ? '/wt' : '/main'}\n${prefix === '' ? '' : `${prefix}/`}\n`)
  })
  expect(await hashFiles(world.fx, world.store, ['/main/src/lib.rs', '/wt/src/lib.rs'])).toEqual([scopedFileHash('/main/.git', 'src/lib.rs')])
})

test('hashFiles: a timeout returns the hashes of the directories that answered', async () => {
  const world = fakeFx((_argv, init) => (init.cwd === '/a/src' ? stdoutOf('/a/.git\n/a\nsrc/\n') : new Promise(() => {})))
  const pending = hashFiles(world.fx, world.store, ['/a/src/lib.rs', '/slow/x.rs'])
  await world.flush()
  await world.advance(1000)
  expect(await pending).toEqual([scopedFileHash('/a/.git', 'src/lib.rs')])
})

test('an edit outside any repository gives no hash on the task; one inside and one outside give exactly one', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'rename foo to bar, tests exist')
  await toolCall($, engine, edit('/repo/src/lib.rs'), { result: 'ok', text: 'ok' })
  await toolCall($, engine, edit('/Users/p/.claude/memory/a.md'), { result: 'ok', text: 'ok' })
  await endSession($, engine)
  const [task] = observed(engine, 'task')
  expect(task.files).toEqual([LIB_HASH])
  const probes = engine.calls.run.filter(c => isRootProbe(c.argv))
  expect(probes.map(c => c.argv)).toEqual([ROOT_ARGV, ROOT_ARGV])
  expect(probes.map(c => c.init.cwd)).toEqual(['/repo/src', '/Users/p/.claude/memory'])
})

test('the same relative path in two repositories hashes twice; a worktree shares its main checkout’s hash', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  repositoriesAt(engine, [{ path: '/repo' }, { path: '/other' }, { path: '/repo-wt-x', common: '/repo/.git' }])
  await personTurn($, engine, 'rename foo to bar, tests exist')
  await toolCall($, engine, edit('/repo/src/lib.rs'), { result: 'ok', text: 'ok' })
  await toolCall($, engine, edit('/other/src/lib.rs'), { result: 'ok', text: 'ok' })
  await toolCall($, engine, edit('/repo-wt-x/src/lib.rs'), { result: 'ok', text: 'ok' })
  await endSession($, engine)
  const [task] = observed(engine, 'task')
  expect(task.files).toEqual([LIB_HASH, scopedFileHash('/other/.git', 'src/lib.rs')].sort())
})

test('a not-a-repository answer is dropped when a later edit lands in that directory', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'rename foo to bar, tests exist')
  await toolCall($, engine, edit('/fresh/a.rs'), { result: 'ok', text: 'ok' })
  await settle(engine)
  repositoriesAt(engine, [{ path: '/fresh' }])
  await toolCall($, engine, edit('/fresh/b.rs'), { result: 'ok', text: 'ok' })
  await endSession($, engine)
  const [task] = observed(engine, 'task')
  expect(task.files).toEqual([scopedFileHash('/fresh/.git', 'a.rs'), scopedFileHash('/fresh/.git', 'b.rs')].sort())
})

test('a revert with paths only is queued before its task row, inside the final batch', async ($: any, on: any) => {
  const engine = await taskEditing($, on)
  await toolCall($, engine, bash('git restore src/lib.rs'), { result: {}, text: '' })
  await endSession($, engine)
  const batches = nativeCalls(engine.calls, 'router-observe')
  const last = batches[batches.length - 1].payload.records
  const revertAt = last.findIndex((r: any) => r.event === 'revert')
  const taskAt = last.findIndex((r: any) => r.kind === 'task')
  expect(revertAt).toBeGreaterThanOrEqual(0)
  expect(revertAt).toBeLessThan(taskAt)
  expect(last[revertAt].files).toEqual([LIB_HASH])
  expect(last[taskAt].files).toEqual([LIB_HASH])
})

test('a git revert lookup hashes from the common dir and resolves no directory by the file’s name', async ($: any, on: any) => {
  const engine = await taskEditing($, on)
  gitWithRevertedFiles(engine, '/repo/.git', ['removed-dir/gone.rs', 'src/lib.rs'])
  await toolCall($, engine, bash('cd /repo/sub && git revert abc'), { result: {}, text: '' })
  await endSession($, engine)
  expect(revertRows(engine).map((r: any) => r.files)).toEqual([hashes(['/repo/removed-dir/gone.rs', '/repo/src/lib.rs'])])
  const probed = engine.calls.run.filter(c => isRootProbe(c.argv)).map(c => c.init.cwd)
  expect(probed).toEqual(['/repo/src'])
})

const correctionRows = (engine: any) => observed(engine, 'reassess').filter((r: any) => r.event === 'correction')
const hashOf = (file: string) => scopedFileHash('/repo/.git', file.replace('/repo/', ''))

test('a correction row is sent again at the end with the files edited since it', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'fix the parser, tests exist')
  await toolCall($, engine, edit('/repo/src/x.rs'), { result: {}, text: 'ok' })
  engine.labels.set('still wrong', label({ relation: 'correction', explicit: { value: 'correct', quote: 'still wrong' } }))
  await personTurn($, engine, 'still wrong')
  await toolCall($, engine, edit('/repo/src/x.rs'), { result: {}, text: 'ok' })
  await toolCall($, engine, edit('/repo/src/z.rs'), { result: {}, text: 'ok' })
  await endSession($, engine)
  const [early, late] = correctionRows(engine)
  expect(correctionRows(engine).length).toBe(2)
  expect(early.files).toBe(undefined)
  expect(late.at).toBe(early.at)
  expect(late.files).toEqual([hashOf('/repo/src/x.rs'), hashOf('/repo/src/z.rs')].sort())
  const [snapshot, ended] = observed(engine, 'task')
  expect(snapshot.outcome_uncorrected).toBe(null)
  expect(ended.outcome_uncorrected).not.toBe(null)
})

test('two corrections: the first row holds both windows, the second its own', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'fix the parser, tests exist')
  engine.labels.set('wrong one', label({ relation: 'correction', explicit: { value: 'correct', quote: 'wrong one' } }))
  engine.labels.set('wrong two', label({ relation: 'correction', explicit: { value: 'correct', quote: 'wrong two' } }))
  await personTurn($, engine, 'wrong one')
  await toolCall($, engine, edit('/repo/src/x.rs'), { result: {}, text: 'ok' })
  await personTurn($, engine, 'wrong two')
  await toolCall($, engine, edit('/repo/src/z.rs'), { result: {}, text: 'ok' })
  await endSession($, engine)
  // The test clock is fixed, so both corrections share an `at`; the end sends the entries in order, last.
  const [first, second] = correctionRows(engine).slice(-2)
  expect(first.files).toEqual([hashOf('/repo/src/x.rs'), hashOf('/repo/src/z.rs')].sort())
  expect(second.files).toEqual([hashOf('/repo/src/z.rs')])
})

test('no correction means no correction row; a flag leaves outcome_uncorrected null', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'fix the parser, tests exist')
  await toolCall($, engine, edit('/repo/src/x.rs'), { result: {}, text: 'ok' })
  await toolCall($, engine, bash('cargo test'), passingCheck)
  await $.command.run({ command: 'relais-flag', args: '', origin: { kind: 'composer' }, presentation: { layout: 'main', columns: 120 } })
  await endSession($, engine)
  expect(correctionRows(engine)).toEqual([])
  expect(observed(engine, 'task').every((t: any) => t.outcome_uncorrected === null)).toBe(true)
})

test('a subagent edit during a correction window is not in the main task’s corrections', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'rename foo to bar, tests exist')
  engine.labels.set('still wrong', label({ relation: 'correction', explicit: { value: 'correct', quote: 'still wrong' } }))
  await personTurn($, engine, 'still wrong')
  await $.agent.spawn({ prompt: 'rename in the module', description: 'rename foo', subagentType: 'general-purpose' })
  await toolCall($, engine, edit('/repo/src/sub.rs', 'agent-1'), { result: {}, text: 'ok' })
  await endSession($, engine)
  expect(correctionRows(engine).filter((r: any) => r.files)).toEqual([])
})

// --- a session a relay opens after /clear ---------------------------------------

async function clearInto($: any, engine: any, pins: Record<string, string> = {}) {
  await $.session.end({ reason: 'clear', sessionId: 'session-1' })
  await settle(engine)
  engine.script.sessionId = 'session-2'
  engine.state.pins = { ...(engine.state.pins as object), ...pins }
}

async function startAfterClear($: any, engine: any) {
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await $.classic.SessionStart({ source: 'clear' })
  await settle(engine)
}

const decisionSessions = (engine: any) =>
  nativeCalls(engine.calls, 'router-observe').flatMap((c: any) =>
    (c.payload.records as any[]).filter(r => r.kind === 'decision').map(r => ({ session: c.payload.session, reason: r.reason, applied: r.applied })),
  )

const stateGate = (engine: any) => {
  let release!: () => void
  const gate = new Promise<void>(resolve => (release = resolve))
  engine.script.runGate = (argv: string[]) => (argv[2] === 'router-state' ? gate : undefined)
  return release
}

test('after a clear, a relayed opening prompt opens the task and a pinned launch is recorded under the new session', async ($: any, on: any) => {
  on('classic.SessionStart', () => ({}))
  const engine = await routedSession($, on)
  await clearInto($, engine, { 'relais-review': 'opus' })
  await startAfterClear($, engine)
  await $.prompt.submit({ text: 'resume the plan', wait: false, origin: { kind: 'plugin', name: 'relay' } })
  await step($, { turnId: 'turn-relayed', index: 0 })
  await $.agent.spawn({ prompt: 'review it', description: 'review', subagentType: 'relais-review', model: 'sonnet' })
  expect(engine.calls.spawned[0].model).toBe('sonnet')
  await endSession($, engine)
  expect(decisionSessions(engine).filter(d => d.reason === 'pin')).toEqual([{ session: 'session-2', reason: 'pin', applied: false }])
})

test('after a clear, a launch with no prompt waits for the pending router-state and sees its pins', async ($: any, on: any) => {
  on('classic.SessionStart', () => ({}))
  const engine = await routedSession($, on)
  await clearInto($, engine, { 'relais-review': 'opus' })
  const release = stateGate(engine)
  await startAfterClear($, engine)
  const completes = engine.calls.completes.length
  const launched = $.agent.spawn({ prompt: 'review it', description: 'review', subagentType: 'relais-review', model: 'sonnet' })
  await settle(engine)
  expect(engine.calls.spawned.length).toBe(0)
  release()
  await launched
  expect(engine.calls.spawned[0].model).toBe('sonnet')
  expect(engine.calls.completes.length).toBe(completes)
})

test('a launch waits for a slow router-state only up to the budget, then keeps its model with no row', async ($: any, on: any) => {
  on('classic.SessionStart', () => ({}))
  const engine = await routedSession($, on)
  await clearInto($, engine)
  const release = stateGate(engine)
  await startAfterClear($, engine)
  let isDone = false
  const launched = $.agent.spawn({ prompt: 'review it', description: 'review', subagentType: 'pinned-agent', model: 'sonnet' }).then(() => {
    isDone = true
  })
  await engine.clock.advance(1900)
  await settle(engine)
  expect(isDone).toBe(false)
  await engine.clock.advance(200)
  await settle(engine)
  expect(isDone).toBe(true)
  release()
  await launched
  expect(engine.calls.spawned[0].model).toBe('sonnet')
  await endSession($, engine)
  expect(decisionSessions(engine)).toEqual([])
})

const failingState = (engine: any, results: any[]) => {
  engine.script.runResult = ((base: any) => (argv: string[], init: any) => {
    if (argv[2] === 'router-state' && results.length > 0) return results.shift()
    return base(argv, init)
  })(engine.script.runResult)
}
const FAIL_EXIT = { exitCode: 3, stdout: '', stderr: 'boom', isStdoutTruncated: false, isStderrTruncated: false }
const FAIL_PARSE = { exitCode: 0, stdout: 'not json', stderr: '', isStdoutTruncated: false, isStderrTruncated: false }
const GOOD = (engine: any) => ({ exitCode: 0, stdout: JSON.stringify(engine.state), stderr: '', isStdoutTruncated: false, isStderrTruncated: false })

for (const [name, failure, text] of [
  ['a non-zero exit', FAIL_EXIT, '(exit 3): boom'],
  ['unparseable stdout', FAIL_PARSE, '(unparseable): not json'],
] as const) {
  test(`a failing router-state (${name}) with no run gives one toast per failure streak`, async ($: any, on: any) => {
    const engine = await routedSession($, on)
    const before = engine.calls.toasts.length
    failingState(engine, [failure, failure])
    await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
    await settle(engine)
    await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
    await settle(engine)
    const toasts = engine.calls.toasts.slice(before)
    expect(toasts.length).toBe(1)
    expect(toasts[0]).toContain('session routing is off for this session: router-state failed (')
    expect(toasts[0]).toContain('to reproduce: relais native router-state --session session-1')
    expect(toasts[0]).toContain(text)
  })
}

test('failure, failure, success, failure gives two toasts', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  const before = engine.calls.toasts.length
  failingState(engine, [FAIL_EXIT, FAIL_EXIT, GOOD(engine), FAIL_EXIT])
  for (let i = 0; i < 4; i++) {
    await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
    await settle(engine)
  }
  expect(engine.calls.toasts.slice(before).length).toBe(2)
})

test('a failing final flush is noted once at the next session start, after a clear and from a fresh store', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'rename foo to bar, tests exist')
  engine.observeExits.push(1)
  const before = engine.calls.toasts.length
  await $.session.end({ reason: 'clear', sessionId: 'session-1' })
  await settle(engine)
  expect(engine.calls.toasts.length).toBe(before)
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await settle(engine)
  const lost = engine.calls.toasts.slice(before).filter((t: string) => t.includes('lost'))
  expect(lost.length).toBe(1)
  expect(lost[0]).toContain('exit 1')
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await settle(engine)
  expect(engine.calls.toasts.slice(before).filter((t: string) => t.includes('lost')).length).toBe(1)
})

test('a plugin-origin prompt later in the session is not classified', async ($: any, on: any) => {
  const engine = await routedSession($, on)
  await personTurn($, engine, 'rename foo to bar, tests exist')
  const completes = engine.calls.completes.length
  await $.prompt.submit({ text: 'a plugin speaks', wait: false, origin: { kind: 'plugin', name: 'other' } })
  await settle(engine)
  expect(engine.calls.completes.length).toBe(completes)
})
