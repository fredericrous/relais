// The pane's layout (direction A, the artboard): one column read top to
// bottom. Finished phases are one line each; the current phase is expanded
// with its live output, capped by the rows the pane gets.

import type { Fx } from './fx.ts'
import {
  type RunModel,
  type Step,
  attemptText,
  elapsedOf,
  formatCost,
  isLive,
  mmss,
  shortId,
} from './timeline.ts'

export type Row = { text: string; color?: string; dim?: boolean; bold?: boolean }

export type Room = { rows: number; columns: number; now: number }

const TITLE_WIDTH = 22
const MAX_AGENT_ROWS = 3
const MAX_STDERR_ROWS = 3
// The pane's smallest sensible height; below it the layout still draws, cut.
const MIN_ROWS = 6

const pad = (text: string, width: number) => (text.length >= width ? text + ' ' : text.padEnd(width))

const rule = (columns: number): Row => ({ text: '─'.repeat(Math.max(1, columns)), dim: true })

function stepRow(step: Step): Row {
  const time = step.endedAt !== undefined ? mmss(step.endedAt - step.startedAt) : ''
  const span = time && step.detail ? ` · ${time}` : time
  const body = `${pad(step.title, TITLE_WIDTH)}${step.detail}${span}`
  if (step.state === 'fail') return { text: `FAIL ${body}`, color: 'error', bold: true }
  if (step.state === 'active') return { text: `▸ ${body}`, bold: true }
  return { text: `✓ ${body}`, dim: true }
}

// Rows of a run that are not live output: header, steps, stderr, footer.
function frame(m: RunModel, room: Room): { head: Row[]; steps: Row[]; tail: Row[] } {
  const head: Row[] = [
    {
      // Before relais names the run, the plugin's placeholder key is no id.
      text: [
        'relais',
        m.run.startsWith('starting-') ? '' : `run ${shortId(m.run)}`,
        m.phase || 'starting',
        attemptText(m),
        mmss(elapsedOf(m, room.now)),
      ]
        .filter(Boolean)
        .join(' · '),
      bold: true,
    },
    rule(room.columns),
  ]
  const steps: Row[] =
    m.steps.length === 0 ? [{ text: '· waiting for the first event', dim: true }] : []
  if (isLive(m) && m.steps.length > 0) {
    // Placeholders for what is still to come, as the artboard shows them.
    const titles = m.steps.map(s => s.title)
    for (const pending of ['review', 'receipt']) {
      if (!titles.includes(pending)) steps.push({ text: `· ${pad(pending, TITLE_WIDTH)}—`, dim: true })
    }
  }
  const stderr = m.events.filter(e => e.kind === 'stderr').slice(-MAX_STDERR_ROWS)
  const tail: Row[] = []
  // The latest decision (repair, escalate, accept…) with its reason; every
  // one is in the timeline `/relais-status` prints.
  const decision = m.decisions[m.decisions.length - 1]
  if (decision) tail.push({ text: `decision ${decision.text}`, bold: true })
  tail.push(...stderr.map(e => ({ text: `stderr │ ${e.text}`, color: 'error' })))
  tail.push(rule(room.columns))
  // One row per agent: a repair continues the same agent under a new
  // dispatch, and it is still one agent (its latest status shown).
  const agents = agentRows(m)
  const shown = agents.slice(0, MAX_AGENT_ROWS)
  if (shown.length === 0) tail.push({ text: 'agents  none', dim: true })
  shown.forEach((a, i) => {
    const who = `${a.kind} ${shortId(a.id)}`
    const turns = a.dispatches > 1 ? ` · ${a.dispatches} dispatches` : ''
    tail.push({ text: `${i === 0 ? 'agents  ' : '        '}● ${pad(who, 16)}${a.status}${turns}` })
  })
  if (agents.length > shown.length) {
    tail.push({ text: `        +${agents.length - shown.length} more agents`, dim: true })
  }
  tail.push({ text: `cost    ${formatCost(m.cost)}` })
  if (m.outcome) {
    const receipt = m.outcome.receipt ? ` · receipt ${m.outcome.receipt}` : ''
    tail.push({
      text: `outcome ${m.outcome.state}${receipt}`,
      bold: true,
      color: m.outcome.state.startsWith('accepted') ? undefined : 'error',
    })
  }
  if (m.ledger) tail.push({ text: `ledger  ${m.ledger}`, dim: true })
  return { head, steps, tail }
}

