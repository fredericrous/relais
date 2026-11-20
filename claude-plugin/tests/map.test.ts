// The programme window: rows as data from the JSON relais answers.
// `goldenAnswer` is a copy of tests/fixtures/programme-session.golden.json, the
// file the Rust test pins `relais programme session --json` to. A plugin test
// cannot read a file that is not code (the runner loads .ts modules only), so
// a change to the golden file is copied here.

import { expect, test } from 'claude-code/testing'
import { programmeLayers, programmeRows, programmeStatus, type ProgrammeMap, type ProgrammeTicket } from '../hooks/map.ts'
import { widthOf } from '../hooks/transcript.ts'

const goldenAnswer = {
  "programme": {
    "counts": {
      "blocked": 1,
      "closed": 1,
      "frontier": 1,
      "in_flight": 1,
      "open": 3,
      "out_of_scope": 1
    },
    "holder": "self",
    "kind": "work",
    "ready": [
      "t4"
    ],
    "repo": "alpha",
    "slug": "p",
    "ticket": "t2",
    "tickets": [
      {
        "blocked_by": [],
        "dependents": [
          "t2"
        ],
        "due": null,
        "held": false,
        "id": "t1",
        "name": "First",
        "open_blockers": [],
        "status": "closed"
      },
      {
        "blocked_by": [
          "t1"
        ],
        "dependents": [
          "t3"
        ],
        "due": null,
        "held": true,
        "id": "t2",
        "name": "Second",
        "open_blockers": [],
        "status": "in_flight"
      },
      {
        "blocked_by": [
          "t2"
        ],
        "dependents": [],
        "due": null,
        "held": false,
        "id": "t3",
        "name": "Third",
        "open_blockers": [
          "t2"
        ],
        "status": "blocked"
      },
      {
        "blocked_by": [],
        "dependents": [],
        "due": "2026-12-01",
        "held": false,
        "id": "t4",
        "name": "Fourth",
        "open_blockers": [],
        "status": "frontier"
      },
      {
        "blocked_by": [],
        "dependents": [],
        "due": null,
        "held": false,
        "id": "t5",
        "name": "Fifth",
        "open_blockers": [],
        "status": "out_of_scope"
      }
    ]
  }
}

const golden = (): ProgrammeMap => structuredClone(goldenAnswer.programme) as ProgrammeMap

const texts = (view: ProgrammeMap, width = 80) => programmeRows(view, width).map(r => r.text)

const ticket = (id: string, status: ProgrammeTicket['status'], extra: Partial<ProgrammeTicket> = {}): ProgrammeTicket => ({
  id,
  name: `Name ${id}`,
  status,
  blocked_by: [],
  open_blockers: [],
  dependents: [],
  due: null,
  held: false,
  ...extra,
})

test('the golden answer draws the window around the current ticket', () => {
  expect(texts(golden())).toEqual([
    'programme p · alpha',
    '✓ 1 closed',
    '✓ t1 First',
    '│',
    '▶ t2 Second ◀ you',
    '└─ · t3 Third (needs t2)',
    'ready: t4 (due 12-01)',
    '3 open · 1 ready · 1 blocked · 1 in flight · 1 out of scope',
  ])
  expect(programmeStatus(golden())).toBe('▶ p t2')
})

test('a handoff of the previous session says next, not you', () => {
  const view = { ...golden(), holder: 'previous' as const }
  expect(texts(view)).toContain('▶ t2 Second ◀ next')
})

