// The programme read: when relais is asked, what a reply is bound to, and
// what the pane shows of the answer.

import { expect, test } from 'claude-code/testing'
import { scriptedEngine, settle, type Engine } from './support.ts'
import { ADOPT_GIVE_UP_MS, readProgramme } from '../hooks/programme.ts'
import { createStore } from '../hooks/store.ts'

const view = (ticket: string, extra: Record<string, unknown> = {}) => ({
  repo: 'alpha',
  slug: 'p',
  ticket,
  kind: 'work',
  holder: 'self',
  counts: { closed: 0, frontier: 0, blocked: 0, in_flight: 1, open: 1, out_of_scope: 0 },
  tickets: [
    { id: ticket, name: `Ticket ${ticket}`, status: 'in_flight', blocked_by: [], open_blockers: [], dependents: [], due: null, held: true },
  ],
  ready: [],
  ...extra,
})

const answer = (programme: unknown, exitCode = 0, stderr = '') => ({
  exitCode,
  stdout: JSON.stringify({ programme }),
  stderr,
  isStdoutTruncated: false,
  isStderrTruncated: false,
})

const failure = (exitCode: number, stderr: string) => ({ exitCode, stdout: '', stderr, isStdoutTruncated: false, isStderrTruncated: false })

const ok = { exitCode: 0, stdout: '', stderr: '', isStdoutTruncated: false, isStderrTruncated: false }

const reads = (engine: Engine) => engine.calls.run.filter(c => c.argv[1] === 'programme' && c.argv[2] === 'session')

const adopts = (engine: Engine) => engine.calls.run.filter(c => c.argv[1] === 'programme' && c.argv[2] === 'adopt')

const START = { cwd: '/repo', surface: 'terminal', isInteractive: true }

async function started($: any, on: any, programme: unknown = view('t1')) {
  on('classic.SessionStart', () => ({}))
  const engine = scriptedEngine(on)
  engine.script.runResult = (argv: string[]) => (argv[1] === 'programme' ? answer(programme) : ok)
  await $.session.start(START)
  await settle(engine)
  return engine
}

// The pane's rows as drawn after the 100 ms tick.
const mounted = new WeakMap<object, any>()

async function paneTexts($: any, engine: Engine, bodyRows = 40) {
  await engine.clock.advance(200)
  await settle(engine)
  await mounted.get($)?.unmount()
  const ui = await $.ui.mount({
    plugin: 'relais',
    surface: 'terminal',
    component: 'Pane',
    requestId: 'relais',
    props: { title: 'relais', isFocused: false, bodyColumns: 72, placement: 'dock', scroll: { offset: 0, bodyRows }, view: {} },
    viewport: { columns: 100, rows: 40, isFullscreen: true },
  })
  mounted.set($, ui)
  const rows = (await ui.findAll({})).filter((e: any) => e.type === 'Text' || e.type === 'Button')
  return { ui, texts: rows.map((r: any) => r.text as string) }
}

test('session.start asks relais which ticket the session holds, bounded at 5 s', async ($: any, on: any) => {
  const engine = await started($, on)
  expect(reads(engine).length).toBe(1)
  expect(reads(engine)[0].argv).toEqual([
    'relais', 'programme', 'session', '--json', '--no-fetch', '--session', 'session-1', '--dir', '/repo',
  ])
  expect(reads(engine)[0].init.timeoutMs).toBe(5000)
  const { texts } = await paneTexts($, engine)
  expect(texts).toContain('programme p · alpha')
  expect(texts).toContain('▶ t1 Ticket t1 ◀ you')
  expect(engine.calls.statuses[engine.calls.statuses.length - 1]).toBe('▶ p t1')
})

test('an empty session id spawns nothing', async ($: any, on: any) => {
  on('classic.SessionStart', () => ({}))
  const engine = scriptedEngine(on)
  engine.script.sessionId = ''
  await $.session.start(START)
  await settle(engine)
  await $.classic.SessionStart({ source: 'clear' })
  await settle(engine)
  expect(reads(engine).length).toBe(0)
})

test('a main-chain relais programme claim, release or next reads again; nothing else does', async ($: any, on: any) => {
  const engine = await started($, on)
  for (const verb of ['claim p t1', 'release p t1', 'next --claim']) {
    await $.tool.call({ tool: 'Bash', command: `relais programme ${verb}` })
    await settle(engine)
  }
  expect(reads(engine).length).toBe(4)
  await $.tool.call({ tool: 'Bash', command: 'relais programme show .' })
  await $.tool.call({ tool: 'Bash', command: 'relais programme claim p t1', agentId: 'agent-1' })
  await $.tool.call({ tool: 'Read', command: 'relais programme claim p t1' })
  await settle(engine)
  expect(reads(engine).length).toBe(4)
})

