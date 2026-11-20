// What came before a /clear: the live tail on each main turn, the freeze at
// the clear, and `[ hide ]`.

import { expect, test } from 'claude-code/testing'
import { tailOf, textMessages, trimTail, TAIL_CHARS } from '../hooks/clear-tail.ts'
import { scriptedEngine, settle, turn, type Engine } from './support.ts'

const START = { cwd: '/repo', surface: 'terminal', isInteractive: true }
const KEY = 'relais.clearTail'

const talk = [
  { role: 'user', text: 'hand off now' },
  { role: 'assistant', text: 'safe to /clear', toolUses: [{ tool: 'Bash', input: { command: 'ls' }, text: 'files' }] },
  { role: 'user', text: '<task-notification><task-id>agent-1</task-id></task-notification>' },
  { role: 'assistant', text: '' },
  { role: 'system', text: 'not a conversation message' },
]

const mainTurn = () => {
  const { agentId: _agentId, ...main } = turn('x', {})
  return main
}

// What the plugin's store holds, by key: the test hands the engine this object.
const kv: Record<string, unknown> = {}

async function started($: any, on: any, seed: Record<string, unknown> = {}, isPlaced = true, hangWrites = false) {
  for (const key of Object.keys(kv)) delete kv[key]
  Object.assign(kv, seed)
  on('classic.SessionStart', () => ({}))
  const engine = scriptedEngine(on, { store: kv, isPlaced, hangStoreWrites: hangWrites })
  engine.script.messages = () => talk
  await $.session.start(START)
  await settle(engine)
  return engine
}

// The Pane is one instance: a second mount in a test replaces the first.
const mounted = new WeakMap<object, any>()

async function drawn($: any, engine: Engine) {
  await engine.clock.advance(200)
  await settle(engine)
  await mounted.get($)?.unmount()
  const ui = await $.ui.mount({
    plugin: 'relais',
    surface: 'terminal',
    component: 'Pane',
    requestId: 'relais',
    props: { title: 'relais', isFocused: false, bodyColumns: 72, placement: 'dock', scroll: { offset: 0, bodyRows: 40 }, view: {} },
    viewport: { columns: 100, rows: 40, isFullscreen: true },
  })
  mounted.set($, ui)
  const rows = (await ui.findAll({})).filter((e: any) => e.type === 'Text' || e.type === 'Button')
  return { ui, texts: rows.map((r: any) => r.text as string) }
}

const clear = async ($: any, engine: Engine) => {
  await $.session.end({ reason: 'clear', sessionId: 'session-1' })
  await settle(engine)
}

test('only user and assistant text is kept: no tool use, no notification, no empty message', () => {
  expect(textMessages(talk)).toEqual([
    { role: 'user', text: 'hand off now' },
    { role: 'assistant', text: 'safe to /clear' },
  ])
  expect(textMessages('nope')).toEqual([])
})

test('a tail is trimmed from the front at a message boundary to 2000 characters', () => {
  const messages = ['a', 'b', 'c'].map(text => ({ role: 'user' as const, text: text.repeat(900) }))
  const kept = trimTail(messages)
  expect(kept.map(m => m.text[0])).toEqual(['b', 'c'])
  expect(kept.reduce((n, m) => n + m.text.length, 0)).toBeLessThanOrEqual(TAIL_CHARS)
})

test('only the newest 20 messages of a long session make the tail', () => {
  const many = Array.from({ length: 30 }, (_, n) => ({ role: 'user', text: `m${n}` }))
  expect(tailOf(many).map(m => m.text)).toEqual(Array.from({ length: 20 }, (_, n) => `m${n + 10}`))
})

test('one message longer than the limit is cut from its front with an ellipsis', () => {
  const kept = trimTail([{ role: 'assistant', text: `${'x'.repeat(3000)}END` }])
  expect(kept.length).toBe(1)
  expect(kept[0].text.startsWith('…')).toBe(true)
  expect(kept[0].text.endsWith('xxxEND')).toBe(true)
  expect(Array.from(kept[0].text).length).toBe(TAIL_CHARS)
})

