// The timeline model: what a run's event lines add up to. Pure; the stream
// handler folds each line into its run's model, the pane and the status line
// read it.

export const MAX_EVENTS = 200
export const MAX_OUTPUT_LINES = 40
export const MAX_STDERR_LINES = 10
// A line is cut to this before it is stored, whatever width the pane gets.
export const MAX_LINE = 240

export type Check = {
  label: string
  argv: string[]
  running: boolean
  exit: number | null | undefined
  durationMs: number | undefined
  output: string[]
  // Whether the last stored output line is still being written.
  isOpen: boolean
  elidedBytes: number
}

export type Step = {
  title: string
  phase: string
  state: 'active' | 'done' | 'fail'
  detail: string
  startedAt: number
  endedAt: number | undefined
  checks: Check[]
}

export type AgentRow = {
  dispatch: string
  kind: string
  attempt: number | null
  model: string
  effort: string | null
  agentId: string | undefined
  status: string
}

export type Entry = { at: number; kind: string; text: string }

export type Cost = { booked: number | null; completeness: string }

export type RunModel = {
  run: string
  startedAt: number
  endedAt: number | undefined
  phase: string
  attempt: number
  maxAttempts: number | undefined
  steps: Step[]
  agents: AgentRow[]
  decisions: Entry[]
  cost: Cost | undefined
  outcome: { state: string; receipt: string | null } | undefined
  events: Entry[]
  ledger: string | undefined
  // What the next worker attempt is, after a `repairing`/`escalating`
  // transition: its step is titled by it.
  retry: 'repair' | 'escalation' | undefined
}

export const emptyRun = (run: string, startedAt: number): RunModel => ({
  run,
  startedAt,
  endedAt: undefined,
  phase: '',
  attempt: 0,
  maxAttempts: undefined,
  steps: [],
  agents: [],
  decisions: [],
  cost: undefined,
  outcome: undefined,
  events: [],
  ledger: undefined,
  retry: undefined,
})

const FAILED_STATES = ['failed', 'blocked', 'cancelled', 'interrupted', 'budget_exhausted']
const isFailing = (state: string) => FAILED_STATES.includes(state)

export const isLive = (m: RunModel) => m.outcome === undefined

