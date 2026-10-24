// The pane's layout (direction A, the artboard): one column read top to
// bottom. Finished phases are one line each; the current phase is expanded
// with its live output, capped by the rows the pane gets.

import type { Fx } from './fx.ts'
import type { TranscriptState } from './transcript.ts'
import { MAX_TRANSCRIPT_ROWS } from './limits.ts'
import { MAX_QUIET_LINES, quietElapsed, type QuietRun } from './quiet.ts'
import {
  type RunModel,
  type Step,
  attemptText,
  awaitsAPerson,
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

// The widest a row of prose may be: it wraps at a word boundary past this.
const WRAP_COLUMNS = 80

// `text` as rows of at most `width` cells, broken at spaces (a word longer
// than a row is cut), the continuation rows indented by `indent`.
export function wrapWords(text: string, width: number, indent: string): string[] {
  const rows: string[] = []
  let row = ''
  const flush = () => {
    if (row !== '') rows.push(row)
    row = ''
  }
  for (const word of text.split(/\s+/).filter(Boolean)) {
    const lead = rows.length === 0 ? '' : indent
    let rest = word
    // A word wider than a row is cut at the row's width.
    while (lead.length + rest.length > width) {
      flush()
      const cut = width - indent.length
      rows.push(`${rows.length === 0 ? '' : indent}${rest.slice(0, cut)}`)
      rest = rest.slice(cut)
    }
    if (row === '') row = `${rows.length === 0 ? '' : indent}${rest}`
    else if (row.length + 1 + rest.length <= width) row = `${row} ${rest}`
    else {
      flush()
      row = `${indent}${rest}`
    }
  }
  flush()
  return rows
}

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
// `meta` are the tail rows (ledger, session, hint) that go first when rows are short.
type Frame = { head: Row[]; steps: Row[]; tail: Row[]; meta: Set<Row> }

function frame(m: RunModel, room: Room, isLast: boolean): Frame {
  const meta = new Set<Row>()
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
  if (isLast && hasButton) {
    const hint: Row = { text: HINT, dim: true }
    meta.add(hint)
    tail.push(hint)
  }
  tail.push({ text: `cost    ${formatCost(m.cost)}` })
  if (m.wait && isLive(m)) tail.push({ text: m.wait, dim: true })
  if (m.stalled && isLive(m)) tail.push({ text: m.stalled, dim: true })
  if (m.outcome) {

    tail.push({
      text: `outcome ${outcomeText(m.outcome)}`,
      bold: true,
      color: m.outcome.state.startsWith('accepted') ? undefined : 'error',
    })
  }
  if (m.outcome?.reason) {
    for (const text of wrapWords(m.outcome.reason, WRAP_COLUMNS, '        ')) tail.push({ text, dim: true })
  }
  if (m.ledger) {
    const ledger: Row = { text: `ledger  ${m.ledger}`, dim: true }
    meta.add(ledger)
    tail.push(ledger)
  }
  // The full id is in `status --run`'s reply; the pane shows a prefix.
  if (m.session) {
    const session: Row = { text: `session ${m.session.slice(0, 8)}`, dim: true }
    meta.add(session)
    tail.push(session)
  }
  return { head, steps, tail, meta }
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
// shows what its agent does instead. A live run's active step always shows
// one row: its latest line, the activity row while its check runs, or a
// waiting row when it is silent and no check runs.
function outputRows(m: RunModel, cap: number): Row[] {
  if (cap < 0) return []
  const live = isLive(m)
  const n = live ? Math.max(cap, 1) : cap
  if (n <= 0) return []
  const current = m.steps.find(s => s.state === 'active') ?? m.steps[m.steps.length - 1]
  const lines = current ? stepOutput(current) : []
  if (lines.length === 0) return live && !runningCheck(current) ? [{ text: '  │ waiting for output', dim: true }] : []
  return lines.slice(-n).map(text => ({ text: `  │ ${text}`, dim: true }))
}

const SPINNER_FRAMES = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏']

const runningCheck = (step: Step | undefined) => {
  const check = step?.checks[step.checks.length - 1]
  return check?.running ? check : undefined
}

// The dim row after a running check's output: a spinner frame that advances
// with each one-second redraw, and how long ago the check last wrote.
function activityRow(m: RunModel, now: number): Row | undefined {
  const current = m.steps.find(s => s.state === 'active')
  const check = runningCheck(current)
  if (!check) return undefined
  const frame = SPINNER_FRAMES[Math.floor(now / 1000) % SPINNER_FRAMES.length]
  const since =
    check.lastOutputAt === undefined
      ? 'no output yet'
      : `last line ${Math.max(0, Math.floor((now - check.lastOutputAt) / 1000))}s ago`
  return { text: `  │ ${frame} running · ${since}`, dim: true }
}

// The quiet runs to draw (already of this session and past the anti-flash),
// live ones first, and whether the watcher is down.
export type QuietPane = { runs: QuietRun[]; down: boolean }

const MAX_QUIET_HEADS = 3
const NO_QUIET: QuietPane = { runs: [], down: false }

// The most finished runs listed one per row; older ones are one count row.
const MAX_FOLDED = 3
// The output a frame gets before the folded rows give way.
const OUTPUT_FLOOR = 8

// The pane never returns more than max(room.rows, MIN_ROWS) rows, quiet rows
// included. The trim order, followed in `relaisRows`: the output down to 8
// lines, then the folded rows to one `+N earlier runs` row, then the live
// frame's ledger, session and hint rows, then the output down to 1 row, then
// the tail's dim rows. Still too tall: the other focus runs shrink to their
// head (and outcome) row and the top is cut behind `↑ N rows above`. The kept
// frame's head, its agent Buttons, its latest output line and the activity
// row under it always stay.
export function layoutPane(runs: RunModel[], room: Room, quiet: QuietPane = NO_QUIET): Row[] {
  const down: Row[] = quiet.down ? [{ text: 'quiet watcher down', dim: true }] : []
  if (runs.length === 0 && quiet.runs.length === 0) {
    return [{ text: 'relais · waiting for the first event', dim: true }, ...down]
  }
  const limit = Math.max(room.rows, MIN_ROWS)
  // The relais frames come first; the quiet runs get only the rows they leave.
  const relais = relaisRows(runs, room, limit - down.length)
  const spare = Math.max(0, limit - down.length - relais.length)
  return [...relais, ...quietRows(quiet.runs, room, spare).slice(0, spare), ...down]
}

// A finished run on one row: dim when accepted, else `FAIL` in the error colour.
function foldedRow(m: RunModel, now: number): Row {
  const state = m.outcome?.state ?? 'ended'
  const awaits = awaitsAPerson(m) ? ' · awaits you' : ''
  const body = `run ${shortId(m.run)} · ${state} · ${formatCost(m.cost)} · ${mmss(elapsedOf(m, now))}${awaits}`
  return state.startsWith('accepted') ? { text: body, dim: true } : { text: `FAIL ${body}`, color: 'error' }
}

type Trim = { meta: boolean; dim: boolean }
type Drawn = { rows: Row[]; keep: Set<Row> }

// One expanded frame with `cap` output lines (-1: none); `keep` are the rows
// that never go: the head, the output's latest line, the activity row under it,
// the agent Buttons.
function drawFrame(f: Frame & { m: RunModel; now: number }, cap: number, trim: Trim): Drawn {
  const rows: Row[] = [...f.head]
  const keep = new Set<Row>([f.head[0]])
  for (const step of f.m.steps) {
    rows.push(stepRow(step))
    if (step.state === 'active') {
      const out = outputRows(f.m, cap)
      rows.push(...out)
      const activity = activityRow(f.m, f.now)
      if (activity) {
        rows.push(activity)
        keep.add(activity)
      }
      if (out.length > 0) keep.add(out[out.length - 1])
    }
  }
  // A run that ended failing keeps its failed check's output while rows
  // are spare; an accepted one does not (its failure was repaired).
  if (!isLive(f.m) && !f.m.outcome?.state.startsWith('accepted')) rows.push(...outputRowsOfFailed(f.m, cap))
  rows.push(...f.steps)
  for (const row of f.tail) {
    if (row.button) keep.add(row)
    else if (trim.dim && row.dim) continue
    else if (trim.meta && f.meta.has(row)) continue
    rows.push(row)
  }
  return { rows, keep }
}

// A frame cut to its head, and the outcome row of a run awaiting a person.
function minimalFrame(f: Frame & { m: RunModel }): Row[] {
  const outcome = awaitsAPerson(f.m) ? f.tail.find(r => r.text.startsWith('outcome ')) : undefined
  return outcome ? [f.head[0], outcome] : [f.head[0]]
}

// The lines a frame would draw if given all it wants.
function wantedLines(m: RunModel): number {
  const current = m.steps.find(s => s.state === 'active')
  const active = current ? stepOutput(current).length : 0
  const failed = m.steps.find(s => s.state === 'fail' && s.checks.some(c => c.exit !== 0 && c.exit !== undefined))
  const failedLines = !isLive(m) && failed ? (failed.checks.filter(c => c.exit !== 0)[0]?.output.length ?? 0) : 0
  return Math.max(isLive(m) && current ? 1 : 0, active, failedLines)
}

// The relais runs' rows, at most `limit` of them when they can be (see
// `layoutPane` for the trim order).
function relaisRows(runs: RunModel[], room: Room, limit: number): Row[] {
  if (runs.length === 0) return []
  const lastLive = runs.map(isLive).lastIndexOf(true)
  let focus = runs.flatMap((m, i) => (isLive(m) ? [i] : []))
  let awaiting = -1
  runs.forEach((m, i) => {
    if (awaitsAPerson(m)) awaiting = i
  })
  if (awaiting >= 0) focus.push(awaiting)
  // With no focus run the most recent one stays expanded: its outcome stays readable.
  if (focus.length === 0) focus = [runs.length - 1]
  focus = [...new Set(focus)].sort((a, b) => a - b)
  const keptIndex = lastLive >= 0 ? lastLive : focus[focus.length - 1]
  const folded = runs.filter((_, i) => !focus.includes(i))
  const frames = focus.map((i, n) => ({ m: runs[i], ...frame(runs[i], room, n === focus.length - 1), index: i, now: room.now }))
  const withOutput = frames.filter(f => isLive(f.m) || f.m.steps.some(s => s.checks.length > 0)).length
  const wanted = Math.max(0, ...frames.map(f => wantedLines(f.m)))

  const foldedRows = (collapsed: boolean): Row[] => {
    if (folded.length === 0) return []
    if (collapsed) return [{ text: `+${folded.length} earlier runs`, dim: true }]
    const shown = folded.slice(-MAX_FOLDED)
    const earlier = folded.length - shown.length
    return [
      ...(earlier > 0 ? [{ text: `+${earlier} earlier runs`, dim: true }] : []),
      ...shown.map(m => foldedRow(m, room.now)),
    ]
  }

  const stages: { collapsed: boolean; trim: Trim; floor: number }[] = [
    { collapsed: false, trim: { meta: false, dim: false }, floor: Math.min(OUTPUT_FLOOR, wanted) },
    { collapsed: true, trim: { meta: false, dim: false }, floor: Math.min(OUTPUT_FLOOR, wanted) },
    { collapsed: true, trim: { meta: true, dim: false }, floor: Math.min(OUTPUT_FLOOR, wanted) },
    { collapsed: true, trim: { meta: true, dim: false }, floor: Math.min(1, wanted) },
    { collapsed: true, trim: { meta: true, dim: true }, floor: Math.min(1, wanted) },
  ]
  for (const stage of stages) {
    const top = foldedRows(stage.collapsed)
    const fixed = top.length + frames.reduce((n, f) => n + drawFrame(f, -1, stage.trim).rows.length, 0)
    const cap = withOutput > 0 ? Math.floor(Math.max(0, limit - fixed) / withOutput) : 0
    if (cap < stage.floor) continue
    const drawn = frames.map(f => drawFrame(f, cap, stage.trim))
    const rows = [...top, ...drawn.flatMap(d => d.rows)]
    if (rows.length <= limit) return rows
  }

  // Still too tall: every focus run but the kept one shrinks to its head.
  const trim: Trim = { meta: true, dim: true }
  const top = foldedRows(true)
  const before: Row[] = [...top]
  const after: Row[] = []
  let kept: Drawn = { rows: [], keep: new Set() }
  for (const f of frames) {
    if (f.index === keptIndex) continue
    ;(f.index < keptIndex ? before : after).push(...minimalFrame(f))
  }
  const keptFrame = frames.find(f => f.index === keptIndex)!
  const others = before.length + after.length
  const base = drawFrame(keptFrame, -1, trim).rows.length
  kept = drawFrame(keptFrame, Math.max(0, limit - others - base), trim)
  let body = kept.rows
  const total = () => before.length + body.length + after.length
  let above = 0
  if (total() > limit) {
    const cut = Math.min(before.length, total() - limit + 1)
    before.splice(0, cut)
    above = cut
  }
  const indicator = above > 0 ? 1 : 0
  let over = total() + indicator - limit
  if (over > 0) {
    // Inside the kept frame, from the top, except what must stay.
    body = body.filter(row => {
      if (over <= 0 || kept.keep.has(row)) return true
      over -= 1
      return false
    })
  }
  if (over > 0) after.splice(after.length - Math.min(over, after.length))
  const head: Row[] = above > 0 ? [{ text: `↑ ${above} rows above`, dim: true }] : []
  return [...head, ...before, ...body, ...after]
}

// Quiet runs below every relais run: Text only, so the agent Buttons keep
// their keyboard order. At most MAX_QUIET_HEADS heads (the caller sorts live
// before kept failures), each with its share of the `spare` rows.
function quietRows(runs: QuietRun[], room: Room, spare: number): Row[] {
  const heads = runs.slice(0, MAX_QUIET_HEADS)
  const more = runs.length - heads.length
  const left = Math.max(0, spare - heads.length - (more > 0 ? 1 : 0))
  const each = heads.length === 0 ? 0 : Math.min(MAX_QUIET_LINES, Math.floor(left / heads.length))
  const out: Row[] = []
  for (const run of heads) {
    const text = `${run.tool} · ${run.check} · ${mmss(quietElapsed(run, room.now))}`
    out.push(run.failure ? { text, color: 'error' } : { text, dim: true })
    const lines = run.lines.slice(-each).map(l => ({ text: `  │ ${l}`, dim: true }))
    if (lines.length > 0) out.push(...lines)
    else if (each > 0 && !run.failure) out.push({ text: '  │ waiting for output', dim: true })
  }
  if (more > 0) out.push({ text: `+${more} more quiet runs`, dim: true })
  return out
}

// A run that ended on a failing check shows that check's output under its
// failed step, which is no longer "active".
function outputRowsOfFailed(m: RunModel, cap: number): Row[] {
  if (cap <= 0) return []
  const failed = m.steps.find(s => s.state === 'fail' && s.checks.some(c => c.exit !== 0 && c.exit !== undefined))
  if (!failed) return []
  const check = failed.checks.filter(c => c.exit !== 0)[0]
  return check ? check.output.slice(-cap).map(text => ({ text: `  │ ${text}`, color: 'error' as const })) : []
}

// What the pane shows besides the runs: one agent's transcript.
export type PaneShown = { kind: 'runs' } | { kind: 'agent'; agentId: string; run: string; transcript: TranscriptState }

export type PaneState = { runs: RunModel[]; now: number; shown?: PaneShown; quiet?: QuietPane }

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
  const rows = layoutPane(state?.runs ?? [], room, state?.quiet)
  return h(
    Box,
    { flexDirection: 'column' },
    ...rows.map((row, i) => {
      const press = row.button?.press
      if (row.button && press?.kind === 'agent') {
        return h(
          Button,
          // The text as `label`, not a Text child: Claude Code 2.1.291 (CI's
          // `CLAUDE_CODE_PIN`) refuses a Button with element children; 2.1.295
          // accepts it.
          {
            key: row.button.key,
            label: row.text,
            plain: true,
            onPress: () => actions.openAgent(press.agentId, press.run, columns),
          },
        )
      }
      return text(row, `row:${i}`)
    }),
  )
}

// The header, the rule, the transcript's rows and the one note line.
const ROWS_CAP = MAX_TRANSCRIPT_ROWS + 3