test('each main turn reads the session with no argument the oldest supported engine refuses; a subagent turn reads nothing', async ($: any, on: any) => {
  const engine = await started($, on)
  await $.turn.complete(mainTurn())
  await settle(engine)
  // `{ last }` is refused by Claude Code 2.1.291, the plugin's floor.
  expect(engine.calls.messages).toEqual([{}])
  await $.turn.complete(turn('agent-9', {}))
  await settle(engine)
  expect(engine.calls.messages.length).toBe(1)
})

test('a /clear freezes the tail: rows, hour, a hide button and an unasked pane', async ($: any, on: any) => {
  const engine = await started($, on)
  await $.turn.complete(mainTurn())
  await settle(engine)
  const before = engine.calls.opened.length
  await clear($, engine)
  expect(engine.calls.opened.length).toBe(before + 1)
  const { texts } = await drawn($, engine)
  expect(texts.some(t => /^before \/clear · \d\d:\d\d \[ hide \]$/.test(t))).toBe(true)
  expect(texts).toContain('person │ hand off now')
  expect(texts).toContain('agent  │ safe to /clear')
  expect(texts.some(t => t.includes('task-notification') || t.includes('files'))).toBe(false)
  expect(engine.calls.statuses.some(status => status?.includes('before /clear saved'))).toBe(false)
})

test('the status line carries the saved tail only where the pane is not placed, after the run status', async ($: any, on: any) => {
  const engine = await started($, on, {}, false)
  await $.turn.complete(mainTurn())
  await settle(engine)
  await clear($, engine)
  await engine.clock.advance(200)
  await settle(engine)
  expect(engine.calls.statuses[engine.calls.statuses.length - 1]).toBe('before /clear saved · open the relais pane')
})

test('session.end by clear waits on no store write', async ($: any, on: any) => {
  const engine = await started($, on, {}, true, true)
  await $.turn.complete(mainTurn())
  await settle(engine)
  await $.session.end({ reason: 'clear', sessionId: 'session-1' })
  await engine.clock.advance(200)
  await settle(engine)
  const { texts } = await drawn($, engine)
  expect(texts).toContain('person │ hand off now')
})

test('a read that settles after a newer one began does not replace it', async ($: any, on: any) => {
  const engine = await started($, on)
  engine.script.messages = () => [{ role: 'user', text: 'older read' }]
  engine.script.messagesDelay = 300
  await $.turn.complete(mainTurn())
  engine.script.messages = () => [{ role: 'user', text: 'newer read' }]
  engine.script.messagesDelay = 0
  await $.turn.complete(mainTurn())
  await engine.clock.advance(400)
  await settle(engine)
  engine.script.messagesNever = true
  const ending = $.session.end({ reason: 'clear', sessionId: 'session-1' })
  await engine.clock.advance(500)
  await ending
  await settle(engine)
  const { texts } = await drawn($, engine)
  expect(texts).toContain('person │ newer read')
  expect(texts.some(t => t.includes('older read'))).toBe(false)
})

test('a read that settles after its bound never updates the tail', async ($: any, on: any) => {
  const engine = await started($, on)
  engine.script.messagesDelay = 6000
  await $.turn.complete(mainTurn())
  await engine.clock.advance(5000)
  await engine.clock.advance(1500)
  await settle(engine)
  engine.script.messagesNever = true
  const ending = $.session.end({ reason: 'clear', sessionId: 'session-1' })
  await engine.clock.advance(500)
  await ending
  await settle(engine)
  const { texts } = await drawn($, engine)
  expect(texts.some(t => t.startsWith('before /clear'))).toBe(false)
})

test('the last read at the clear is bounded at 500 ms, and the live tail stands in', async ($: any, on: any) => {
  const engine = await started($, on)
  await $.turn.complete(mainTurn())
  await settle(engine)
  engine.script.messagesNever = true
  const ending = $.session.end({ reason: 'clear', sessionId: 'session-1' })
  await engine.clock.advance(500)
  await ending
  await settle(engine)
  const { texts } = await drawn($, engine)
  expect(texts).toContain('person │ hand off now')
})