// Control characters out, the line cut: a stored line is never wider than MAX_LINE.
export function cutLine(line: string): string {
  const clean = line
    // eslint-disable-next-line no-control-regex
    .replace(/\u001b\[[0-9;?]*[ -/]*[@-~]/g, '')
    // eslint-disable-next-line no-control-regex
    .replace(/[\u0000-\u0008\u000b-\u001f\u007f]/g, '')
    .replace(/\t/g, '  ')
  return clean.length > MAX_LINE ? clean.slice(0, MAX_LINE - 1) + '…' : clean
}

// The output lines of `text` appended to a check's tail, which keeps only
// the last MAX_OUTPUT_LINES.
export function appendOutput(check: Check, text: string): Check {
  const pieces = text.split('\n')
  const endsLine = pieces[pieces.length - 1] === ''
  if (endsLine) pieces.pop()
  const lines = [...check.output]
  pieces.forEach((piece, i) => {
    if (i === 0 && check.isOpen && lines.length > 0) {
      lines[lines.length - 1] = cutLine(lines[lines.length - 1] + piece)
    } else {
      lines.push(cutLine(piece))
    }
  })
  const isOpen = pieces.length > 0 ? !endsLine : check.isOpen && !endsLine
  return { ...check, output: lines.slice(-MAX_OUTPUT_LINES), isOpen }
}

const pushEntry = (list: Entry[], entry: Entry): Entry[] => [...list, entry].slice(-MAX_EVENTS)

function titleOfState(state: string, attempt: number): string {
  switch (state) {
    case 'prepared':
      return 'preflight'
    case 'running':
      return `attempt ${attempt || 1}`
    case 'verifying':
      return 'verification'
    case 'repairing':
      return 'repair'
    case 'escalating':
      return 'escalation'
    default:
      return state.replace(/_/g, ' ')
  }
}

const lastStep = (m: RunModel): Step | undefined => m.steps[m.steps.length - 1]

const replaceLast = (steps: Step[], step: Step): Step[] => [...steps.slice(0, -1), step]

function closeStep(m: RunModel, at: number, state: 'done' | 'fail'): RunModel {
  const step = lastStep(m)
  if (!step || step.state !== 'active') return m
  // A verification whose check failed is a failed step, whatever comes
  // next (a repair, an escalation); a red baseline is expected and is not.
  const checkFailed = step.phase === 'verifying' && step.checks.some(c => c.exit !== 0 && c.exit !== undefined)
  return { ...m, steps: replaceLast(m.steps, { ...step, state: checkFailed ? 'fail' : state, endedAt: at }) }
}

function openStep(m: RunModel, title: string, phase: string, at: number): RunModel {
  const step: Step = {
    title,
    phase,
    state: 'active',
    detail: '',
    startedAt: at,
    endedAt: undefined,
    checks: [],
  }
  return { ...m, steps: [...m.steps, step] }
}

function mapLast(m: RunModel, fn: (s: Step) => Step): RunModel {
  const step = lastStep(m)
  return step ? { ...m, steps: replaceLast(m.steps, fn(step)) } : m
}

function lastIndexOfLabel(checks: Check[], label: string): number {
  for (let i = checks.length - 1; i >= 0; i--) if (checks[i].label === label) return i
  return -1
}

function mapCheck(m: RunModel, label: string, fn: (c: Check) => Check): RunModel {
  return mapLast(m, s => ({
    ...s,
    checks: s.checks.map((c, i) => (c.label === label && i === lastIndexOfLabel(s.checks, label) ? fn(c) : c)),
  }))
}

const maxAttemptsOf = (detail: any): number | undefined => {
  const value = detail?.max_attempts ?? detail?.attempts?.max ?? detail?.budget?.max_attempts
  return typeof value === 'number' ? value : undefined
}

const agentStatusOf = (outcome: string) =>
  outcome === 'completed' || outcome === 'ok' || outcome === 'exit 0' ? 'completed' : outcome

// One event line (`{relais:'event', run, at, event}`) folded into the model.
export function applyEvent(model: RunModel, event: any, at: number): RunModel {
  let m = model
  switch (event.kind) {
    case 'phase': {
      const attempt = m.attempt
      m = closeStep(m, at, isFailing(event.state) ? 'fail' : 'done')
      m = { ...m, phase: event.state, maxAttempts: maxAttemptsOf(event.detail) ?? m.maxAttempts }
      const isTerminal = m.outcome !== undefined
      // A repair or an escalation is the next attempt's kind, not a step:
      // the decision row says why, the next attempt's title says what.
      if (event.state === 'repairing' || event.state === 'escalating') {
        m = { ...m, retry: event.state === 'repairing' ? 'repair' : 'escalation' }
      } else if (!isTerminal && !FINAL_STATES.includes(event.state)) {
        m = openStep(m, titleOfState(event.state, attempt + 1), event.state, at)
        m = mapLast(m, s => ({ ...s, detail: event.reason ?? '' }))
      }
      return withEntry(m, at, 'phase', `${event.state}${event.reason ? ` · ${event.reason}` : ''}`)
    }
    case 'step': {
      // A step that is not a transition (preflight, baseline, the task
      // worktree, review, receipt); the same name again updates its detail.
      const current = lastStep(m)
      m = { ...m, maxAttempts: typeof event.max_attempts === 'number' ? event.max_attempts : m.maxAttempts }
      if (current && current.state === 'active' && current.title === event.name) {
        m = mapLast(m, s => ({ ...s, detail: event.detail || s.detail }))
      } else {
        m = closeStep(m, at, 'done')
        m = openStep(m, event.name, m.phase, at)
        m = mapLast(m, s => ({ ...s, detail: event.detail ?? '' }))
      }
      return withEntry(m, at, 'step', `${event.name}${event.detail ? ` · ${event.detail}` : ''}`)
    }
    case 'dispatch_started': {
      const attempt = typeof event.attempt === 'number' ? event.attempt : m.attempt
      const row: AgentRow = {
        dispatch: event.dispatch,
        kind: event.agent_kind,
        attempt: event.attempt ?? null,
        model: event.model,
        effort: event.effort ?? null,
        agentId: undefined,
        status: 'running',
      }
      const route = `${event.model}${event.effort ? `@${event.effort}` : ''}`
      m = { ...m, attempt, agents: [...m.agents, row], maxAttempts: maxAttemptsOf(event) ?? m.maxAttempts }
      if (event.agent_kind === 'worker') {
        const kind = m.retry ?? 'worker'
        m = mapLast(m, s => ({ ...s, title: `attempt ${attempt} · ${kind}`, detail: route }))
        m = { ...m, retry: undefined }
      } else {
        m = mapLast(m, s => ({ ...s, detail: `${event.agent_kind} · ${route}` }))
      }
      return withEntry(m, at, 'dispatch', `${event.agent_kind} ${route}`)
    }
    case 'dispatch_ended': {
      const status = agentStatusOf(event.outcome)
      const agentId = typeof event.agent === 'string' ? event.agent : undefined
      m = {
        ...m,
        agents: m.agents.map(a =>
          a.dispatch === event.dispatch ? { ...a, status, agentId: a.agentId ?? agentId } : a,
        ),
      }
      return withEntry(m, at, 'dispatch', `ended · ${event.outcome}`)
    }
    case 'cost': {
      const prior = m.cost
      const booked =
        event.booked === null || event.booked === undefined || (prior && prior.booked === null)
          ? null
          : (prior?.booked ?? 0) + event.booked
      const completeness = worseCompleteness(prior?.completeness, event.completeness)
      return withEntry({ ...m, cost: { booked, completeness } }, at, 'cost', formatCost({ booked, completeness }))
    }
    case 'check_started': {
      const check: Check = {
        label: event.label,
        argv: event.argv ?? [],
        running: true,
        exit: undefined,
        durationMs: undefined,
        output: [],
        isOpen: false,
        elidedBytes: 0,
      }
      m = mapLast(m, s => ({ ...s, checks: [...s.checks, check], detail: (event.argv ?? []).join(' ') }))
      return withEntry(m, at, 'check', `start · ${event.label}`)
    }
    case 'output': {
      const elided = typeof event.elided_bytes === 'number' ? event.elided_bytes : 0
      return mapCheck(m, event.label, c => ({
        ...appendOutput(c, event.text ?? ''),
        elidedBytes: Math.max(c.elidedBytes, elided),
      }))
    }
    case 'check_ended': {
      m = mapCheck(m, event.label, c => ({
        ...c,
        running: false,
        exit: event.exit,
        durationMs: event.duration_ms,
      }))
      const failed = event.exit !== 0
      return withEntry(m, at, 'check', `${failed ? 'FAIL ' : ''}${event.label} · exit ${event.exit ?? 'none'}`)
    }
    case 'decision': {
      const entry = { at, kind: 'decision', text: `${event.what} · ${event.reason}` }
      return withEntry({ ...m, decisions: pushEntry(m.decisions, entry) }, at, 'decision', entry.text)
    }
    case 'outcome': {
      m = closeStep(m, at, isFailing(event.state) ? 'fail' : 'done')
      m = { ...m, phase: event.state, endedAt: at, outcome: { state: event.state, receipt: event.receipt ?? null } }
      return withEntry(m, at, 'outcome', `${event.state}${event.receipt ? ` · ${event.receipt}` : ''}`)
    }
    case 'stderr':
      return withEntry(m, at, 'stderr', event.text ?? '')
    default:
      return m
  }
}

// States where the run is over: the `outcome` event follows and names them.
const FINAL_STATES = ['accepted', 'accepted_by_person', 'needs_review', 'needs_decision', ...FAILED_STATES]

const withEntry = (m: RunModel, at: number, kind: string, text: string): RunModel => ({
  ...m,
  events: pushEntry(m.events, { at, kind, text: cutLine(text) }),
})

const ORDER = ['actual', 'estimated', 'incomplete_lower_bound', 'unknown']
function worseCompleteness(a: string | undefined, b: string): string {
  if (a === undefined) return b
  return ORDER.indexOf(a) >= ORDER.indexOf(b) ? a : b
}

export const isoMs = (at: unknown, fallback: number): number => {
  const t = typeof at === 'string' ? Date.parse(at) : NaN
  return Number.isNaN(t) ? fallback : t
}

// Folds one protocol line into the models, creating the run's on first sight.
export function applyLine(
  models: Record<string, RunModel>,
  line: { run?: string; at?: string; event?: unknown },
  now: number,
): Record<string, RunModel> {
  if (typeof line.run !== 'string' || typeof line.event !== 'object' || line.event === null) return models
  const at = isoMs(line.at, now)
  const model = models[line.run] ?? emptyRun(line.run, at)
  return { ...models, [line.run]: applyEvent(model, line.event, at) }
}

export function mmss(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000))
  const minutes = Math.floor(total / 60)
  const seconds = String(total % 60).padStart(2, '0')
  return `${minutes}:${seconds}`
}