test('the first read of a session opens the pane unasked, once', async ($: any, on: any) => {
  const engine = await started($, on)
  expect(engine.calls.opened.length).toBe(1)
  await $.tool.call({ tool: 'Bash', command: 'relais programme claim p t1' })
  await settle(engine)
  expect(engine.calls.opened.length).toBe(1)
})

test('no claim draws nothing and opens nothing', async ($: any, on: any) => {
  const engine = await started($, on, null)
  expect(engine.calls.opened.length).toBe(0)
  const { texts } = await paneTexts($, engine)
  expect(texts).toEqual(['relais · waiting for the first event'])
  expect(engine.calls.statuses.filter(s => s?.includes('▶')).length).toBe(0)
})

test('a reply older than the newest spawn is dropped', async ($: any, on: any) => {
  const engine = scriptedEngine(on)
  let release: () => void = () => {}
  const held = new Promise<void>(resolve => (release = resolve))
  let spawned = 0
  engine.script.runResult = (argv: string[]) => (argv[1] === 'programme' ? answer(view(spawned++ === 0 ? 'old' : 'new')) : ok)
  engine.script.runGate = (argv: string[]) => (argv[1] === 'programme' && spawned === 1 ? held : undefined)
  await $.session.start(START)
  await settle(engine)
  await $.tool.call({ tool: 'Bash', command: 'relais programme claim p new' })
  await settle(engine)
  release()
  await settle(engine)
  const { texts } = await paneTexts($, engine)
  expect(texts).toContain('▶ new Ticket new ◀ you')
  expect(texts.some(t => t.includes('Ticket old'))).toBe(false)
})

test('a reply for a session that has ended is dropped', async ($: any, on: any) => {
  const engine = scriptedEngine(on)
  let release: () => void = () => {}
  const held = new Promise<void>(resolve => (release = resolve))
  engine.script.runResult = (argv: string[]) => (argv[1] === 'programme' ? answer(view(argv[argv.indexOf('--session') + 1])) : ok)
  engine.script.runGate = (argv: string[]) => (argv.includes('session-1') ? held : undefined)
  await $.session.start(START)
  await settle(engine)
  await $.session.end({ reason: 'clear', sessionId: 'session-1' })
  engine.script.sessionId = 'session-2'
  await $.session.start(START)
  await settle(engine)
  release()
  await settle(engine)
  const { texts } = await paneTexts($, engine)
  expect(texts).toContain('▶ session-2 Ticket session-2 ◀ you')
  expect(texts.some(t => t.includes('session-1'))).toBe(false)
})

test('--previous names the session a /clear ended, for its successor only', async ($: any, on: any) => {
  const engine = await started($, on)
  await $.session.end({ reason: 'clear', sessionId: 'session-1' })
  engine.script.sessionId = 'session-2'
  await $.session.start(START)
  await settle(engine)
  const successor = reads(engine)[reads(engine).length - 1].argv
  expect(successor.slice(successor.indexOf('--session'))).toEqual(['--session', 'session-2', '--dir', '/repo', '--previous', 'session-1'])
  await $.classic.SessionStart({ source: 'resume' })
  await settle(engine)
  expect(reads(engine)[reads(engine).length - 1].argv).not.toContain('--previous')
})

for (const source of ['resume', 'fork']) {
  test(`a ${source} after a clear passes no --previous`, async ($: any, on: any) => {
    const engine = await started($, on)
    await $.session.end({ reason: 'clear', sessionId: 'session-1' })
    engine.script.sessionId = 'session-2'
    await $.classic.SessionStart({ source })
    await settle(engine)
    expect(reads(engine)[reads(engine).length - 1].argv).not.toContain('--previous')
  })
}

test('a session that ended by resume leaves no --previous behind', async ($: any, on: any) => {
  const engine = await started($, on)
  await $.session.end({ reason: 'resume', sessionId: 'session-1' })
  engine.script.sessionId = 'session-2'
  await $.session.start(START)
  await settle(engine)
  expect(reads(engine)[reads(engine).length - 1].argv).not.toContain('--previous')
})

