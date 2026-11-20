// The pane's layout at the `layoutPane` seam: rows as data, no mount.

import { expect, test } from 'claude-code/testing'
import { rebuildRuns } from '../../hooks/timeline.ts'
import { layoutPane } from '../../hooks/pane.ts'

const NOW = Date.parse('2026-10-06T12:10:00Z')
const at = (seq: number) => new Date(Date.parse('2026-10-06T12:00:00Z') + seq * 1000).toISOString()

// One run in verification: a worker attempt done, `make check` streaming.
function liveTimeline(run: string, lines = 30) {
  const ev = (seq: number, event: Record<string, unknown>) => ({ relais: 'event', run, seq, at: at(seq), event })
  const events = [
    ev(1, { kind: 'phase', state: 'running', reason: 'started', detail: {} }),
    ev(2, { kind: 'step', name: 'preflight', detail: 'route implementation · sonnet@medium' }),
    ev(3, { kind: 'dispatch_started', dispatch: 'disp-1', agent_kind: 'worker', attempt: 1, model: 'sonnet', effort: 'medium' }),
    ev(4, { kind: 'dispatch_ended', dispatch: 'disp-1', outcome: 'completed', agent: 'agent-1' }),
    ev(5, { kind: 'phase', state: 'verifying', reason: 'candidate_ready', detail: {} }),
    ev(6, { kind: 'check_started', label: 'make@1', argv: ['make', 'check'] }),
  ]
  for (let i = 0; i < lines; i++) events.push(ev(7 + i, { kind: 'output', label: 'make@1', text: `line ${i}\n`, elided_bytes: 0 }))
  return { run, revision: 1, events, current: { state: 'verifying' } }
}

const rowsOf = (timelines: any[], rows: number) =>
  layoutPane(
    Object.values(rebuildRuns({ runs: timelines }, NOW)).sort((a: any, b: any) => a.startedAt - b.startedAt) as any,
    { rows, columns: 72, now: NOW },
  )

// Today's rows for one live run alone, captured before the fold change
// (2026-10-23), plus the one dim activity row under the streaming output
// (2026-10-24): every other row is byte-identical to that capture. It is drawn
// in 41 rows, the 40 of the capture plus the activity row, which counts
// inside the bound.
const ONE_LIVE_RUN = [
  {
    "text": "relais · run run-…c35c · verifying · attempt 1 · 9:59",
    "bold": true
  },
  {
    "text": "────────────────────────────────────────────────────────────────────────",
    "dim": true
  },
  {
    "text": "✓ attempt 1             started · 0:01",
    "dim": true
  },
  {
    "text": "✓ attempt 1 · worker    sonnet@medium · 0:03",
    "dim": true
  },
  {
    "text": "▸ verification          make check",
    "bold": true
  },
  {
    "text": "  │ line 1",
    "dim": true
  },
  {
    "text": "  │ line 2",
    "dim": true
  },
  {
    "text": "  │ line 3",
    "dim": true
  },
  {
    "text": "  │ line 4",
    "dim": true
  },
  {
    "text": "  │ line 5",
    "dim": true
  },
  {
    "text": "  │ line 6",
    "dim": true
  },
  {
    "text": "  │ line 7",
    "dim": true
  },
  {
    "text": "  │ line 8",
    "dim": true
  },
  {
    "text": "  │ line 9",
    "dim": true
  },
  {
    "text": "  │ line 10",
    "dim": true
  },
  {
    "text": "  │ line 11",
    "dim": true
  },
  {
    "text": "  │ line 12",
    "dim": true
  },
  {
    "text": "  │ line 13",
    "dim": true
  },
  {
    "text": "  │ line 14",
    "dim": true
  },
  {
    "text": "  │ line 15",
    "dim": true
  },
  {
    "text": "  │ line 16",
    "dim": true
  },
  {
    "text": "  │ line 17",
    "dim": true
  },
  {
    "text": "  │ line 18",
    "dim": true
  },
  {
    "text": "  │ line 19",
    "dim": true
  },
  {
    "text": "  │ line 20",
    "dim": true
  },
  {
    "text": "  │ line 21",
    "dim": true
  },
  {
    "text": "  │ line 22",
    "dim": true
  },
  {
    "text": "  │ line 23",
    "dim": true
  },
  {
    "text": "  │ line 24",
    "dim": true
  },
  {
    "text": "  │ line 25",
    "dim": true
  },
  {
    "text": "  │ line 26",
    "dim": true
  },
  {
    "text": "  │ line 27",
    "dim": true
  },
  {
    "text": "  │ line 28",
    "dim": true
  },
  {
    "text": "  │ line 29",
    "dim": true
  },
  {
    "text": "  │ ⠋ running · last line 564s ago",
    "dim": true
  },
  {
    "text": "· review                —",
    "dim": true
  },
  {
    "text": "· receipt               —",
    "dim": true
  },
  {
    "text": "────────────────────────────────────────────────────────────────────────",
    "dim": true
  },
  {
    "text": "agents  ● worker agent-1  completed",
    "button": {
      "key": "agent:agent-1",
      "press": {
        "kind": "agent",
        "agentId": "agent-1",
        "run": "run-65d322006dd13-c35c"
      }
    }
  },
  {
    "text": "Click or Enter an agent: its transcript here · ↓ to manage: full view",
    "dim": true
  },
  {
    "text": "cost    unknown"
  }
]