export function elapsedOf(m: RunModel, now: number): number {
  return (m.endedAt ?? now) - m.startedAt
}

// Micro-USD as the pane shows it: cents under a dollar, dollars above.
export function formatCost(cost: Cost | undefined): string {
  if (!cost || cost.booked === null || cost.completeness === 'unknown') return 'unknown'
  const dollars = cost.booked / 1_000_000
  const tag = cost.completeness === 'actual' ? '' : cost.completeness === 'estimated' ? ' est' : ' ≥'
  return dollars < 1 ? `¢${(dollars * 100).toFixed(2)}${tag}` : `$${dollars.toFixed(2)}${tag}`
}

export const shortId = (id: string): string =>
  id.length > 10 ? `${id.slice(0, 4)}…${id.slice(-4)}` : id

export function attemptText(m: RunModel): string {
  if (m.attempt === 0) return ''
  return m.maxAttempts ? `attempt ${m.attempt}/${m.maxAttempts}` : `attempt ${m.attempt}`
}

// The output a status reply and the transcript carry: the running check's
// last lines, or the last check's.
export function latestOutput(m: RunModel, limit: number): string[] {
  for (let i = m.steps.length - 1; i >= 0; i--) {
    const checks = m.steps[i].checks
    if (checks.length > 0) return checks[checks.length - 1].output.slice(-limit)
  }
  return []
}