test('an older relais is noted once in one dim row and never asked again', async ($: any, on: any) => {
  const engine = scriptedEngine(on)
  engine.script.runResult = (argv: string[]) =>
    argv[1] === 'programme' ? failure(2, "error: unrecognized subcommand 'session'\n\nUsage: relais programme <COMMAND>") : ok
  await $.session.start(START)
  await settle(engine)
  await $.tool.call({ tool: 'Bash', command: 'relais programme claim p t1' })
  await settle(engine)
  expect(reads(engine).length).toBe(1)
  const { texts } = await paneTexts($, engine)
  expect(texts.filter(t => t === 'programme view needs a newer relais · relais install').length).toBe(1)
})

test('any other exit 2 is retried on the next trigger', async ($: any, on: any) => {
  const engine = scriptedEngine(on)
  engine.script.runResult = (argv: string[]) => (argv[1] === 'programme' ? failure(2, 'error: unexpected argument') : ok)
  await $.session.start(START)
  await settle(engine)
  await $.tool.call({ tool: 'Bash', command: 'relais programme claim p t1' })
  await settle(engine)
  expect(reads(engine).length).toBe(2)
  const { texts } = await paneTexts($, engine)
  expect(texts.some(t => t.includes('programme view needs a newer relais'))).toBe(false)
})

test('exit 1 keeps the last view and adds one dim note', async ($: any, on: any) => {
  const engine = await started($, on)
  engine.script.runResult = (argv: string[]) => (argv[1] === 'programme' ? failure(1, 'relais: claim store unreadable') : ok)
  await $.tool.call({ tool: 'Bash', command: 'relais programme claim p t1' })
  await settle(engine)
  const { texts } = await paneTexts($, engine)
  expect(texts).toContain('▶ t1 Ticket t1 ◀ you')
  expect(texts.filter(t => t.startsWith('programme view may be stale')).length).toBe(1)
})

test('a read past 5 s keeps the view of the same session', async ($: any, on: any) => {
  const engine = await started($, on)
  engine.script.runGate = () => new Promise<void>(() => {})
  await $.tool.call({ tool: 'Bash', command: 'relais programme claim p t1' })
  await engine.clock.advance(5000)
  await settle(engine)
  const { texts } = await paneTexts($, engine)
  expect(texts).toContain('▶ t1 Ticket t1 ◀ you')
})

test('/relais-status counts the reads, and says nothing before any programme was shown', async ($: any, on: any) => {
  await started($, on, null)
  expect((await $.command.run({ command: 'relais-status' })).text).not.toContain('programme reads')
})

test('/relais-status counts the programme reads and the tails in one line', async ($: any, on: any) => {
  await started($, on)
  const text = (await $.command.run({ command: 'relais-status' })).text as string
  expect(text).toContain('programme reads: 1 shown, 0 none, 0 failed · tails: 0 frozen, 0 non-empty, 0 hidden (0 within 60 s)')
})

const brokenReplies: [string, Record<string, unknown>][] = [
  ['a kind that is neither work nor handoff', { kind: 'other' }],
  ['a holder that is neither self nor previous', { holder: 'nobody' }],
  ['counts with fields missing', { counts: { closed: 0 } }],
  [
    'a ticket status outside the set',
    { tickets: [{ id: 't1', name: 'x', status: 'unknown', blocked_by: [], open_blockers: [], dependents: [], due: null, held: true }] },
  ],
  ['a ready list that is not a list', { ready: 't4' }],
]

for (const [what, extra] of brokenReplies) {
  test(`a reply with ${what} is unreadable: the stale note, no view`, async ($: any, on: any) => {
    const engine = await started($, on, view('t1', extra))
    const { texts } = await paneTexts($, engine)
    expect(texts.some(t => t.startsWith('programme view may be stale'))).toBe(true)
    expect(texts).not.toContain('programme p · alpha')
  })
}

test('an exit code other than 0, 1 or 2 sets the stale note', async ($: any, on: any) => {
  const engine = await started($, on)
  engine.script.runResult = (argv: string[]) => (argv[1] === 'programme' ? failure(101, 'panicked') : ok)
  await $.tool.call({ tool: 'Bash', command: 'relais programme claim p t1' })
  await settle(engine)
  const { texts } = await paneTexts($, engine)
  expect(texts).toContain('▶ t1 Ticket t1 ◀ you')
  expect(texts.filter(t => t.startsWith('programme view may be stale')).length).toBe(1)
})

// f12: the session a /clear starts takes its predecessor's handoff over.

