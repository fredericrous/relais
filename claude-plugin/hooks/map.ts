// The programme window the pane draws: a pure function of the JSON that
// `relais programme session --json` answers. The map logic (standings,
// dependents, the ready order) is relais's; this file only lays it out.

import type { Row } from './pane.ts'
import { cellsOf, widthOf } from './transcript.ts'
import { cutLineTo } from './timeline.ts'

export type TicketStatus = 'closed' | 'frontier' | 'blocked' | 'in_flight' | 'out_of_scope'

export type ProgrammeTicket = {
  id: string
  name: string
  status: TicketStatus
  blocked_by: string[]
  open_blockers: string[]
  dependents: string[]
  due: string | null
  held: boolean
}

export type ProgrammeCounts = {
  closed: number
  frontier: number
  blocked: number
  in_flight: number
  open: number
  out_of_scope: number
}

// A claimed ticket whose map was read.
export type ProgrammeMap = {
  repo: string
  slug: string
  ticket: string
  kind: string
  holder: 'self' | 'previous'
  counts: ProgrammeCounts
  tickets: ProgrammeTicket[]
  ready: string[]
}

// A claimed ticket whose map could not be read.
export type ProgrammeFailed = { repo: string; slug: string; ticket: string; kind: string; error: string }

export type ProgrammeView = ProgrammeMap | ProgrammeFailed

// The window at three heights: all of it, then head + current + counts, then
// the current ticket alone (what the pane keeps when the runs need the rows).
export type ProgrammeLayers = { full: Row[]; reduced: Row[]; current: Row[] }

export const MAX_DEPENDENTS = 4
const MAX_READY_ROWS = 2
const READY_INDENT = '       '

// `text` cut to `width` terminal cells, ending in `…` when cut.
export function cutToCells(text: string, width: number): string {
  const clean = cutLineTo(text, 10_000)
  if (widthOf(clean) <= width) return clean
  const chars = Array.from(clean)
  let cells = 0
  let out = ''
  for (const ch of chars) {
    const n = cellsOf(ch)
    if (cells + n > Math.max(1, width - 1)) break
    cells += n
    out += ch
  }
  return `${out}…`
}

const baseName = (path: string) => path.replace(/\/+$/, '').split('/').pop() || path

const isFailed = (view: ProgrammeView): view is ProgrammeFailed => 'error' in view

// `lead`, the name cut to what is left of `width`, then `tail`.
function named(lead: string, name: string, tail: string, width: number): string {
  const room = width - widthOf(lead) - widthOf(tail)
  const body = room >= 2 ? cutToCells(name, room) : ''
  return `${lead}${body}${tail}`
}

const dueText = (due: string | null) => (due ? ` (due ${due.slice(5)})` : '')

function readyRows(view: ProgrammeMap, width: number): Row[] {
  const byId = new Map(view.tickets.map(t => [t.id, t]))
  const items = view.ready.map(id => `${id}${dueText(byId.get(id)?.due ?? null)}`)
  if (items.length === 0) return []
  const rows: string[] = []
  const lead = () => (rows.length === 0 ? 'ready: ' : READY_INDENT)
  let row = ''
  let shown = 0
  for (const item of items) {
    const candidate = row === '' ? `${lead()}${item}` : `${row} · ${item}`
    if (row === '' || widthOf(candidate) <= width) {
      row = candidate
      shown += 1
    } else if (rows.length + 1 < MAX_READY_ROWS) {
      rows.push(row)
      row = `${lead()}${item}`
      shown += 1
    } else break
  }
  rows.push(row)
  const hidden = items.length - shown
  if (hidden > 0) rows[rows.length - 1] = cutToCells(`${rows[rows.length - 1]} · +${hidden}`, width)
  return rows.map(text => ({ text: cutToCells(text, width), dim: true }))
}

function dependentRow(ticket: ProgrammeTicket, connector: string, width: number): Row {
  const lead = `${connector} `
  switch (ticket.status) {
    case 'frontier':
      return { text: named(`${lead}○ ${ticket.id} `, ticket.name, '', width) }
    case 'blocked': {
      const needs = ticket.open_blockers.length > 0 ? ` (needs ${ticket.open_blockers.join(', ')})` : ''
      return { text: named(`${lead}· ${ticket.id} `, ticket.name, needs, width), dim: true }
    }
    case 'in_flight':
      return { text: named(`${lead}▶ ${ticket.id} `, ticket.name, ' (in flight)', width), bold: true }
    case 'closed':
      return { text: named(`${lead}✓ ${ticket.id} `, ticket.name, '', width), dim: true }
    case 'out_of_scope':
      return { text: named(`${lead}– ${ticket.id} `, ticket.name, ' (out of scope)', width), dim: true }
  }
}

function countsText(c: ProgrammeCounts): string {
  const parts = [`${c.open} open`, `${c.frontier} ready`, `${c.blocked} blocked`, `${c.in_flight} in flight`, `${c.closed} closed`]
  if (c.out_of_scope > 0) parts.push(`${c.out_of_scope} out of scope`)
  return parts.join(' · ')
}

export function programmeLayers(view: ProgrammeView, width: number): ProgrammeLayers {
  const columns = Math.max(8, width)
  if (isFailed(view)) {
    const row: Row = { text: cutToCells(`programme ${view.slug} · ${view.error}`, columns), dim: true }
    return { full: [row], reduced: [row], current: [row] }
  }
  const head: Row = {
    text: cutToCells(`programme ${view.slug} · ${baseName(view.repo)}`, columns),
    bold: true,
  }
  const byId = new Map(view.tickets.map(t => [t.id, t]))
  const current = byId.get(view.ticket)
  const marker = view.holder === 'previous' ? ' ◀ next' : ' ◀ you'
  const currentRow: Row = {
    text: named('▶ ', `${view.ticket} ${current?.name ?? ''}`.trim(), marker, columns),
    bold: true,
  }
  const counts: Row = { text: cutToCells(countsText(view.counts), columns), dim: true }
  const reduced = [head, currentRow, counts]

  const rows: Row[] = [head]
  if (view.counts.closed > 0) rows.push({ text: `✓ ${view.counts.closed} closed`, dim: true })
  const blocker = (current?.blocked_by ?? [])
    .map(id => byId.get(id))
    .filter((t): t is ProgrammeTicket => t !== undefined && t.status === 'closed')
    .pop()
  if (blocker) {
    rows.push({ text: named(`✓ ${blocker.id} `, blocker.name, '', columns), dim: true })
    rows.push({ text: '│', dim: true })
  }
  rows.push(currentRow)
  const dependents = (current?.dependents ?? [])
    .map(id => byId.get(id))
    .filter((t): t is ProgrammeTicket => t !== undefined)
  const shown = dependents.slice(0, MAX_DEPENDENTS)
  const more = dependents.length - shown.length
  shown.forEach((ticket, i) => {
    const isLast = i === shown.length - 1 && more === 0
    rows.push(dependentRow(ticket, isLast ? '└─' : '├─', columns))
  })
  if (more > 0) rows.push({ text: `└─ +${more} more`, dim: true })
  rows.push(...readyRows(view, columns))
  rows.push(counts)
  return { full: rows, reduced, current: [currentRow] }
}

export const programmeRows = (view: ProgrammeView, width: number): Row[] => programmeLayers(view, width).full

// The status line's share of a programme.
export const programmeStatus = (view: ProgrammeView): string => `▶ ${view.slug} ${view.ticket}`