type AgentLine = { id: string; kind: string; status: string; dispatches: number }

function agentRows(m: RunModel): AgentLine[] {
  const rows: AgentLine[] = []
  for (const a of m.agents) {
    const id = a.agentId ?? a.dispatch
    const known = a.agentId ? rows.find(r => r.id === id) : undefined
    if (known) {
      known.status = a.status
      known.dispatches += 1
    } else {
      rows.push({ id, kind: a.kind, status: a.status, dispatches: 1 })
    }
  }
  return rows
}

// The output a run shows, `cap` lines at most: the last ones, so the
// extra (earlier) lines are dropped, not wrapped.
function outputRows(m: RunModel, cap: number): Row[] {
  if (cap <= 0) return []
  const current = m.steps.find(s => s.state === 'active') ?? m.steps[m.steps.length - 1]
  const checks = current?.checks ?? []
  const lines = checks.length > 0 ? checks[checks.length - 1].output : []
  return lines.slice(-cap).map(text => ({ text: `  │ ${text}`, dim: true }))
}

export function layoutPane(runs: RunModel[], room: Room): Row[] {
  if (runs.length === 0) {
    return [{ text: 'relais · waiting for the first event', dim: true }]
  }
  const rows = Math.max(room.rows, MIN_ROWS)
  const frames = runs.map(m => ({ m, ...frame(m, room) }))
  const fixed = frames.reduce((n, f) => n + f.head.length + f.steps.length + f.m.steps.length + f.tail.length, 0)
  const withOutput = frames.filter(f => isLive(f.m) || f.m.steps.some(s => s.checks.length > 0)).length
  const cap = withOutput > 0 ? Math.floor(Math.max(0, rows - fixed) / withOutput) : 0
  const out: Row[] = []
  for (const f of frames) {
    out.push(...f.head)
    f.m.steps.forEach(step => {
      out.push(stepRow(step))
      if (step.state === 'active') out.push(...outputRows(f.m, cap))
    })
    // A run that ended failing keeps its failed check's output while rows
    // are spare; an accepted one does not (its failure was repaired).
    if (!isLive(f.m) && !f.m.outcome?.state.startsWith('accepted')) out.push(...outputRowsOfFailed(f.m, cap))
    out.push(...f.steps, ...f.tail)
  }
  return out
}

// A run that ended on a failing check shows that check's output under its
// failed step, which is no longer "active".
function outputRowsOfFailed(m: RunModel, cap: number): Row[] {
  const failed = m.steps.find(s => s.state === 'fail' && s.checks.some(c => c.exit !== 0 && c.exit !== undefined))
  if (!failed) return []
  const check = failed.checks.filter(c => c.exit !== 0)[0]
  return check ? check.output.slice(-cap).map(text => ({ text: `  │ ${text}`, color: 'error' as const })) : []
}

export type PaneState = { runs: RunModel[]; now: number }

// The tree a `Pane` render draws: one `Text` a row, each cut at the end.
export function paneTree(fx: Fx, e: any, state: PaneState | undefined) {
  const { Box, Text } = fx.ui.resolve(e)
  const room: Room = {
    rows: e.props?.scroll?.bodyRows ?? 24,
    columns: e.props?.bodyColumns ?? 80,
    now: state?.now ?? 0,
  }
  const rows = layoutPane(state?.runs ?? [], room)
  return h(
    Box,
    { flexDirection: 'column' },
    ...rows.map((row, i) =>
      h(
        Text,
        {
          key: `row:${i}`,
          color: row.color,
          dimColor: row.dim,
          bold: row.bold,
          wrap: 'truncate-end',
        },
        row.text,
      ),
    ),
  )
}