async function cleared($: any, on: any, adoptAnswer: (argv: string[]) => any) {
  const engine = await started($, on)
  engine.script.runResult = (argv: string[]) =>
    argv[1] !== 'programme' ? ok : argv[2] === 'adopt' ? adoptAnswer(argv) : answer(view('t19'))
  await $.session.end({ reason: 'clear', sessionId: 'session-1' })
  engine.script.sessionId = 'session-2'
  await $.session.start(START)
  await settle(engine)
  return engine
}

test('a clear adopts the handoff of the ended session for its successor, then reads', async ($: any, on: any) => {
  const engine = await cleared($, on, () => ok)
  expect(adopts(engine).length).toBe(1)
  expect(adopts(engine)[0].argv).toEqual(['relais', 'programme', 'adopt', '--previous', 'session-1', '--session', 'session-2'])
  expect(adopts(engine)[0].init.timeoutMs).toBe(5000)
  const runs = engine.calls.run.filter(c => c.argv[1] === 'programme')
  const adoptAt = runs.indexOf(adopts(engine)[0])
  expect(runs[adoptAt + 1].argv[2]).toBe('session')
  const { texts } = await paneTexts($, engine)
  expect(texts.some(t => t.startsWith('ticket not adopted'))).toBe(false)
  await $.tool.call({ tool: 'Bash', command: 'relais programme claim p t19' })
  await settle(engine)
  expect(adopts(engine).length).toBe(1)
})

test('no adoption on a first start, a resume or a fork', async ($: any, on: any) => {
  const engine = await started($, on)
  for (const source of ['resume', 'fork']) {
    await $.classic.SessionStart({ source })
    await settle(engine)
  }
  expect(adopts(engine).length).toBe(0)
  await $.session.end({ reason: 'resume', sessionId: 'session-1' })
  engine.script.sessionId = 'session-2'
  await $.session.start(START)
  await settle(engine)
  expect(adopts(engine).length).toBe(0)
})

test('a failed adoption notes the claim command and is retried on the next read', async ($: any, on: any) => {
  let answers = 0
  const engine = await cleared($, on, () => (answers++ === 0 ? failure(1, 'relais: a lock stayed held') : ok))
  expect(adopts(engine).length).toBe(1)
  let { texts } = await paneTexts($, engine)
  expect(texts).toContain('ticket not adopted · relais programme claim p t19')
  await $.tool.call({ tool: 'Bash', command: 'relais programme next --claim' })
  await settle(engine)
  expect(adopts(engine).length).toBe(2)
  expect(adopts(engine)[1].argv).toEqual(['relais', 'programme', 'adopt', '--previous', 'session-1', '--session', 'session-2'])
  ;({ texts } = await paneTexts($, engine))
  expect(texts.some(t => t.startsWith('ticket not adopted'))).toBe(false)
  await $.tool.call({ tool: 'Bash', command: 'relais programme next --claim' })
  await settle(engine)
  expect(adopts(engine).length).toBe(2)
})

test('a pending adoption is retried with its ids after a resume of the same session', async ($: any, on: any) => {
  let answers = 0
  const engine = await cleared($, on, () => (answers++ === 0 ? failure(1, 'relais: a lock stayed held') : ok))
  await $.classic.SessionStart({ source: 'resume' })
  await settle(engine)
  expect(adopts(engine).length).toBe(2)
  expect(adopts(engine)[1].argv).toContain('session-1')
})

test('an older relais without adopt is noted as older and never retried; the read goes on', async ($: any, on: any) => {
  const engine = await cleared($, on, () => failure(2, "error: unrecognized subcommand 'adopt'\n\nUsage: relais programme <COMMAND>"))
  await $.tool.call({ tool: 'Bash', command: 'relais programme claim p t19' })
  await settle(engine)
  expect(adopts(engine).length).toBe(1)
  const { texts } = await paneTexts($, engine)
  expect(texts).toContain('▶ t19 Ticket t19 ◀ you')
  expect(texts.some(t => t.startsWith('ticket not adopted') || t.includes('needs a newer relais'))).toBe(false)
})

test('any other exit 2 from adopt ends the adoption without a note', async ($: any, on: any) => {
  const engine = await cleared($, on, () => failure(2, 'relais: --previous is this session'))
  await $.tool.call({ tool: 'Bash', command: 'relais programme claim p t19' })
  await settle(engine)
  expect(adopts(engine).length).toBe(1)
  const { texts } = await paneTexts($, engine)
  expect(texts.some(t => t.startsWith('ticket not adopted'))).toBe(false)
})