test('one live run alone draws as before the fold, plus the activity row', () => {
  expect(rowsOf([liveTimeline('run-65d322006dd13-c35c')], 41)).toEqual(ONE_LIVE_RUN)
})

// A finished run (accepted or not) that started `offset` seconds after the base.
function doneTimeline(run: string, offset: number, state = 'accepted') {
  const ev = (seq: number, event: Record<string, unknown>) => ({
    relais: 'event',
    run,
    seq,
    at: at(offset + seq),
    event,
  })
  const events = [
    ev(1, { kind: 'phase', state: 'running', reason: 'started', detail: {} }),
    ev(2, { kind: 'outcome', state, receipt: null }),
  ]
  return { run, revision: 1, events, current: { state } }
}

// A live run that started `offset` seconds after the base, with `agents` workers.
function liveAt(run: string, offset: number, lines: number, agents = 1, stderr = 0, review = false) {
  const ev = (seq: number, event: Record<string, unknown>) => ({
    relais: 'event',
    run,
    seq,
    at: at(offset + seq),
    event,
  })
  const events: any[] = [ev(1, { kind: 'phase', state: 'running', reason: 'started', detail: {} })]
  for (let n = 1; n <= agents; n++) {
    events.push(ev(10 * n, { kind: 'dispatch_started', dispatch: `d${n}`, agent_kind: 'worker', attempt: n, model: 'sonnet', effort: 'medium' }))
    events.push(ev(10 * n + 1, { kind: 'dispatch_ended', dispatch: `d${n}`, outcome: 'completed', agent: `agent-${n}` }))
  }
  events.push(ev(100, { kind: 'phase', state: 'verifying', reason: 'candidate_ready', detail: {} }))
  if (review) events.push(ev(101, { kind: 'step', name: 'review', detail: 'strong tier' }))
  else events.push(ev(101, { kind: 'check_started', label: 'make@1', argv: ['make', 'check'] }))
  for (let i = 0; i < lines; i++) events.push(ev(102 + i, { kind: 'output', label: 'make@1', text: `line ${i}\n`, elided_bytes: 0 }))
  const base = 102 + lines
  return { run, revision: 1, events, current: { state: 'verifying' }, stderrLines: stderr, base }
}

const withStderr = (t: any, n: number) => {
  const events = [...t.events]
  for (let i = 0; i < n; i++) events.push({ relais: 'event', run: t.run, seq: 900 + i, at: t.events[0].at, event: { kind: 'stderr', text: `err ${i}` } })
  return { ...t, events }
}

const finished = (n: number) => Array.from({ length: n }, (_, i) => doneTimeline(`done-${i}`, i))
const quiet2 = {
  runs: [
    { id: 'q1', tool: 'cargo', check: 'test', startedAt: NOW - 5000, lines: ['a'], failure: false },
    { id: 'q2', tool: 'make', check: 'lint', startedAt: NOW - 4000, lines: ['b'], failure: false },
  ] as any,
  down: false,
}
const run = (timelines: any[], rows: number, q?: any) =>
  layoutPane(
    Object.values(rebuildRuns({ runs: timelines }, NOW)).sort((a: any, b: any) => a.startedAt - b.startedAt) as any,
    { rows, columns: 72, now: NOW },
    q,
  )
const outputs = (rows: any[]) => rows.filter(r => r.text.startsWith('  │ ')).length

