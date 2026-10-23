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
