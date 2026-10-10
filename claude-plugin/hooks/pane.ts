// The pane's layout (direction A, the artboard): one column read top to
// bottom. Finished phases are one line each; the current phase is expanded
// with its live output, capped by the rows the pane gets.

import type { Fx } from './fx.ts'
import type { TranscriptState } from './transcript.ts'
import { MAX_TRANSCRIPT_ROWS } from './limits.ts'
import {
  type RunModel,
  type Step,
  attemptText,
  elapsedOf,
  formatCost,
  isLive,
  mmss,
  outcomeText,
  shortId,
  stepOutput,
} from './timeline.ts'

// What pressing a row's Button does.
// What an agent row's press opens; `[ back ]` is drawn by `paneTree` itself.
export type Press = { kind: 'agent'; agentId: string; run: string }

export type Row = {
  text: string
  color?: string
  dim?: boolean
  bold?: boolean
  // A row drawn as a plain Button keyed `key`; `text` is its label.
  button?: { key: string; press: Press }
}

export type Room = { rows: number; columns: number; now: number }

const TITLE_WIDTH = 22
const MAX_AGENT_ROWS = 3
const MAX_STDERR_ROWS = 3
// A runs-view row is at most this many cells before the width cut.
const MAX_ROW = 70
const HINT = 'Click or Enter an agent: its transcript here · ↓ to manage: full view'
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
function frame(m: RunModel, room: Room, isLast: boolean): { head: Row[]; steps: Row[]; tail: Row[] } {
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
  // holds-until: a run whose fourth agent matters: its agents past the
  // third sit under `+n more agents`, not pressable, until that row is a list.
  const shown = agents.slice(0, MAX_AGENT_ROWS)
  if (shown.length === 0) tail.push({ text: 'agents  none', dim: true })
  let hasButton = false
  shown.forEach((a, i) => {
    const who = `${a.kind} ${shortId(a.id)}`
    const turns = a.dispatches > 1 ? ` · ${a.dispatches} dispatches` : ''
    const text = `${i === 0 ? 'agents  ' : '        '}● ${pad(who, 16)}${a.status}${turns}`
    if (a.agentId === undefined) {
      tail.push({ text })
      return
    }
    hasButton = true
    tail.push({ text, button: { key: `agent:${a.agentId}`, press: { kind: 'agent', agentId: a.agentId, run: m.run } } })
    if (a.type !== undefined) tail.push({ text: `          ${a.type}`.slice(0, MAX_ROW), dim: true })
  })
  if (agents.length > shown.length) {
    tail.push({ text: `        +${agents.length - shown.length} more agents`, dim: true })
  }
  if (isLast && hasButton) tail.push({ text: HINT, dim: true })
  tail.push({ text: `cost    ${formatCost(m.cost)}` })
  if (m.outcome) {

    tail.push({
      text: `outcome ${outcomeText(m.outcome)}`,
      bold: true,
      color: m.outcome.state.startsWith('accepted') ? undefined : 'error',
    })
  }
  if (m.ledger) tail.push({ text: `ledger  ${m.ledger}`, dim: true })
  if (m.session) tail.push({ text: `session ${m.session}`, dim: true })
  return { head, steps, tail }
}

type AgentLine = {
  id: string
  kind: string
  status: string
  dispatches: number
  agentId: string | undefined
  type: string | undefined
}

function agentRows(m: RunModel): AgentLine[] {
  const rows: AgentLine[] = []
  for (const a of m.agents) {
    const id = a.agentId ?? a.dispatch
    const known = a.agentId ? rows.find(r => r.id === id) : undefined
    if (known) {
      known.status = a.status
      known.dispatches += 1
      known.type = known.type ?? a.type
    } else {
      rows.push({ id, kind: a.kind, status: a.status, dispatches: 1, agentId: a.agentId, type: a.type })
    }
  }
  return rows
}

// The output a run shows, `cap` lines at most: the last ones, so the
// extra (earlier) lines are dropped, not wrapped. A step running no check
// shows what its agent does instead.
function outputRows(m: RunModel, cap: number): Row[] {
  if (cap <= 0) return []
  const current = m.steps.find(s => s.state === 'active') ?? m.steps[m.steps.length - 1]
  const lines = current ? stepOutput(current) : []
  return lines.slice(-cap).map(text => ({ text: `  │ ${text}`, dim: true }))
}

