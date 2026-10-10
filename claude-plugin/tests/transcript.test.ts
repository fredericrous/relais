// transcriptRows: the pure layout of an agent's messages at the pane's width.

import { expect, test } from 'claude-code/testing'
import { refreshTranscript, transcriptRows } from '../hooks/transcript.ts'
import { createStore } from '../hooks/store.ts'

const lines = (n: number, prefix = 'line') => Array.from({ length: n }, (_, i) => `${prefix} ${i + 1}`).join('\n')
const user = (text: string) => ({ role: 'user', text, toolUses: [] })
const agent = (text: string, toolUses: any[] = []) => ({ role: 'assistant', text, toolUses })

test('rows come in message order, a bar on each, the speaker on the first row', () => {
  const rows = transcriptRows(
    [
      user('Migrate to pnpm'),
      agent('I will start by reading package.json.', [
        { tool: 'Read', input: { file_path: 'package.json' }, text: '{"name":"x"}' },
      ]),
    ],
    72,
    400,
  )
  expect(rows.map(r => r.text)).toEqual([
    'person │ Migrate to pnpm',
    'agent  │ I will start by reading package.json.',
    '▸ Read package.json',
    '  │ {"name":"x"}',
  ])
  expect(rows[3].dim).toBe(true)
})

test('no row is over 72 cells, and a long message wraps behind a bar', () => {
  const rows = transcriptRows([agent('word '.repeat(60)), agent('x'.repeat(500))], 72, 400)
  expect(rows.length).toBeGreaterThan(2)
  for (const row of rows) expect(Array.from(row.text).length).toBeLessThanOrEqual(72)
  expect(rows[1].text.startsWith('       │ ')).toBe(true)
})

test('a 30-line message is 12 rows then the count of the rest', () => {
  const rows = transcriptRows([user(lines(30))], 72, 400)
  expect(rows.length).toBe(13)
  expect(rows[12].text).toContain('… 18 more rows')
  expect(rows[12].dim).toBe(true)
})

test('a 10-line result is 3 rows then the count of the rest', () => {
  const rows = transcriptRows([agent('', [{ tool: 'Bash', input: { command: 'ls' }, text: lines(10, 'out') }])], 72, 400)
  expect(rows.map(r => r.text)).toEqual([
    '▸ Bash ls',
    '  │ out 1',
    '  │ out 2',
    '  │ out 3',
    '  │ … 7 more rows',
  ])
})

test('a long tool row ends in …', () => {
  const rows = transcriptRows([agent('', [{ tool: 'Bash', input: { command: 'echo ' + 'a'.repeat(300) }, text: 'ok' }])], 72, 400)
  expect(rows[0].text.endsWith('…')).toBe(true)
  expect(Array.from(rows[0].text).length).toBeLessThanOrEqual(72)
})

test('an error result starts FAIL in the error colour', () => {
  const rows = transcriptRows(
    [agent('', [{ tool: 'Bash', input: { command: 'false' }, text: 'boom\nmore', isError: true }])],
    72,
    400,
  )
  expect(rows[1].text.startsWith('FAIL ')).toBe(true)
  expect(rows[1].color).toBe('error')
  expect(rows[2].text).toBe('  │ more')
})

test('a tool use with no text yet is still running', () => {
  const rows = transcriptRows([agent('', [{ tool: 'Bash', input: { command: 'pnpm install' } }])], 72, 400)
  expect(rows.length).toBe(1)
  expect(rows[0].text.startsWith('▸ Bash pnpm install')).toBe(true)
  expect(rows[0].text.endsWith('… running')).toBe(true)
})

test('a message with no text and no tool use is skipped', () => {
  expect(transcriptRows([agent(''), user('  ')], 72, 400)).toEqual([])
})

test('escape sequences, carriage returns and tabs leave no byte under 0x20', () => {
  const rows = transcriptRows(
    [user('\u001b[31mred\u001b[0m\rtext\twith tab'), agent('', [{ tool: 'Bash', input: { command: 'a\tb\u001b[1m' }, text: 'x\ty\r' }])],
    72,
    400,
  )
  for (const row of rows) {
    for (const byte of new TextEncoder().encode(row.text)) expect(byte).toBeGreaterThanOrEqual(0x20)
  }
})

test('4096 messages give the newest 400 rows', () => {
  const messages = Array.from({ length: 4096 }, (_, i) => user(`message ${i}`))
  const rows = transcriptRows(messages, 72, 400)
  expect(rows.length).toBe(400)
  expect(rows[399].text).toContain('message 4095')
  expect(rows[0].text).toContain('message 3696')
})

const stateBytes = (rows: unknown[]) =>
  new TextEncoder().encode(
    JSON.stringify({ runs: [], now: 0, shown: { kind: 'agent', agentId: 'a', run: 'r', transcript: { kind: 'rows', rows } } }),
  ).length

test('1-cell 3-byte characters at 100 columns stay under the byte limit', () => {
  const messages = Array.from({ length: 4096 }, () => agent('€'.repeat(400)))
  const rows = transcriptRows(messages, 100, 400)
  expect(rows.length).toBe(400)
  expect(stateBytes(rows)).toBeLessThanOrEqual(130_000)
})

test('letters each followed by three combining marks stay under the byte limit', () => {
  const messages = Array.from({ length: 4096 }, () => agent('é̂̃'.repeat(300)))
  const rows = transcriptRows(messages, 100, 400)
  expect(stateBytes(rows)).toBeLessThanOrEqual(130_000)
})

test('all-dim rows stay under 140 000 bytes', () => {
  const messages = Array.from({ length: 4096 }, () =>
    agent('', [{ tool: 'Bash', input: { command: 'ls' }, text: Array.from({ length: 6 }, () => '€'.repeat(120)).join('\n') }]),
  )
  const rows = transcriptRows(messages, 100, 400)
  expect(rows.every(r => r.dim === true || r.text.startsWith('▸'))).toBe(true)
  expect(stateBytes(rows)).toBeLessThanOrEqual(140_000)
})

// Two pumps that overlap (the second starts while the first awaits the
// clock) make one read, not two.
test('two overlapping refreshes make one read', async () => {
  const store = createStore()
  store.shown = {
    kind: 'agent',
    agentId: 'agent-1',
    run: 'run-1',
    transcript: { kind: 'loading' },
    generation: 1,
    columns: 72,
    refreshedAt: 0,
    hasLanded: false,
    status: undefined,
  }
  let reads = 0
  let releaseClock: (value: number) => void = () => undefined
  const clock = new Promise<number>(resolve => {
    releaseClock = resolve
  })
  const never = new Promise(() => undefined)
  const fx: any = {
    clock: { now: () => clock, after: () => ({ cancel: () => undefined }), every: () => ({ cancel: () => undefined }) },
    session: { messages: () => ((reads += 1), never) },
    pane: { read: async () => ({ value: undefined }), write: async () => undefined },
    ui: { status: () => undefined },
  }
  const first = refreshTranscript(fx, store)
  const second = refreshTranscript(fx, store)
  releaseClock(1000)
  await Promise.all([first, second])
  expect(reads).toBe(1)
})
