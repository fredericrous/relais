import { expect, test } from 'claude-code/testing'
import { continueLine, RELAIS_AGENT_TYPE, settle, spawnLine, startedRun } from './support.ts'
import { containsWords, decideTool, mintNonce } from '../hooks/decide.ts'
import { DECIDE_MESSAGE, decideGuard } from '../hooks/guards.ts'
import { createStore } from '../hooks/store.ts'

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

test('tool.check refuses the model’s learning-envelope calls and [learning] writes, naming /relais-learning', async ($: any, on: any) => {
  await startedRun($, on)
  const check = (tool: string, input: Record<string, unknown>) => $.tool.check({ tool, input, ...input })
  const bash = await check('Bash', { command: 'relais native learning-envelope --by me' })
  expect(bash.decision).toBe('deny')
  expect(bash.reason).toContain('/relais-learning')
  const write = await check('Write', { file_path: '/Users/p/.config/relais/machine.toml', content: '[learning]\nauto_promote = {}' })
  expect(write.decision).toBe('deny')
  expect(write.reason).toContain('/relais-learning')
  expect((await check('Bash', { command: 'relais learn last --json' })).decision).not.toBe('deny')
})

// The decide tool (ADR-0004), here rather than a file of its own: a
// fifteenth test file starves the suite's other children on this machine.
const NONCE = 'a'.repeat(64)
// What `relais decide --json` prints on success, as the Rust test
// `every_actor_but_the_terminal_one_round_trips_and_json_has_the_documented_shape`
// asserts it.
const decided = (actor: string) =>
  JSON.stringify({ run: 'run-1', resolution: 'decision_revised', actor, from_state: 'needs_decision', to_state: 'cancelled', advice: null })

// A relais, scripted: what `decide` and `decide-nonce` answer, and every
// call made with its stdin.
function world(reply: (argv: string[]) => { exitCode: number; stdout: string; stderr: string } = () => ({ exitCode: 0, stdout: decided('orchestrator'), stderr: '' })) {
  const calls: { argv: string[]; stdin?: string }[] = []
  let now = 1_000
  const fx: any = {
    process: {
      run: async (argv: string[], init: any) => {
        calls.push({ argv, stdin: init?.stdin })
        if (argv[1] === 'native' && argv[2] === 'decide-nonce') return { exitCode: 0, stdout: `${NONCE}\n`, stderr: '' }
        return reply(argv)
      },
    },
    clock: { now: async () => now },
  }
  return { fx, store: createStore(), calls, tick: (ms: number) => (now += ms) }
}

test('the tool records the orchestrator, sends the proof on stdin only, and parses the JSON answer', async () => {
  const w = world()
  await mintNonce(w.fx, w.store, 'session-1')
  const out: any = await decideTool(w.fx, w.store, { run: 'run-1', answer: 'revise', note: 'scope too wide' })
  const call = w.calls.find(c => c.argv[1] === 'decide')!
  expect(call.argv).toEqual(['relais', 'decide', 'run-1', '--answer', 'revise', '--actor', 'orchestrator', '--json', '--note', 'scope too wide', '--proof-stdin'])
  expect(call.argv.join(' ')).not.toContain(NONCE)
  expect(JSON.parse(call.stdin!)).toEqual({ session: 'session-1', nonce: NONCE })
  expect(out.result).toContain('by orchestrator')
})

test('without a minted nonce the call carries no proof', async () => {
  const w = world()
  await decideTool(w.fx, w.store, { run: 'run-1', answer: 'abandon' })
  const call = w.calls.find(c => c.argv[1] === 'decide')!
  expect(call.argv).not.toContain('--proof-stdin')
  expect(call.stdin).toBe(undefined)
})

test("on_behalf_of_person needs the person's own later words, whole, of at least 12 characters", async () => {
  const w = world(() => ({ exitCode: 0, stdout: decided('person-via-orchestrator'), stderr: '' }))
  const ask = (quote: unknown) =>
    decideTool(w.fx, w.store, { run: 'run-1', answer: 'revise', on_behalf_of_person: true, person_quote: quote }) as Promise<any>
  // The outcome never reached this session.
  expect((await ask('revise it, the scope is wide')).deny).toContain('not delivered in this session')
  w.store.outcomeDeliveredAt.set('run-1', 2_000)
  // The person wrote before the outcome arrived.
  w.store.lastPersonPrompt = { text: 'revise it, the scope is wide', at: 1_500 }
  expect((await ask('revise it, the scope is wide')).deny).toContain('has not written since')
  w.store.lastPersonPrompt = { text: 'Ok so revise it, the scope is wide.', at: 2_500 }
  expect((await ask('ok')).deny).toContain('at least 12')
  expect((await ask('approve it right away')).deny).toContain('not in the person')
  expect((await ask('vise it, the scope')).deny).toContain('not in the person')
  const out = await ask('revise it, the scope is wide')
  expect(out.result).toContain('person-via-orchestrator')
  const call = w.calls.find(c => c.argv[1] === 'decide')!
  expect(call.argv.slice(5, 10)).toEqual(['--actor', 'person-via-orchestrator', '--json', '--person-quote', 'revise it, the scope is wide'])
})

test("a relais refusal comes back as the tool's result, in relais's words", async () => {
  const w = world(() => ({ exitCode: 2, stdout: JSON.stringify({ error: 'run run-1 still carries verification gaps (x)', exit: 2 }), stderr: '' }))
  const out: any = await decideTool(w.fx, w.store, { run: 'run-1', answer: 'approve' })
  expect(out.result).toBe('relais refused the answer (exit 2): run run-1 still carries verification gaps (x)')
})

test('an input the tool cannot pass on is denied before relais runs', async () => {
  const w = world()
  expect(((await decideTool(w.fx, w.store, { answer: 'revise' })) as any).deny).toContain('run')
  expect(((await decideTool(w.fx, w.store, { run: 'run-1', answer: 'yes' })) as any).deny).toContain('approve, revise')
  expect(w.calls.length).toBe(0)
})

test('whole words: a quote must start and end on word boundaries', () => {
  expect(containsWords('Please  REVISE it now', 'revise it')).toBe(true)
  expect(containsWords('unrevise it', 'revise it')).toBe(false)
  expect(containsWords('revise itself', 'revise it')).toBe(false)
  expect(containsWords('yes, revise it.', 'revise it.')).toBe(true)
})

test('the Bash reminder catches relais decide as a command, not as text', () => {
  for (const command of ['relais decide run-1 --answer revise', 'cd x && relais decide r', 'echo; relais decide r', 'out=$(relais decide r)', '/usr/local/bin/relais decide r']) {
    expect(decideGuard('Bash', { command })).toBe(DECIDE_MESSAGE)
  }
  for (const command of ['grep -rn "relais decide" docs/', 'echo relais decide', 'relais native decide-nonce --session s', 'relais status']) {
    expect(decideGuard('Bash', { command })).toBe(undefined)
  }
  expect(decideGuard('Write', { command: 'relais decide r' })).toBe(undefined)
})