test('20 finished runs and a live run mid-check: output first, the head drawn, 40 rows at most', () => {
  const rows = run([...finished(20), liveAt('live-1', 100, 40)], 40)
  expect(outputs(rows)).toBeGreaterThanOrEqual(20)
  expect(rows.some(r => r.text.startsWith('relais · run live-1'))).toBe(true)
  expect(rows.length).toBeLessThanOrEqual(40)
})

test('the same with two quiet runs stays within 40 rows', () => {
  const rows = run([...finished(20), liveAt('live-1', 100, 40)], 40, quiet2)
  expect(rows.length).toBeLessThanOrEqual(40)
})

test('the same at 24 rows keeps 8 output rows and folds the runs to one count row', () => {
  const rows = run([...finished(20), withStderr(liveAt('live-1', 100, 40, 3), 3)], 24)
  expect(outputs(rows)).toBeGreaterThanOrEqual(8)
  expect(rows.length).toBeLessThanOrEqual(24)
  const counts = rows.filter(r => r.text.includes('earlier runs'))
  expect(counts.map(r => r.text)).toEqual(['+20 earlier runs'])
  expect(rows.some(r => r.text.startsWith('run done-'))).toBe(false)
})

test('a live run with 3 agents and 3 stderr lines in a 12-row body keeps its head and Buttons', () => {
  const rows = run([withStderr(liveAt('live-1', 0, 30, 3), 3)], 12)
  expect(rows.length).toBeLessThanOrEqual(12)
  expect(rows.some(r => r.text.startsWith('relais · run live-1'))).toBe(true)
  expect(rows.filter(r => r.button).length).toBe(3)
})

test('a live run older than an awaiting one keeps its head and output, behind a rows-above row', () => {
  const rows = run(
    [...finished(3), liveAt('live-1', 100, 40, 3), doneTimeline('wait-1', 200, 'needs_review')],
    12,
  )
  expect(rows.length).toBeLessThanOrEqual(12)
  expect(rows[0].text).toMatch(/^↑ \d+ rows above$/)
  expect(rows[0].dim).toBe(true)
  expect(rows.some(r => r.text.startsWith('relais · run live-1'))).toBe(true)
  expect(outputs(rows)).toBeGreaterThanOrEqual(1)
})

const activityOf = (rows: { text: string }[]) => rows.filter(r => r.text.includes(' running · '))

test('at 40 rows the activity row is drawn and the bound holds', () => {
  const rows = rowsOf([liveTimeline('run-65d322006dd13-c35c')], 40)
  expect(rows.length).toBeLessThanOrEqual(40)
  expect(activityOf(rows)).toHaveLength(1)
  // The row the activity row needs is the earliest output line, nothing else.
  const first = ONE_LIVE_RUN.findIndex(r => r.text.startsWith('  │ line '))
  expect(rows).toEqual(ONE_LIVE_RUN.filter((_, i) => i !== first))
})

test('when rows are cut, the latest output line stays right above the activity row', () => {
  const rows = run([...finished(3), liveAt('live-1', 100, 40, 3), doneTimeline('wait-1', 200, 'needs_review')], 12)
  const index = rows.findIndex(r => r.text.includes(' running · '))
  expect(index).toBeGreaterThan(0)
  expect(rows[index - 1].text).toBe('  │ line 39')
  expect(rows.length).toBeLessThanOrEqual(12)
})

test('a running check draws one activity row right under its last output line', () => {
  const rows = rowsOf([liveTimeline('run-a', 5)], 40)
  const index = rows.findIndex(r => r.text === '  │ line 4')
  expect(activityOf(rows)).toHaveLength(1)
  expect(rows[index + 1].text).toBe('  │ ⠋ running · last line 589s ago')
  expect(rows[index + 1].dim).toBe(true)
})

test('the spinner frame advances with room.now one second later', () => {
  const draw = (now: number) =>
    layoutPane(Object.values(rebuildRuns({ runs: [liveTimeline('run-a', 5)] }, NOW)) as any, { rows: 40, columns: 72, now })
  expect(activityOf(draw(NOW))[0].text).toContain('⠋ running · last line 589s ago')
  expect(activityOf(draw(NOW + 1000))[0].text).toContain('⠙ running · last line 590s ago')
})

test('a running check with no output yet says so, in one row', () => {
  const rows = rowsOf([liveTimeline('run-a', 0)], 40)
  expect(activityOf(rows).map(r => r.text)).toEqual(['  │ ⠋ running · no output yet'])
  expect(rows.some(r => r.text === '  │ waiting for output')).toBe(false)
})

