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
// (2026-10-23): the change must leave this case exactly as it was.
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

test('one live run alone draws as it did before the fold', () => {
  expect(rowsOf([liveTimeline('run-65d322006dd13-c35c')], 40)).toEqual(ONE_LIVE_RUN)
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
