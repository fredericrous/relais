// The session router's pure decisions (hooks/router.ts).

import { expect, test } from 'claude-code/testing'
import {
  cacheGate,
  classifierPrompt,
  decideMain,
  decideSpawn,
  evidenceOf,
  matchesCheck,
  modeEffective,
  newTask,
  onEvidence,
  outcomeOf,
  parseClassification,
  parseRouterState,
  shadowWhy,
  tableTier,
  type Class,
} from '../hooks/router.ts'
import { ROUTER_STATE } from './routing-support.ts'

const state = parseRouterState(JSON.stringify(ROUTER_STATE))!
const cls = (over: Record<string, unknown> = {}) => ({
  relation: 'new_task',
  kind: 'edit',
  difficulty: 3,
  scope: 'local',
  uncertainty: 'low',
  verifiable: true,
  explicit: { value: 'none', quote: null },
  user_model: null,
  confidence: 0.9,
  ...over,
}) as any
const asClass = (over: Partial<Class> = {}): Class => ({
  kind: 'edit',
  difficulty: 3,
  scope: 'local',
  uncertainty: 'low',
  verifiable: true,
  confidence: 0.9,
  ...over,
})
const main = (over: Record<string, unknown> = {}) =>
  decideMain({
    state,
    task: undefined,
    cls: cls(),
    timedOut: false,
    forceNew: false,
    text: 'do it',
    sessionModel: 'claude-sonnet-5-5',
    lastModel: undefined,
    contextTokens: 0,
    newTaskId: 'task-1',
    now: 0,
    ...over,
  } as any)

test('the classifier reply is parsed through fences and prose; a missing relation is no reply', () => {
  const parsed = parseClassification('```json\n{"relation":"continuation","kind":"debug","difficulty":9,"scope":"x","uncertainty":"high","verifiable":true,"explicit":{"value":"accept","quote":"ship it"},"user_model":"opus","confidence":1.4}\n```')
  expect(parsed).toEqual({
    relation: 'continuation',
    kind: 'debug',
    difficulty: 5,
    scope: 'unknown',
    uncertainty: 'high',
    verifiable: true,
    explicit: { value: 'accept', quote: 'ship it' },
    user_model: 'opus',
    user_model_for: null,
    confidence: 1,
  })
  expect(parseClassification('{"kind":"edit"}')).toBe(undefined)
  expect(parseClassification('no json here')).toBe(undefined)
})

test('the classifier sees the first 2 KB of the prompt and a task summary of at most 600 bytes', () => {
  const task = newTask('t', null, 'x'.repeat(5000), asClass(), 'research', 0)
  const prompt = classifierPrompt('y'.repeat(5000), task)
  const [head, request] = prompt.split('\n\nREQUEST:\n')
  expect(new TextEncoder().encode(head.replace('ACTIVE TASK: ', '')).length <= 600).toBe(true)
  expect(request.length).toBe(2048)
})

test('the capability table: hard research escalates, a rename with tests is research, unknown scope is implementation', () => {
  const table = state.capability_table
  expect(tableTier(table, asClass({ kind: 'question', difficulty: 2, uncertainty: 'high', scope: 'cross-cutting', verifiable: false }))).toBe('escalation')
  expect(tableTier(table, asClass({ difficulty: 2 }))).toBe('research')
  expect(tableTier(table, asClass({ difficulty: 3, scope: 'unknown', uncertainty: 'medium' }))).toBe('implementation')
  expect(tableTier(table, asClass({ difficulty: 4 }))).toBe('escalation')
})

test('a continuation inherits the class: difficulty may rise, never fall, and the tier never drops', () => {
  const first = main({ cls: cls({ difficulty: 3, uncertainty: 'medium' }) })
  expect(first.task!.tier).toBe('implementation')
  const easier = main({ task: first.task, cls: cls({ relation: 'continuation', difficulty: 1, kind: 'question', uncertainty: 'low' }) })
  expect(easier.task!.class.difficulty).toBe(3)
  expect(easier.task!.class.kind).toBe('edit')
  expect(easier.task!.tier).toBe('implementation')
  expect(easier.decision!.reason).toBe('explicit_kept')
  const harder = main({ task: easier.task, cls: cls({ relation: 'continuation', difficulty: 4 }) })
  expect(harder.task!.class.difficulty).toBe(4)
  expect(harder.task!.tier).toBe('escalation')
  expect(harder.task!.escalations).toBe(0)
})