export function layoutPane(runs: RunModel[], room: Room): Row[] {
  if (runs.length === 0) {
    return [{ text: 'relais · waiting for the first event', dim: true }]
  }
  const rows = Math.max(room.rows, MIN_ROWS)
  const frames = runs.map((m, i) => ({ m, ...frame(m, room, i === runs.length - 1) }))
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

// What the pane shows besides the runs: one agent's transcript.
export type PaneShown = { kind: 'runs' } | { kind: 'agent'; agentId: string; run: string; transcript: TranscriptState }

export type PaneState = { runs: RunModel[]; now: number; shown?: PaneShown }

export type PaneActions = {
  openAgent: (agentId: string, run: string, columns: number) => unknown
  back: () => unknown
}

const BACK_HINT = '  Esc hands the keys back · ↓ to manage: full view'

// The agent view's rows under the `[ back ]` line: header, rule, transcript.
export function layoutAgentView(shown: Extract<PaneShown, { kind: 'agent' }>, runs: RunModel[], room: Room): Row[] {
  const m = runs.find(r => r.run === shown.run)
  const agent = m?.agents.find(a => a.agentId === shown.agentId)
  const model = agent ? `${agent.model}${agent.effort ? `@${agent.effort}` : ''}` : ''
  const header = [
    `run ${shortId(shown.run)}`,
    `${agent?.kind ?? 'agent'} ${shortId(shown.agentId)}`,
    agent?.status,
    model,
    m ? mmss(elapsedOf(m, room.now)) : '',
  ]
    .filter(Boolean)
    .join(' · ')
  const out: Row[] = [{ text: header, bold: true }, rule(room.columns)]
  const t = shown.transcript
  if (t.kind === 'loading') out.push({ text: 'loading', dim: true })
  else if (t.kind === 'deny') out.push({ text: `transcript unavailable · ${t.reason}`, dim: true })
  else if (t.kind === 'slow') out.push({ text: 'transcript slow · ↓ to manage: full view', dim: true })
  else {
    if (t.note) out.push({ text: t.note, dim: true })
    out.push(...t.rows)
  }
  return out
}

// The tree a `Pane` render draws: one `Text` a row, each cut at the end; the
// agent rows and `[ back ]` are Buttons.
export function paneTree(fx: Fx, e: any, state: PaneState | undefined, actions: PaneActions) {
  const { Box, Text, Button } = fx.ui.resolve(e)
  const columns = e.props?.bodyColumns ?? 80
  const room: Room = {
    rows: e.props?.scroll?.bodyRows ?? 24,
    columns,
    now: state?.now ?? 0,
  }
  const text = (row: Row, key: string) =>
    h(
      Text,
      { key, color: row.color, dimColor: row.dim, bold: row.bold, wrap: 'truncate-end' },
      row.text,
    )
  const shown = state?.shown
  if (shown?.kind === 'agent') {
    const rows = layoutAgentView(shown, state?.runs ?? [], room).slice(-ROWS_CAP)
    return h(
      Box,
      { flexDirection: 'column' },
      h(
        Box,
        { key: 'top', flexDirection: 'row' },
        h(Button, { key: 'back', autoFocus: true, onPress: () => actions.back() }, 'back'),
        h(Text, { key: 'keys', dimColor: true, wrap: 'truncate-end' }, BACK_HINT),
      ),
      ...rows.map((row, i) => text(row, `row:${i}`)),
    )
  }
  const rows = layoutPane(state?.runs ?? [], room)
  return h(
    Box,
    { flexDirection: 'column' },
    ...rows.map((row, i) => {
      const press = row.button?.press
      if (row.button && press?.kind === 'agent') {
        return h(
          Button,
          { key: row.button.key, plain: true, onPress: () => actions.openAgent(press.agentId, press.run, columns) },
          h(Text, { wrap: 'truncate-end' }, row.text),
        )
      }
      return text(row, `row:${i}`)
    }),
  )
}

// The header, the rule, the transcript's rows and the one note line.
const ROWS_CAP = MAX_TRANSCRIPT_ROWS + 3