test('a finished check draws no activity row', () => {
  const t = liveTimeline('run-a', 3)
  t.events.push({ relais: 'event', run: 'run-a', seq: 99, at: at(99), event: { kind: 'check_ended', label: 'make@1', exit: 0, duration_ms: 1 } })
  expect(activityOf(rowsOf([t], 40))).toHaveLength(0)
})

test('the activity row counts inside the row bound', () => {
  for (const bound of [12, 40]) {
    const rows = rowsOf([liveTimeline('run-a', 60)], bound)
    expect(rows.length).toBeLessThanOrEqual(bound)
    expect(activityOf(rows)).toHaveLength(1)
  }
})

test('a live run in review with no output shows a waiting row', () => {
  const rows = run([liveAt('live-1', 0, 0, 1, 0, true)], 30)
  expect(rows.some(r => r.text === '  │ waiting for output')).toBe(true)
})

test('a folded failed run starts with FAIL in the error colour; an accepted one is dim', () => {
  const rows = run([doneTimeline('bad-1', 0, 'failed'), doneTimeline('good-1', 1), liveAt('live-1', 100, 3)], 40)
  const bad = rows.find(r => r.text.includes('run bad-1'))!
  expect(bad.text.startsWith('FAIL')).toBe(true)
  expect(bad.color).toBe('error')
  const good = rows.find(r => r.text.includes('run good-1'))!
  expect(good.dim).toBe(true)
  expect(good.text.startsWith('FAIL')).toBe(false)
})

const slotEvent = (run: string, seq: number) => ({
  relais: 'event',
  run,
  seq,
  at: at(seq),
  event: { kind: 'waiting_for_slot', class: 'heavy_local', position: 1, waited_s: 1260 },
})
const WAIT_TEXT = 'waiting for a heavy local slot, position 1 · 21m'
const waitRowsOf = (rows: { text: string }[]) => rows.filter(r => r.text.includes(WAIT_TEXT))

test('a live run waiting for a slot draws a spinner row with the wait text, once', () => {
  const t = liveTimeline('run-w', 3)
  t.events = t.events.slice(0, 2)
  t.events.push(slotEvent('run-w', 50))
  const rows = rowsOf([t], 40)
  expect(waitRowsOf(rows).map(r => r.text)).toEqual([`  │ ⠋ ${WAIT_TEXT}`])
  expect(rows.some(r => r.text === '  │ waiting for output')).toBe(false)
})

test('a waiting_for_slot event before any phase event draws the wait row, no waiting for output', () => {
  const t = { run: 'run-e', revision: 1, events: [slotEvent('run-e', 1)], current: { state: 'running' } }
  const rows = rowsOf([t], 40)
  expect(waitRowsOf(rows)).toHaveLength(1)
  expect(rows.some(r => r.text === '  │ waiting for output')).toBe(false)
  expect(rows.findIndex(r => r.text.includes(WAIT_TEXT))).toBe(2)
})

test('a running check still draws running · last line Ns ago', () => {
  const t = liveTimeline('run-c', 5)
  t.events.push(slotEvent('run-c', 60))
  const rows = rowsOf([t], 40)
  expect(activityOf(rows)).toHaveLength(1)
  expect(activityOf(rows)[0].text).toContain('running · ')
  expect(waitRowsOf(rows)).toHaveLength(1)
})

test('two live runs in 12 rows: the shrunk frame keeps its head and wait row', () => {
  const waiting = liveAt('live-a', 0, 0, 1, 0, true)
  waiting.events.push(slotEvent('live-a', 500))
  const rows = run([waiting, liveAt('live-b', 100, 40)], 12)
  expect(rows.length).toBeLessThanOrEqual(12)
  const head = rows.findIndex(r => r.text.startsWith('relais · run live-a'))
  expect(head).toBeGreaterThanOrEqual(0)
  expect(rows[head + 1].text).toBe(`  │ ⠋ ${WAIT_TEXT}`)
})

// The programme and tail block, after the quiet rows.
const runsOf = (timelines: any[]) =>
  Object.values(rebuildRuns({ runs: timelines }, NOW)).sort((a: any, b: any) => a.startedAt - b.startedAt) as any