test('below 0.6 confidence a new task stays at /model; cheaper than /model only through the research rule', () => {
  const low = main({ cls: cls({ difficulty: 1, confidence: 0.5 }), sessionModel: 'claude-opus-5-5' })
  expect(low.decision!.tier).toBe('escalation')
  expect(low.decision!.reason).toBe('abstained')
  const implementation = main({ cls: cls({ difficulty: 3, uncertainty: 'medium' }), sessionModel: 'claude-opus-5-5' })
  expect(implementation.decision!.tier).toBe('escalation')
  const research = main({ cls: cls({ difficulty: 2 }), sessionModel: 'claude-opus-5-5' })
  expect(research.decision!.tier).toBe('research')
  expect(research.decision!.model).toBe('claude-haiku-5-5')
})

test('the cache gate: a large context with a small predicted saving does not switch down; up always passes', () => {
  const big = main({ cls: cls({ difficulty: 1 }), lastModel: 'claude-opus-5-5', contextTokens: 2_000_000 })
  expect(big.decision!.reason).toBe('cache_gate')
  expect(big.decision!.tier).toBe('escalation')
  expect(big.decision!.wouldPassGate).toBe(false)
  const small = main({ cls: cls({ difficulty: 1 }), lastModel: 'claude-opus-5-5', contextTokens: 1000 })
  expect(small.decision!.tier).toBe('research')
  const up = main({ cls: cls({ difficulty: 5 }), lastModel: 'claude-haiku-5-5', contextTokens: 2_000_000 })
  expect(up.decision!.tier).toBe('escalation')
  expect(up.decision!.reason).toBe('table')
  expect(cacheGate({ state, from: 'claude-opus-5-5', to: 'unpriced-model', contextTokens: 0, class: asClass() }).pass).toBe(false)
})

test('a red run before any edit is no evidence; a failure after an edit escalates one step', () => {
  let task = newTask('t', null, 'x', asClass({ difficulty: 2 }), 'research', 0)
  const red = onEvidence(state, task, { kind: 'check', command: 'cargo test', passed: false }, '/repo')
  expect(red.reassess).toBe(undefined)
  task = onEvidence(state, red.task, { kind: 'edit', file: '/repo/src/a.rs' }, '/repo').task
  const failed = onEvidence(state, task, { kind: 'check', command: 'cargo test', passed: false }, '/repo')
  expect(failed.reassess!.event).toBe('failed_verification')
  expect(failed.task.tier).toBe('implementation')
})

test('two failed repairs escalate twice; a third failure is exhausted; a correction then raises the effort', () => {
  let task = newTask('t', null, 'x', asClass({ difficulty: 2 }), 'research', 0)
  const events: string[] = []
  for (let i = 0; i < 3; i++) {
    task = onEvidence(state, task, { kind: 'edit', file: '/repo/src/a.rs' }, '/repo').task
    const out = onEvidence(state, task, { kind: 'check', command: 'cargo test', passed: false }, '/repo')
    task = out.task
    events.push(out.reassess!.event)
  }
  expect(events).toEqual(['failed_verification', 'repair_failed', 'repair_failed'])
  expect(task.tier).toBe('escalation')
  expect(task.escalations).toBe(2)
  expect(task.exhausted).toBe(true)
  const corrected = decideMain({
    state, task, cls: cls({ relation: 'correction' }), timedOut: false, forceNew: false, text: 'wrong',
    sessionModel: 'claude-sonnet-5-5', lastModel: undefined, contextTokens: 0, newTaskId: 'x', now: 0,
  })
  expect(corrected.task!.tier).toBe('escalation')
  expect(corrected.decision!.effort).toBe('xhigh')
  expect(corrected.task!.escalations).toBe(2)
})

test('scope growth and green checks never escalate; growth may raise the tier through the table', () => {
  let task = newTask('t', null, 'x', asClass({ difficulty: 3, uncertainty: 'high', scope: 'local' }), 'implementation', 0)
  const one = onEvidence(state, task, { kind: 'edit', file: '/repo/src/a.rs' }, '/repo')
  expect(one.reassess).toBe(undefined)
  const two = onEvidence(state, one.task, { kind: 'edit', file: '/repo/tests/b.rs' }, '/repo')
  expect(two.reassess!.event).toBe('scope_growth')
  expect(two.reassess!.escalating).toBe(false)
  expect(two.task.class.scope).toBe('cross-cutting')
  expect(two.task.tier).toBe('escalation')
  expect(two.task.escalations).toBe(0)
  task = onEvidence(state, two.task, { kind: 'check', command: 'cargo test', passed: true }, '/repo').task
  expect(outcomeOf(task)).toBe('completed_verified')
})