test('a SessionStart(clear) that runs while the end waits does not lose the live tail', async ($: any, on: any) => {
  const engine = await started($, on)
  await $.turn.complete(mainTurn())
  await settle(engine)
  engine.script.messagesNever = true
  const ending = $.session.end({ reason: 'clear', sessionId: 'session-1' })
  await $.classic.SessionStart({ source: 'clear' })
  await engine.clock.advance(500)
  await ending
  await settle(engine)
  const { texts } = await drawn($, engine)
  expect(texts).toContain('person │ hand off now')
})

test('two clears with no turn between keep the tail the first one froze', async ($: any, on: any) => {
  const engine = await started($, on)
  await $.turn.complete(mainTurn())
  await settle(engine)
  await clear($, engine)
  engine.script.sessionId = 'session-2'
  engine.script.messages = () => []
  await $.session.start(START)
  await settle(engine)
  await $.session.end({ reason: 'clear', sessionId: 'session-2' })
  await settle(engine)
  const { texts } = await drawn($, engine)
  expect(texts).toContain('person │ hand off now')
})

test('a later clear with something to show replaces the tail', async ($: any, on: any) => {
  const engine = await started($, on)
  await $.turn.complete(mainTurn())
  await settle(engine)
  await clear($, engine)
  engine.script.sessionId = 'session-2'
  engine.script.messages = () => [{ role: 'user', text: 'second conversation' }]
  await $.session.start(START)
  await $.turn.complete(mainTurn())
  await settle(engine)
  await $.session.end({ reason: 'clear', sessionId: 'session-2' })
  await settle(engine)
  const { texts } = await drawn($, engine)
  expect(texts).toContain('person │ second conversation')
  expect(texts).not.toContain('person │ hand off now')
})

test('[ hide ] removes the tail from the pane, the status line and the store, and a reload does not bring it back', async ($: any, on: any) => {
  const engine = await started($, on)
  await $.turn.complete(mainTurn())
  await settle(engine)
  await clear($, engine)
  const { ui } = await drawn($, engine)
  await ui.press({ key: 'hide' })
  await settle(engine)
  expect(engine.calls.statuses[engine.calls.statuses.length - 1] ?? '').not.toContain('before /clear')
  // The module starts the session again and reads its store: the hidden tail is not there.
  await $.session.start(START)
  await settle(engine)
  const { texts } = await drawn($, engine)
  expect(texts.some(t => t.startsWith('before /clear'))).toBe(false)
  expect(engine.calls.statuses[engine.calls.statuses.length - 1] ?? '').not.toContain('before /clear')
})

test('a reload restores the frozen tail from the plugin store, until it is hidden', async ($: any, on: any) => {
  const frozen = { endedSession: 'session-0', at: Date.parse('2026-10-06T11:00:00Z'), messages: [{ role: 'user', text: 'kept across a reload' }] }
  const engine = await started($, on, { [KEY]: frozen })
  const { texts, ui } = await drawn($, engine)
  expect(texts).toContain('person │ kept across a reload')
  await ui.press({ key: 'hide' })
  await settle(engine)
  expect((await drawn($, engine)).texts.some(t => t.startsWith('before /clear'))).toBe(false)
  await $.session.start(START)
  await settle(engine)
  expect((await drawn($, engine)).texts.some(t => t.startsWith('before /clear'))).toBe(false)
})

test('/relais-status counts the freezes and hides', async ($: any, on: any) => {
  const engine = await started($, on)
  await $.turn.complete(mainTurn())
  await settle(engine)
  await clear($, engine)
  const { ui } = await drawn($, engine)
  await ui.press({ key: 'hide' })
  await settle(engine)
  const text = (await $.command.run({ command: 'relais-status' })).text as string
  expect(text).toContain('tails: 1 frozen, 1 non-empty, 1 hidden (1 within 60 s)')
})