const programme = {
  full: [{ text: 'programme p · alpha', bold: true }, { text: '▶ t2 Second ◀ you', bold: true }, { text: 'ready: t4' }, { text: '3 open', dim: true }],
  reduced: [{ text: 'programme p · alpha', bold: true }, { text: '▶ t2 Second ◀ you', bold: true }, { text: '3 open', dim: true }],
  current: [{ text: '▶ t2 Second ◀ you', bold: true }],
}
const tail = (count: number) => ({
  header: { text: 'before /clear · 12:00 [ hide ]', button: { key: 'hide', press: { kind: 'hide' as const } } },
  rows: Array.from({ length: count }, (_, i) => ({ text: `person │ message ${i}` })),
})
const room = (rows: number) => ({ rows, columns: 72, now: NOW })

test('no claim and no tail add zero rows, with runs and without', () => {
  const timelines = [liveAt('live-a', 0, 20), doneTimeline('done-a', 1)]
  for (const rows of [6, 12, 40]) {
    expect(layoutPane(runsOf(timelines), room(rows), undefined, {})).toEqual(layoutPane(runsOf(timelines), room(rows)))
  }
  expect(layoutPane([], room(12), undefined, {})).toEqual(layoutPane([], room(12)))
})

test('a programme draws with no run, in place of the waiting row', () => {
  const rows = layoutPane([], room(24), undefined, { programme })
  expect(rows.map(r => r.text)).toEqual(programme.full.map(r => r.text))
})

test('with room to spare the programme draws whole, after the runs', () => {
  const rows = layoutPane(runsOf([doneTimeline('done-a', 0)]), room(40), undefined, { programme })
  expect(rows.slice(-4).map(r => r.text)).toEqual(programme.full.map(r => r.text))
})

test('6 rows of room with 3 runs: the programme is reduced to its current row', () => {
  const timelines = [liveAt('live-a', 0, 20), liveAt('live-b', 1, 20), liveAt('live-c', 2, 20)]
  const rows = layoutPane(runsOf(timelines), room(6), undefined, { programme })
  expect(rows[rows.length - 1].text).toBe('▶ t2 Second ◀ you')
  expect(rows.filter(r => r.text.startsWith('programme')).length).toBe(0)
})

test('the programme shrinks to head, current and counts before it shrinks to the current row', () => {
  const timelines = [liveAt('live-a', 0, 0)]
  let sizes: number[] = []
  for (let rows = 6; rows <= 40; rows += 1) {
    const out = layoutPane(runsOf(timelines), room(rows), undefined, { programme })
    sizes = [...sizes, out.filter(r => programme.full.some(p => p.text === r.text)).length]
  }
  expect(sizes.includes(1)).toBe(true)
  expect(sizes.includes(3)).toBe(true)
  expect(sizes[sizes.length - 1]).toBe(4)
  expect(sizes).toEqual([...sizes].sort((a, b) => a - b))
})

test('the tail always keeps its header row and its newest rows, behind the programme', () => {
  const timelines = [liveAt('live-a', 0, 20), liveAt('live-b', 1, 20), liveAt('live-c', 2, 20)]
  const squeezed = layoutPane(runsOf(timelines), room(6), undefined, { programme, tail: tail(10) })
  expect(squeezed.map(r => r.text)).toContain('before /clear · 12:00 [ hide ]')
  expect(squeezed.map(r => r.text)).toContain('▶ t2 Second ◀ you')
  // The current row and the header are never given up, and the pane never passes its rows.
  expect(squeezed.length).toBeLessThanOrEqual(6)
  // Plain rows go before the agent Buttons: the live run keeps its agent row.
  expect(squeezed.some(r => r.button !== undefined && r.button.key !== 'hide')).toBe(true)
  const roomy = layoutPane([], room(12), undefined, { tail: tail(30) })
  expect(roomy[0].text).toBe('before /clear · 12:00 [ hide ]')
  expect(roomy.length).toBe(12)
  expect(roomy[roomy.length - 1].text).toBe('person │ message 29')
})

test('the runs keep their priority: a live run keeps its head and agents with a programme and a tail', () => {
  const timelines = [liveAt('live-a', 0, 20)]
  const without = layoutPane(runsOf(timelines), room(24))
  const withBlock = layoutPane(runsOf(timelines), room(24), undefined, { programme, tail: tail(5) })
  expect(withBlock.length).toBeLessThanOrEqual(24)
  expect(withBlock.slice(0, 3)).toEqual(without.slice(0, 3))
})