test('checks are matched on argv prefixes after env assignments; background and refused runs are ignored', () => {
  const checks = state.checks
  expect(matchesCheck('RUST_LOG=debug cargo test -p relais', checks)).toBe(true)
  expect(matchesCheck('cd crates && cargo test', checks)).toBe(true)
  expect(matchesCheck('cargo build', checks)).toBe(false)
  expect(matchesCheck('echo cargo test', checks)).toBe(false)
  const failed = { isError: true, result: 'Exit code 101', text: 'Exit code 101\nerror' }
  expect(evidenceOf('Bash', { command: 'cargo test' }, failed, checks)).toEqual({ kind: 'check', command: 'cargo test', passed: false })
  expect(evidenceOf('Bash', { command: 'cargo test', run_in_background: true }, failed, checks)).toBe(undefined)
  expect(evidenceOf('Bash', { command: 'cargo test' }, { isError: true, text: 'Interrupted' }, checks)).toBe(undefined)
  expect(evidenceOf('Bash', { command: 'cargo test' }, { deny: 'no' }, checks)).toBe(undefined)
  expect(evidenceOf('Bash', { command: 'cargo test' }, { result: {}, text: 'ok' }, checks)).toEqual({ kind: 'check', command: 'cargo test', passed: true })
  expect(evidenceOf('Edit', { file_path: '/repo/a' }, { isError: true, text: 'no match' }, checks)).toBe(undefined)
})

test('mode_effective is the narrower of relais and the plugin store records', () => {
  const envelope = { answer: 'Allow session routing', at: 'now', session: 's' }
  expect(modeEffective(state, { envelope: undefined, r3: { id: 'r3-1' } })).toBe('shadow')
  expect(shadowWhy(state, { envelope: undefined, r3: { id: 'r3-1' } })).toContain('/relais-routing')
  expect(modeEffective(state, { envelope, r3: { id: 'r3-0' } })).toBe('shadow')
  expect(shadowWhy(state, { envelope, r3: { id: 'r3-0' } })).toContain('/relais-r3')
  expect(modeEffective(state, { envelope, r3: { id: 'r3-1' } })).toBe('on')
  expect(modeEffective({ ...state, mode: 'shadow' }, { envelope, r3: { id: 'r3-1' } })).toBe('shadow')
  expect(modeEffective({ ...state, mode: 'off' }, { envelope, r3: { id: 'r3-1' } })).toBe('off')
  expect(modeEffective(undefined, { envelope, r3: { id: 'r3-1' } })).toBe('shadow')
})

test('outcomes: corrected outranks verified; an accept is completed_accepted; inferred signals are no outcome', () => {
  const task = newTask('t', null, 'x', asClass(), 'research', 0)
  expect(outcomeOf(task)).toBe('unknown')
  expect(outcomeOf({ ...task, inferred: ['aborted', 'no_complaint'] })).toBe('unknown')
  expect(outcomeOf({ ...task, acceptQuote: 'ship it' })).toBe('completed_accepted')
  expect(outcomeOf({ ...task, relaisAccepted: true })).toBe('completed_verified')
  expect(outcomeOf({ ...task, corrected: true, relaisAccepted: true })).toBe('corrected')
})

test('a spawn: pins are kept, the person’s model honoured, low confidence abstains, the table routes', () => {
  expect(decideSpawn({ state, type: 'pinned-agent', cls: cls() }).decision!.reason).toBe('pin')
  expect(decideSpawn({ state, type: 'pinned-agent', cls: cls() }).isRouted).toBe(false)
  const named = decideSpawn({ state, type: 'Explore', cls: cls(), personModel: 'opus' })
  expect(named.decision!.model).toBe('claude-opus-5-5')
  expect(named.decision!.reason).toBe('user_model')
  expect(named.isRouted).toBe(false)
  // A model named inside the spawn prompt is the parent's preference: routed by the table.
  const parentAsked = decideSpawn({ state, type: 'Explore', cls: cls({ user_model: 'opus', difficulty: 1, kind: 'question' }) })
  expect(parentAsked.decision!.reason).toBe('table')
  expect(parentAsked.decision!.model).toBe('claude-haiku-5-5')
  expect(decideSpawn({ state, type: 'Explore', cls: cls({ confidence: 0.2 }) }).decision!.override).toBe(false)
  const routed = decideSpawn({ state, type: 'Explore', cls: cls({ difficulty: 1, kind: 'question' }) })
  expect(routed.isRouted).toBe(true)
  expect(routed.decision!.model).toBe('claude-haiku-5-5')
})

test('router-state that does not parse, or names no tier models, is none', () => {
  expect(parseRouterState('nope')).toBe(undefined)
  expect(parseRouterState(JSON.stringify({ ...ROUTER_STATE, schema: 2 }))).toBe(undefined)
  expect(parseRouterState(JSON.stringify({ ...ROUTER_STATE, tiers: { research: { model: 'x' } } }))).toBe(undefined)
})
