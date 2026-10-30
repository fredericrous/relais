// The session router's pure decisions (hooks/router.ts).

import { expect, test } from 'claude-code/testing'
import {
  cacheGate,
  classifierPrompt,
  decideMain,
  decideSpawn,
  decisionRecord,
  evidenceOf,
  explorationDraw,
  fileHash,
  gateChecks,
  matchesCheck,
  mayWrite,
  modeEffective,
  newTask,
  onEvidence,
  outcomeOf,
  parseClassification,
  parseGateStamp,
  parseRouterState,
  rerun,
  revertedPaths,
  sha256Hex,
  tableTier,
  taskRecord,
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
const mainInput = () => ({
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
})
const main = (over: Record<string, unknown> = {}) => decideMain({ ...mainInput(), ...over } as any)

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

test('lint, vet and test commands are passing checks, each with its kind; a failing exit is a failed check', () => {
  const ok = { result: {}, text: 'ok' }
  const failed = { isError: true, result: 'Exit code 1', text: 'Exit code 1' }
  const run = (command: string, result: unknown) => evidenceOf('Bash', { command }, result, state.checks, state.check_kinds)
  expect(run('golangci-lint run ./...', ok)).toEqual({ kind: 'check', command: 'golangci-lint run ./...', passed: true, checkKind: 'lint' })
  expect(run('go vet ./...', ok)).toEqual({ kind: 'check', command: 'go vet ./...', passed: true, checkKind: 'lint' })
  expect(run('go test ./...', ok)).toEqual({ kind: 'check', command: 'go test ./...', passed: true, checkKind: 'test' })
  expect(run('go vet ./...', failed)).toEqual({ kind: 'check', command: 'go vet ./...', passed: false, checkKind: 'lint' })
  expect(run('go build ./...', ok)).toBe(undefined)
  let task = newTask('t', null, 'x', asClass(), 'research', 0)
  for (const command of ['golangci-lint run ./...', 'go vet ./...', 'go test ./...']) {
    task = onEvidence(state, task, run(command, ok)!, '/repo').task
  }
  expect(taskRecord(task, 1000, '/repo').checks).toEqual([
    { name: 'golangci-lint run ./...', kind: 'lint', passed: true },
    { name: 'go vet ./...', kind: 'lint', passed: true },
    { name: 'go test ./...', kind: 'test', passed: true },
  ])
  expect(outcomeOf(task)).toBe('completed_verified')
})

test('a relais that serves no check kinds has them read off the argv', () => {
  const { check_kinds: _kinds, ...served } = ROUTER_STATE
  const old = parseRouterState(JSON.stringify(served))!
  expect(old.check_kinds).toEqual(['test', 'test', 'test', 'lint', 'lint'])
})

test('a gate stamp: a pass inside the window is a passing check, a fail a failed one, outside runs are ignored', () => {
  const start = 1_000_000_000_000
  const end = start + 60_000
  const epoch = (offsetSeconds: number) => start / 1000 + offsetSeconds
  const stamp = (passed: number[], failed: number[]) =>
    parseGateStamp(
      JSON.stringify({
        stamped: true,
        tree: 'abcdef0123456789',
        passed: passed.map(e => ({ gate: 'pre-push', epoch: e })),
        failed: failed.map(e => ({ gate: 'pre-push', epoch: e })),
      }),
    )!
  const name = 'amont-gate@abcdef0'
  expect(gateChecks(stamp([epoch(10)], []), start, end)).toEqual([{ name, kind: 'gate', passed: true }])
  expect(gateChecks(stamp([], [epoch(10)]), start, end)).toEqual([{ name, kind: 'gate', passed: false }])
  // A fail after the pass: only the failure; a pass after the fail: the last word is a pass.
  expect(gateChecks(stamp([epoch(10)], [epoch(20)]), start, end)).toEqual([{ name, kind: 'gate', passed: false }])
  expect(gateChecks(stamp([epoch(20)], [epoch(10)]), start, end).map(c => c.passed)).toEqual([false, true])
  expect(gateChecks(stamp([epoch(-100), epoch(1000)], [epoch(-50)]), start, end)).toEqual([])
  expect(gateChecks({ stamped: false }, start, end)).toEqual([])
  expect(parseGateStamp('{"stamped":false}')).toEqual({ stamped: false })
  expect(parseGateStamp('nope')).toBe(undefined)
})

test('mode_effective is router-state’s mode, and shadow without a state', () => {
  expect(modeEffective(state)).toBe('on')
  expect(modeEffective({ ...state, mode: 'shadow' })).toBe('shadow')
  expect(modeEffective({ ...state, mode: 'off' })).toBe('off')
  expect(modeEffective(undefined)).toBe('shadow')
})

test('file hashes are the first 16 hex of the sha256 of the repo-relative path', () => {
  expect(sha256Hex('')).toBe('e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855')
  expect(sha256Hex('abc')).toBe('ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad')
  expect(sha256Hex('a'.repeat(1000)).length).toBe(64)
  expect(fileHash('/repo/src/lib.rs', '/repo')).toBe('b1a35a68f14e6962')
  expect(fileHash('./src/lib.rs', '/repo')).toBe('b1a35a68f14e6962')
  const task = { ...newTask('t', null, 'x', asClass(), 'research', 0), files: ['/repo/src/lib.rs', 'src/lib.rs'] }
  const record = taskRecord(task, 1000, '/repo')
  expect(record.files).toEqual(['b1a35a68f14e6962'])
  expect(record.completed_at_end).toBe('unknown')
  expect(taskRecord(task, null, '/repo').completed_at_end).toBe(null)
})

test('reverts: git revert, checkout -- paths and restore are found; a branch switch or unstaging is not', () => {
  expect(revertedPaths('git restore src/lib.rs')).toEqual(['src/lib.rs'])
  expect(revertedPaths('git restore --source=HEAD~1 -- a b')).toEqual(['a', 'b'])
  expect(revertedPaths('git restore --staged src/lib.rs')).toBe(undefined)
  expect(revertedPaths('git restore -S -W src/lib.rs')).toEqual(['src/lib.rs'])
  expect(revertedPaths('git checkout -- a.rs')).toEqual(['a.rs'])
  expect(revertedPaths('git checkout HEAD~2 -- a.rs b.rs')).toEqual(['a.rs', 'b.rs'])
  expect(revertedPaths('git checkout main')).toBe(undefined)
  expect(revertedPaths('git revert --no-edit abc123')).toEqual([])
  expect(revertedPaths('cargo test && git restore x.rs')).toEqual(['x.rs'])
  expect(revertedPaths('echo git restore x')).toBe(undefined)
})

test('outcomes: corrected outranks verified; an accept is completed_accepted; inferred signals are no outcome', () => {
  const task = newTask('t', null, 'x', asClass(), 'research', 0)
  expect(outcomeOf(task)).toBe('answered')
  expect(outcomeOf({ ...task, inferred: ['no_complaint'] })).toBe('answered')
  expect(outcomeOf({ ...task, inferred: ['aborted', 'no_complaint'] })).toBe('unknown')
  expect(outcomeOf({ ...task, acceptQuote: 'ship it' })).toBe('completed_accepted')
  expect(outcomeOf({ ...task, relaisAccepted: true })).toBe('completed_verified')
  expect(outcomeOf({ ...task, corrected: true, relaisAccepted: true })).toBe('corrected')
})

test('outcomes: a task that edited or might have written is unknown, never answered', () => {
  const task = newTask('t', null, 'x', asClass(), 'research', 0)
  expect(outcomeOf({ ...task, hasEdit: true, files: ['/r/a.rs'] })).toBe('unknown')
  // An edit with no file (a notebook call without a path) is still an edit.
  expect(outcomeOf({ ...task, hasEdit: true, files: [] })).toBe('unknown')
  expect(outcomeOf({ ...task, maybeWrote: true })).toBe('unknown')
  expect(outcomeOf({ ...task, hasEdit: true, files: ['/r/a.rs'], verifications: [{ command: 'cargo test', passed: true }] })).toBe('completed_verified')
  expect(outcomeOf({ ...task, maybeWrote: true, acceptQuote: 'ship it' })).toBe('completed_accepted')
  expect(outcomeOf({ ...task, maybeWrote: true, corrected: true })).toBe('corrected')
})

test('mayWrite: sed -i, redirections, tee, git writers and formatters write; reads do not', () => {
  for (const command of ['sed -i s/a/b/ f', "sed -i '' s/a/b/ f", 'echo x > f', 'cat a >> b', 'ls | tee out', 'git apply p.diff', 'git commit -m x', 'git -C r checkout main', 'cargo fmt', 'prettier --write .', 'FOO=1 rm -f x', 'relais run --task t.json', 'relais dataset replay --recipe r', 'git rm f', 'git mv a b', 'git clean -fd', 'env FOO=1 rm x', 'env -i sed -i s/a/b/ f', 'xargs rm', 'npx prettier --write .', 'bunx prettier --write .', 'pnpm exec prettier --write .', 'uv run black .', 'perl -i -pe s/a/b/ f', 'perl -pi -e s/a/b/ f', 'cargo clippy --fix']) {
    expect(mayWrite(command)).toBe(true)
  }
  for (const command of ['sed -n 1,5p f', 'grep -rn foo . 2>/dev/null', 'ls 2>&1', 'git status && git diff', 'git log --oneline', 'cargo fmt --check', 'cat f | head', 'cargo test', 'relais dataset replay --recipe r --dry-run', 'relais --version']) {
    expect(mayWrite(command)).toBe(false)
  }
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

test('router-state accepts keeps its strings, and is empty from a relais that predates it', () => {
  expect(state.accepts).toEqual(['answered', 'parent_task_id', 'unmatched_checks'])
  const { accepts: _, ...older } = ROUTER_STATE
  expect(parseRouterState(JSON.stringify(older))!.accepts).toEqual([])
  expect(parseRouterState(JSON.stringify({ ...ROUTER_STATE, accepts: ['answered', 3, null] }))!.accepts).toEqual(['answered'])
})

// --- exploration (learning-loop Phase 4) ---------------------------------------

// The same vector crate::rng's tests check: both languages draw alike.
test('the exploration draw matches the vector shared with Rust, non-ASCII ids included', () => {
  expect(explorationDraw('0123456789abcdef', 'session-1', 'task-1')).toBe(0.24444899038069579)
  expect(explorationDraw('fedcba9876543210', 'sessión-é', 'tâche-日本語-🚀')).toBe(0.8532650270632014)
  expect(explorationDraw('', '', '')).toBe(0.96696972315229)
  expect(explorationDraw('a', 'bc', 'd')).toBe(0.34418990774171765)
  expect(explorationDraw('ab', 'c', 'd')).not.toBe(explorationDraw('a', 'bc', 'd'))
})

test('10,000 seeded draws are the sequence Rust draws, and fall below 0.1 ten percent of the time', () => {
  let sum = 0n
  let below = 0
  for (let i = 0; i < 10_000; i++) {
    const draw = explorationDraw('seed-10k', 'session-10k', `task-${i}`)
    sum = (sum + BigInt(draw * 2 ** 53)) & ((1n << 64n) - 1n)
    if (draw < 0.1) below += 1
  }
  expect(sum).toBe(8148712194814526056n)
  expect(below).toBe(1032)
})

test('router-state serves the seed and epsilon; an epsilon out of range, or none, is 0', () => {
  expect(state.seed).toBe('0123456789abcdef')
  expect(state.epsilon).toBe(0.1)
  const without = parseRouterState(JSON.stringify({ ...ROUTER_STATE, seed: undefined, epsilon: undefined }))!
  expect([without.seed, without.epsilon]).toEqual(['', 0])
  expect(parseRouterState(JSON.stringify({ ...ROUTER_STATE, epsilon: 7 }))!.epsilon).toBe(0)
})

const SESSION = 'session-x'
// A task id the seed draws below (or at least) epsilon.
const taskIdWhere = (below: boolean, seed = state.seed, epsilon = state.epsilon) => {
  for (let i = 0; ; i++) {
    const id = `task-${i}`
    if (explorationDraw(seed, SESSION, id) < epsilon === below) return id
  }
}
const explore = (over: Record<string, unknown> = {}) => main({ session: SESSION, newTaskId: taskIdWhere(true), ...over })

test('below epsilon a new main task starts one tier lower; the draw, propensity and gate are recorded', () => {
  const out = explore()
  expect(out.task!.tier).toBe('research')
  expect(out.task!.exploredDown).toBe(true)
  expect(out.decision!.exploration).toMatchObject({ explored: true, propensity: 0.1 })
  const record = decisionRecord({ task: out.task!, turnId: 't', agentId: null, relation: 'new_task', decision: out.decision!, mode: 'on', holdout: false, applied: true, now: 0 })
  expect(record).toMatchObject({ explored: true, propensity: 0.1, would_pass_gate: true, applied: true })
  expect(record.draw).toBe(explorationDraw(state.seed, SESSION, out.task!.id))
  expect(record.draw).toBeLessThan(0.1)
})

test('at or above epsilon the table stands, with propensity 1 - epsilon', () => {
  const out = main({ session: SESSION, newTaskId: taskIdWhere(false) })
  expect(out.task!.tier).toBe('implementation')
  expect(out.decision!.exploration).toMatchObject({ explored: false, propensity: 0.9 })
  expect(out.task!.exploredDown).toBe(false)
})

test('escalation explores to implementation, and research cannot be lowered: not explored, propensity 1', () => {
  const hard = explore({ cls: cls({ difficulty: 5 }) })
  expect(hard.task!.tier).toBe('implementation')
  expect(hard.decision!.exploration!.explored).toBe(true)
  const easy = explore({ cls: cls({ difficulty: 1 }) })
  expect(easy.task!.tier).toBe('research')
  expect(easy.decision!.exploration).toMatchObject({ explored: false, propensity: 1 })
  expect(easy.decision!.exploration!.draw).toBeLessThan(0.1)
  expect(easy.task!.exploredDown).toBe(false)
})

test('a drawn switch down that fails the cache gate is explored and not applied', () => {
  const out = explore({ contextTokens: 500_000_000 })
  expect(out.decision!.reason).toBe('cache_gate')
  expect(out.decision!.wouldPassGate).toBe(false)
  expect(out.decision!.override).toBe(false)
  expect(out.decision!.exploration!.explored).toBe(true)
  const record = decisionRecord({ task: out.task!, turnId: 't', agentId: null, relation: 'new_task', decision: out.decision!, mode: 'on', holdout: false, applied: false, now: 0 })
  expect(record).toMatchObject({ explored: true, applied: false, would_pass_gate: false })
})

test('held-out, shadow, epsilon 0, no session, continuations and corrections are never explored', () => {
  const id = taskIdWhere(true)
  const held = decideMain({ ...mainInput(), state: { ...state, holdout: true }, session: SESSION, newTaskId: id })
  const shadow = decideMain({ ...mainInput(), state: { ...state, mode: 'shadow' }, session: SESSION, newTaskId: id })
  const zero = decideMain({ ...mainInput(), state: { ...state, epsilon: 0 }, session: SESSION, newTaskId: id })
  const nobody = main({ newTaskId: id })
  for (const out of [held, shadow, zero, nobody]) {
    expect(out.decision!.exploration).toBe(undefined)
    expect(out.task!.tier).toBe('implementation')
    const record = decisionRecord({ task: out.task!, turnId: 't', agentId: null, relation: 'new_task', decision: out.decision!, mode: 'on', holdout: false, applied: true, now: 0 })
    expect([record.explored, record.propensity, record.draw]).toEqual([false, null, null])
  }
  const started = explore()
  const next = main({ task: started.task, cls: cls({ relation: 'continuation' }), session: SESSION, newTaskId: id })
  expect(next.decision!.exploration).toBe(undefined)
  expect(next.task!.tier).toBe('research')
  const corrected = main({ task: started.task, cls: cls({ relation: 'correction' }), session: SESSION, newTaskId: id })
  expect(corrected.decision!.exploration).toBe(undefined)
})

test('a table-routed spawn draws with its own task id; pins, person models, abstentions, excluded models, held-out and shadow spawns never draw', () => {
  const below = taskIdWhere(true)
  const drawn = decideSpawn({ state, type: 'Explore', cls: cls(), session: SESSION, taskId: below })
  expect(drawn.isRouted).toBe(true)
  expect(drawn.decision).toMatchObject({ tier: 'research', model: 'claude-haiku-5-5', reason: 'table', override: true, wouldPassGate: true })
  expect(drawn.decision!.exploration).toMatchObject({ explored: true, propensity: 0.1 })
  expect(drawn.decision!.exploration!.draw).toBe(explorationDraw(state.seed, SESSION, below))
  const record = decisionRecord({ task: newTask(below, null, 'x', asClass(), 'research', 0), turnId: null, agentId: 'a1', relation: 'subagent', decision: drawn.decision!, mode: 'on', holdout: false, applied: true, now: 0 })
  expect(record).toMatchObject({ task_id: below, explored: true, propensity: 0.1, applied: true })
  const undrawn = decideSpawn({ state, type: 'Explore', cls: cls(), session: SESSION, taskId: taskIdWhere(false) })
  expect(undrawn.decision).toMatchObject({ tier: 'implementation', model: 'claude-sonnet-5-5' })
  expect(undrawn.decision!.exploration).toMatchObject({ explored: false, propensity: 0.9 })
  const never = [
    decideSpawn({ state, type: 'pinned-agent', cls: cls(), session: SESSION, taskId: below }),
    decideSpawn({ state, type: 'Explore', cls: cls(), personModel: 'opus', session: SESSION, taskId: below }),
    decideSpawn({ state, type: 'Explore', cls: cls({ confidence: 0.2 }), session: SESSION, taskId: below }),
    decideSpawn({ state: { ...state, excluded_models: ['claude-haiku-5-5'] }, type: 'Explore', cls: cls(), session: SESSION, taskId: below }),
    decideSpawn({ state: { ...state, holdout: true }, type: 'Explore', cls: cls(), session: SESSION, taskId: below }),
    decideSpawn({ state: { ...state, mode: 'shadow' }, type: 'Explore', cls: cls(), session: SESSION, taskId: below }),
    decideSpawn({ state, type: 'Explore', cls: cls(), taskId: below }),
    decideSpawn({ state, type: 'Explore', cls: cls(), session: SESSION }),
  ]
  for (const out of never) {
    expect(out.decision!.exploration).toBe(undefined)
    expect(out.decision!.tier).not.toBe('research')
  }
  // The excluded lowered model keeps the table's tier, still routed.
  expect(never[3].decision).toMatchObject({ tier: 'implementation', reason: 'table' })
  expect(never[3].isRouted).toBe(true)
})

test('a drawn spawn starts one tier lower and its rerun keeps that floor', () => {
  const below = taskIdWhere(true)
  const drawn = decideSpawn({ state, type: 'Explore', cls: cls(), session: SESSION, taskId: below })
  const task = newTask(below, 'a1', 'x', drawn.class!, drawn.decision!.tier, 0)
  task.exploredDown = !!drawn.decision!.exploration?.explored
  expect(task.tier).toBe('research')
  const grown = rerun(state, task, 'scope_growth')
  expect(grown.task.tier).toBe('research')
  // Without the floor the table would raise it straight back.
  const forgot = rerun(state, { ...task, exploredDown: false }, 'scope_growth')
  expect(forgot.task.tier).toBe('implementation')
})

test('a research spawn records propensity 1 and explored false', () => {
  const easy = decideSpawn({ state, type: 'Explore', cls: cls({ difficulty: 1, kind: 'question' }), session: SESSION, taskId: taskIdWhere(true) })
  expect(easy.decision!.tier).toBe('research')
  expect(easy.decision!.exploration).toMatchObject({ explored: false, propensity: 1 })
  expect(easy.decision!.exploration!.draw).toBeLessThan(0.1)
})

test('10,000 seeded task starts at epsilon 0.1 explore 10% +/- 1%', () => {
  let explored = 0
  for (let i = 0; i < 10_000; i++) {
    const out = main({ session: 'session-10k', newTaskId: `task-${i}`, state: { ...state, seed: 'seed-10k' } })
    if (out.decision!.exploration!.explored) explored += 1
  }
  expect(explored).toBeGreaterThanOrEqual(900)
  expect(explored).toBeLessThanOrEqual(1100)
})