test('dependents carry ready, blocked and in flight glyphs; closed and out of scope never do', () => {
  const view: ProgrammeMap = {
    ...golden(),
    tickets: [
      ticket('c', 'in_flight', { dependents: ['a', 'b', 'd', 'e', 'f'] }),
      ticket('a', 'frontier', { blocked_by: ['c'] }),
      ticket('b', 'blocked', { blocked_by: ['c', 'x'], open_blockers: ['c', 'x'] }),
      ticket('d', 'in_flight', { blocked_by: ['c'] }),
      ticket('e', 'closed', { blocked_by: ['c'] }),
      ticket('f', 'out_of_scope', { blocked_by: ['c'] }),
    ],
    ticket: 'c',
    ready: [],
  }
  const rows = texts(view)
  expect(rows).toContain('├─ ○ a Name a')
  expect(rows).toContain('├─ · b Name b (needs c, x)')
  expect(rows).toContain('├─ ▶ d Name d (in flight)')
  expect(rows.find(r => r.startsWith('└─ ✓ e') || r.startsWith('├─ ✓ e'))).toBeDefined()
})

test('at most four dependents, then a +n more row', () => {
  const ids = ['a', 'b', 'c', 'd', 'e', 'f']
  const view: ProgrammeMap = {
    ...golden(),
    ticket: 'z',
    tickets: [ticket('z', 'in_flight', { dependents: ids }), ...ids.map(id => ticket(id, 'frontier', { blocked_by: ['z'] }))],
    ready: [],
  }
  const rows = texts(view)
  expect(rows.filter(r => r.startsWith('├─ ○')).length).toBe(4)
  expect(rows).toContain('└─ +2 more')
})

test('a closed or out-of-scope dependent gets neither the ready nor the active glyph', () => {
  const view: ProgrammeMap = {
    ...golden(),
    ticket: 'z',
    tickets: [
      ticket('z', 'in_flight', { dependents: ['a', 'b'] }),
      ticket('a', 'closed', { blocked_by: ['z'] }),
      ticket('b', 'out_of_scope', { blocked_by: ['z'] }),
    ],
    ready: [],
  }
  const rows = texts(view).filter(r => /^[├└]─/.test(r))
  expect(rows.length).toBe(2)
  for (const row of rows) expect(/[○▶]/.test(row)).toBe(false)
})

test('the ready list keeps relais order and wraps onto two rows at most', () => {
  const ids = ['t10', 't11', 't12', 't13', 't14', 't15', 't16', 't17', 't18']
  const view: ProgrammeMap = {
    ...golden(),
    ready: ids,
    tickets: [...golden().tickets, ...ids.map(id => ticket(id, 'frontier', { due: id === 't10' ? '2026-11-24' : null }))],
  }
  const rows = texts(view, 30)
  const at = rows.findIndex(r => r.startsWith('ready:'))
  expect(rows[at]).toBe('ready: t10 (due 11-24) · t11')
  expect(rows[at + 1].startsWith('       ')).toBe(true)
  expect(rows[at + 2].startsWith('       ')).toBe(false)
  expect(rows.slice(at, at + 2).every(r => widthOf(r) <= 30)).toBe(true)
})

test('an error-shape programme is one dim line', () => {
  const rows = programmeRows({ repo: 'alpha', slug: 'p', ticket: 't2', kind: 'work', error: 'repository gone' }, 60)
  expect(rows).toEqual([{ text: 'programme p · repository gone · relais programme session', dim: true }])
})

test('rows are cut in terminal cells, so wide text never passes the width', () => {
  const view: ProgrammeMap = {
    ...golden(),
    tickets: golden().tickets.map(t => (t.id === 't2' ? { ...t, name: '日本語の長い名前がここに続きます続きます' } : t)),
  }
  for (const width of [24, 40]) {
    for (const row of programmeRows(view, width)) expect(widthOf(row.text)).toBeLessThanOrEqual(width)
  }
  expect(texts(view, 24).find(r => r.startsWith('▶'))!.endsWith('◀ you')).toBe(true)
})

test('under pressure the window shrinks to head, current and counts, then to the current row', () => {
  const layers = programmeLayers(golden(), 80)
  expect(layers.reduced.map(r => r.text)).toEqual([
    'programme p · alpha',
    '▶ t2 Second ◀ you',
    '3 open · 1 ready · 1 blocked · 1 in flight · 1 closed · 1 out of scope',
  ])
  expect(layers.current.map(r => r.text)).toEqual(['▶ t2 Second ◀ you'])
})