test('a second clear while an adoption runs drops it; the new successor adopts its own', async ($: any, on: any) => {
  const engine = await started($, on)
  let release: () => void = () => {}
  const held = new Promise<void>(resolve => (release = resolve))
  engine.script.runResult = (argv: string[]) => (argv[1] !== 'programme' ? ok : argv[2] === 'adopt' ? ok : answer(view('t19')))
  engine.script.runGate = (argv: string[]) => (argv[2] === 'adopt' && argv.includes('session-2') ? held : undefined)
  await $.session.end({ reason: 'clear', sessionId: 'session-1' })
  engine.script.sessionId = 'session-2'
  await $.session.start(START)
  await settle(engine)
  await $.session.end({ reason: 'clear', sessionId: 'session-2' })
  engine.script.sessionId = 'session-3'
  await $.session.start(START)
  await settle(engine)
  release()
  await settle(engine)
  const own = adopts(engine).filter(c => c.argv.includes('session-3'))
  expect(own.length).toBe(1)
  expect(own[0].argv).toEqual(['relais', 'programme', 'adopt', '--previous', 'session-2', '--session', 'session-3'])
})

test('an adoption still pending 2 h after its first read stops and keeps its note', async () => {
  const store = createStore()
  store.programme.session = 'session-2'
  store.programme.adopt = { state: 'failed', previous: 'session-1', session: 'session-2', since: 0 }
  const runs: string[][] = []
  const fx: any = {
    kv: { get: async () => [], set: async () => undefined },
    clock: { now: async () => ADOPT_GIVE_UP_MS + 1000, after: () => ({ cancel: () => undefined }) },
    session: { id: async () => 'session-2' },
    process: {
      run: async (argv: string[]) => {
        runs.push(argv)
        return { exitCode: 0, stdout: JSON.stringify({ programme: null }), stderr: '' }
      },
    },
    ui: { open: async () => undefined },
  }
  await readProgramme(fx, store, 'command')
  await readProgramme(fx, store, 'command')
  expect(runs.filter(argv => argv[2] === 'adopt').length).toBe(0)
  expect(runs.filter(argv => argv[2] === 'session').length).toBe(2)
  expect(store.programme.note).toBe('ticket not adopted in 2 h · its handoff lapsed')
})

test('two reads started together after a clear spawn one adopt', async () => {
  const store = createStore()
  store.programme.adopt = { state: 'unbound', previous: 'session-1', session: undefined }
  let release: () => void = () => {}
  const clockHeld = new Promise<void>(resolve => (release = resolve))
  const runs: string[][] = []
  const fx: any = {
    kv: { get: async () => [], set: async () => undefined },
    clock: { now: async () => (await clockHeld, 1000), after: () => ({ cancel: () => undefined }) },
    session: { id: async () => 'session-2' },
    process: {
      run: async (argv: string[]) => {
        runs.push(argv)
        return { exitCode: 0, stdout: JSON.stringify({ programme: null }), stderr: '' }
      },
    },
    ui: { open: async () => undefined },
  }
  const both = Promise.all([readProgramme(fx, store, 'start'), readProgramme(fx, store, 'clear')])
  await new Promise(resolve => setTimeout(resolve, 0))
  release()
  await both
  expect(runs.filter(argv => argv[2] === 'adopt').length).toBe(1)
})

test('a failed adopt records its cause and first stderr line', async () => {
  const store = createStore()
  store.programme.adopt = { state: 'unbound', previous: 'session-1', session: undefined }
  const kv = new Map<string, unknown>()
  const fx: any = {
    kv: { get: async (key: string) => kv.get(key), set: async (key: string, value: unknown) => void kv.set(key, value) },
    clock: { now: async () => 1000, after: () => ({ cancel: () => undefined }) },
    session: { id: async () => 'session-2' },
    process: {
      run: async (argv: string[]) =>
        argv[2] === 'adopt'
          ? { exitCode: 1, stdout: '', stderr: 'relais: alpha p t19 not adopted: lock stayed held\n' }
          : { exitCode: 0, stdout: JSON.stringify({ programme: null }), stderr: '' },
    },
    ui: { open: async () => undefined },
  }
  await readProgramme(fx, store, 'clear')
  expect(kv.get('relais.programmeEvents')).toContainEqual({
    at: 1000,
    kind: 'adopt',
    result: 'error',
    cause: 'exit',
    detail: 'relais: alpha p t19 not adopted: lock stayed held',
  })
  expect(store.programme.adopt?.state).toBe('failed')
})