// The status line: the live runs, or the outcome kept after `done`.
export function statusLine(
  models: Record<string, RunModel>,
  now: number,
  heldOutcome: string | undefined,
): string | undefined {
  const live = Object.values(models).filter(isLive)
  // Claude Code draws the line under the plugin's name, so it does not repeat it.
  if (live.length === 0) return heldOutcome ? `${heldOutcome} · /relais-status` : undefined
  const latest = live[live.length - 1]
  const count = live.length === 1 ? '1 run' : `${live.length} runs`
  const parts = [count, latest.phase || 'starting', attemptText(latest), mmss(elapsedOf(latest, now))]
  return `${parts.filter(Boolean).join(' · ')} · /relais-status`
}

// The same timeline as text, for the transcript and the `status` tool.
export function timelineText(m: RunModel, now: number): string[] {
  const head = [`relais run ${m.run}`, m.phase || 'starting', attemptText(m), mmss(elapsedOf(m, now))]
  const lines = [head.filter(Boolean).join(' · ')]
  for (const step of m.steps) {
    const mark = step.state === 'done' ? '✓' : step.state === 'fail' ? 'FAIL' : '▸'
    const span = step.endedAt ? ` · ${mmss(step.endedAt - step.startedAt)}` : ''
    lines.push(`${mark} ${step.title}${step.detail ? ` · ${step.detail}` : ''}${span}`)
  }
  for (const d of m.decisions) lines.push(`decision · ${d.text}`)
  lines.push(`cost ${formatCost(m.cost)}`)
  if (m.outcome) lines.push(`outcome ${m.outcome.state}${m.outcome.receipt ? ` · receipt ${m.outcome.receipt}` : ''}`)
  return lines
}

// What the `status` tool returns: phases, decisions, cost, the outcome and at
// most the last 40 output lines of a run.
export function statusReply(m: RunModel, now: number) {
  return {
    run: m.run,
    phase: m.phase,
    attempt: m.attempt,
    maxAttempts: m.maxAttempts ?? null,
    elapsedMs: elapsedOf(m, now),
    phases: m.steps.map(s => ({
      title: s.title,
      state: s.state,
      detail: s.detail,
      durationMs: s.endedAt ? s.endedAt - s.startedAt : null,
    })),
    decisions: m.decisions.map(d => d.text),
    cost: formatCost(m.cost),
    outcome: m.outcome ?? null,
    output: latestOutput(m, MAX_OUTPUT_LINES),
    // relais's own human lines and the plugin's failures, the latest few.
    stderr: m.events.filter(e => e.kind === 'stderr').slice(-MAX_STDERR_LINES).map(e => e.text),
  }
}

// A run rebuilt from `relais native status`' timeline (one JSON object):
// the events it lists are folded as the stream's would be.
export function rebuildRuns(
  timeline: unknown,
  now: number,
): Record<string, RunModel> {
  const lines = Array.isArray((timeline as any)?.events) ? (timeline as any).events : []
  let models: Record<string, RunModel> = {}
  for (const line of lines) {
    const run = line.run ?? (timeline as any).run
    models = applyLine(models, { run, at: line.at, event: line.event ?? line }, now)
  }
  return models
}
