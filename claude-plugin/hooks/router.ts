// The session router's decisions, pure: the classifier's prompt and parse,
// the task state, the capability table, recovery, the cache gate, the
// effective mode and the outcome. No `$` here; routing.ts does the effects.
// Plan: docs/plans/2026-10-07-session-router.md; wire contract:
// docs/router-protocol.md.

export type Tier = 'research' | 'implementation' | 'escalation'
export const TIERS: readonly Tier[] = ['research', 'implementation', 'escalation']
export type Effort = 'low' | 'medium' | 'high' | 'xhigh' | 'max'
export const EFFORTS: readonly Effort[] = ['low', 'medium', 'high', 'xhigh', 'max']
export type Mode = 'on' | 'shadow' | 'off'
export type Relation = 'new_task' | 'continuation' | 'correction'
export type Kind = 'question' | 'edit' | 'debug' | 'design' | 'review'
export type Scope = 'local' | 'module' | 'cross-cutting' | 'unknown'
export type Uncertainty = 'low' | 'medium' | 'high'
export type Reason =
  | 'table'
  | 'recovery'
  | 'pin'
  | 'user_model'
  | 'explicit_kept'
  | 'timeout_kept'
  | 'abstained'
  | 'cache_gate'
export type Outcome = 'completed_verified' | 'completed_accepted' | 'corrected' | 'answered' | 'unknown'
export type ReassessEvent = 'failed_verification' | 'repair_failed' | 'scope_growth' | 'spawn' | 'correction' | 'flag' | 'revert'

const KINDS: readonly Kind[] = ['question', 'edit', 'debug', 'design', 'review']
const SCOPES: readonly Scope[] = ['local', 'module', 'cross-cutting', 'unknown']
const UNCERTAINTIES: readonly Uncertainty[] = ['low', 'medium', 'high']
const RELATIONS: readonly Relation[] = ['new_task', 'continuation', 'correction']

// What the classifier says about one request.
export type Class = {
  kind: Kind
  difficulty: number
  scope: Scope
  uncertainty: Uncertainty
  verifiable: boolean
  confidence: number
}

export type Classification = Class & {
  relation: Relation
  explicit: { value: 'accept' | 'correct' | 'none'; quote: string | null }
  user_model: string | null
  // Who the named model is for: this session's own work, or a subagent the
  // person asks to be started ("use an Explore agent on opus").
  user_model_for: 'session' | 'subagent' | null
}

// `relais native router-state`, as far as the plugin reads it. Unknown
// fields are ignored (contract).
export type RouterState = {
  mode: Mode
  mode_reason?: string
  envelope: unknown
  holdout: boolean
  // The session's exploration seed and the rate in force (0 without an
  // envelope, or once the stop rule fired). From a relais that predates
  // them: '' and 0, so nothing is explored.
  seed: string
  epsilon: number
  tiers: Record<Tier, { model: string; effort: Effort | null }>
  capability_table: { version: number; rules: { when: Record<string, unknown>; tier: Tier }[] }
  rates: Record<string, { input: number; output: number; cache_read: number; cache_write: number } | null>
  priors: { task_tokens_by_difficulty: number[]; task_tokens: Record<string, number> }
  pins: Record<string, string>
  excluded_models: string[]
  checks: string[][]
  // The kind of each entry of `checks` (same order): test, lint or build.
  // Empty from a relais that predates it: the kind is then read off the argv.
  check_kinds: string[]
}

// What a check proves. `gate` is an amont stamp, read at task end.
export type CheckKind = 'test' | 'lint' | 'build' | 'gate'

// One task: the main session's, or a router-created subagent's.
export type TaskState = {
  id: string
  agentId: string | null
  opening: string
  startedAt: number
  class: Class
  // The tier in use, and the effort raised by corrections at escalation.
  tier: Tier
  effort: Effort | null
  // Whether the router rewrites the model at all (false when it abstained
  // or the person's model is kept).
  override: boolean
  userModel: string | null
  // A model the person named for the subagents this task starts: kept for
  // them, never applied to the session.
  subagentModel: string | null
  turns: number
  escalations: number
  exhausted: boolean
  hasEdit: boolean
  // Sticky: the task did something that could have written a file the hook
  // cannot see (a Bash writer, a subagent the router did not create, a
  // relais run or replay). With it a task with no recorded edit is `unknown`,
  // never `answered`.
  maybeWrote: boolean
  // Edits since the last verification, and whether the last verification
  // that counted as escalating evidence failed with no pass since.
  editedSinceCheck: boolean
  lastCountedFailure: boolean
  verifications: { command: string; passed: boolean; kind?: CheckKind }[]
  files: string[]
  subagents: string[]
  // The tasks of the router-created subagents this one spawned: their
  // parent's outcome amends them when this task ends (`task_outcome`).
  subagentTasks: { taskId: string; agentId: string | null }[]
  corrected: boolean
  acceptQuote: string | null
  // The person's latest explicit words (accept or correct), for the record.
  explicitQuote: string | null
  relaisAccepted: boolean
  inferred: string[]
  lastReason: Reason
  // The task started one tier below the table's choice, by the exploration
  // draw: its floor stays one below the table's for the whole task.
  exploredDown: boolean
}

// What the exploration draw decided for a task start: a main-session new
// task or a table-routed subagent spawn. Absent on a decision the draw did
// not apply to (continuations, corrections, pinned, person-named or abstained
// spawns, a spawn whose lowered model is excluded, held-out sessions, mode
// not `on`).
export type Exploration = { explored: boolean; propensity: number; draw: number }

export type Decision = {
  tier: Tier
  model: string
  effort: Effort | null
  reason: Reason
  // Whether a request is to be rewritten at all; applied only when the
  // mode is `on` and the session is not held out.
  override: boolean
  wouldPassGate: boolean
  exploration?: Exploration
}

export const CLASSIFIER_MAX_TOKENS = 160
export const CLASSIFIER_TIMEOUT_MS = 3000
const PROMPT_BYTES = 2048
const SUMMARY_BYTES = 600
const SPAWN_PROMPT_BYTES = 1024
const OPENING_BYTES = 1024
export const LOW_CONFIDENCE = 0.6
export const MAX_ESCALATIONS = 2
const LOCAL_FILE_LIMIT = 8
const DEFAULT_PRIORS = [20_000, 60_000, 150_000, 400_000, 800_000]
// The token mix a task's tokens are priced by. router-state serves no
// observed mix per kind in R1, so a fixed one stands in: most of an agentic
// task's tokens are cache reads.
export const DEFAULT_MIX = { input: 0.1, output: 0.05, cache_read: 0.85 }
// The contract's default alias resolution (`[session_routing.model_ids]`).
export const DEFAULT_MODEL_IDS: Record<string, string> = {
  haiku: 'claude-haiku-5-5',
  sonnet: 'claude-sonnet-5-5',
  opus: 'claude-opus-5-5',
  fable: 'claude-fable-5-1',
}
export const DEFAULT_TABLE: RouterState['capability_table'] = {
  version: 1,
  rules: [
    { when: { difficulty_min: 4 }, tier: 'escalation' },
    { when: { uncertainty: ['high'], scope: ['module', 'cross-cutting', 'unknown'] }, tier: 'escalation' },
    { when: { difficulty_max: 2, uncertainty: ['low'], verifiable_or_question: true }, tier: 'research' },
    { when: {}, tier: 'implementation' },
  ],
}

// --- text -----------------------------------------------------------------

const encoder = new TextEncoder()

// The text cut to at most `bytes` of UTF-8, on a character boundary.
export function clip(text: string, bytes: number): string {
  if (encoder.encode(text).length <= bytes) return text
  let out = text.slice(0, bytes)
  while (out.length > 0 && encoder.encode(out).length > bytes) out = out.slice(0, -1)
  return out
}

// FNV-1a over the text: a spawn's key, not a secret.
export function hash(text: string): string {
  let h = 0x811c9dc5
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i)
    h = Math.imul(h, 0x01000193) >>> 0
  }
  return h.toString(16).padStart(8, '0')
}

// --- the classifier ---------------------------------------------------------

// Fixed, so its hash (`prompt_version`) names one prompt.
export const CLASSIFIER_SYSTEM = [
  'You label one request a person typed to a coding agent. Reply with ONE JSON object and nothing else:',
  '{"relation":"new_task|continuation|correction","kind":"question|edit|debug|design|review","difficulty":1,"scope":"local|module|cross-cutting|unknown","uncertainty":"low|medium|high","verifiable":true,"explicit":{"value":"accept|correct|none","quote":null},"user_model":null,"user_model_for":null,"confidence":0.8}',
  'Fields:',
  '- relation, read against ACTIVE TASK: new_task when there is none or the request starts unrelated work; continuation when it goes on with the task ("yes, do it", "continue", "same issue in the other service"); correction when it says the work done was wrong or must be redone.',
  '- kind: question (an answer, no edits), edit, debug, design, review.',
  '- difficulty 1-5: 1 a lookup or a rename; 2 a small contained change; 3 an ordinary feature or bug; 4 a subtle bug or a change across modules; 5 a hard investigation (concurrency, behaviour under load, an unclear cause).',
  '- scope: local (one file or function), module (one component), cross-cutting (several components), unknown.',
  '- uncertainty: high when the cause or the approach is unclear (a subtle bug), low when the work is plain.',
  '- verifiable: true when a test, build or check exists or is named that can prove the work.',
  '- explicit: accept when the person approves the work done ("looks good", "ship it"), correct when they say it is wrong; quote their words, else value none and quote null.',
  '- user_model: a model the person named (haiku, sonnet, opus, fable or a full id), else null.',
  '- user_model_for: "subagent" when the named model is for an agent or subagent the person asks to be started ("use an Explore agent with opus"), "session" when it is for this work itself, null when no model is named.',
  '- confidence 0-1: how sure you are of relation and difficulty.',
].join('\n')

// The task, as the classifier reads it: at most 600 bytes.
export function taskSummary(task: TaskState | undefined): string {
  if (!task) return 'none'
  const last = task.verifications[task.verifications.length - 1]
  const summary = {
    opening: clip(task.opening, 300),
    kind: task.class.kind,
    difficulty: task.class.difficulty,
    scope: task.class.scope,
    uncertainty: task.class.uncertainty,
    tier: task.tier,
    turns: task.turns,
    last_check: last ? `${clip(last.command, 60)}: ${last.passed ? 'passed' : 'failed'}` : null,
  }
  return clip(JSON.stringify(summary), SUMMARY_BYTES)
}

export function classifierPrompt(text: string, task: TaskState | undefined): string {
  return `ACTIVE TASK: ${taskSummary(task)}\n\nREQUEST:\n${clip(text, PROMPT_BYTES)}`
}

export function spawnPrompt(type: string, description: string, prompt: string): string {
  return [
    'ACTIVE TASK: none',
    '',
    `REQUEST (delegated to a subagent of type ${clip(type, 80)}; description: ${clip(description, 200)}):`,
    clip(prompt, SPAWN_PROMPT_BYTES),
  ].join('\n')
}

const oneOf = <T extends string>(list: readonly T[], value: unknown, fallback: T): T =>
  typeof value === 'string' && (list as readonly string[]).includes(value) ? (value as T) : fallback

// The classifier's reply, or undefined when it holds no usable object.
// Fences and prose around the object are tolerated; a missing relation is not.
export function parseClassification(text: unknown): Classification | undefined {
  if (typeof text !== 'string') return undefined
  const start = text.indexOf('{')
  const end = text.lastIndexOf('}')
  if (start < 0 || end <= start) return undefined
  let raw: any
  try {
    raw = JSON.parse(text.slice(start, end + 1))
  } catch {
    return undefined
  }
  if (!raw || typeof raw !== 'object' || !RELATIONS.includes(raw.relation)) return undefined
  const difficulty = Math.round(Number(raw.difficulty))
  const confidence = Number(raw.confidence)
  const explicitRaw = raw.explicit
  const explicitValue =
    typeof explicitRaw === 'string' ? explicitRaw : typeof explicitRaw === 'object' && explicitRaw ? explicitRaw.value : 'none'
  const quote = typeof explicitRaw === 'object' && explicitRaw && typeof explicitRaw.quote === 'string' ? explicitRaw.quote : null
  return {
    relation: raw.relation,
    kind: oneOf(KINDS, raw.kind, 'edit'),
    difficulty: Number.isFinite(difficulty) ? Math.min(5, Math.max(1, difficulty)) : 3,
    scope: oneOf(SCOPES, raw.scope, 'unknown'),
    uncertainty: oneOf(UNCERTAINTIES, raw.uncertainty, 'medium'),
    verifiable: raw.verifiable === true,
    explicit: { value: oneOf(['accept', 'correct', 'none'] as const, explicitValue, 'none'), quote: quote ? clip(quote, 300) : null },
    user_model: typeof raw.user_model === 'string' && raw.user_model.trim() !== '' ? raw.user_model.trim() : null,
    user_model_for: raw.user_model_for === 'subagent' ? 'subagent' : raw.user_model_for === 'session' ? 'session' : null,
    confidence: Number.isFinite(confidence) ? Math.min(1, Math.max(0, confidence)) : 0,
  }
}

const classOf = (c: Classification | Class): Class => ({
  kind: c.kind,
  difficulty: c.difficulty,
  scope: c.scope,
  uncertainty: c.uncertainty,
  verifiable: c.verifiable,
  confidence: c.confidence,
})

// --- router-state -----------------------------------------------------------

// `router-state`'s stdout, or undefined when it is not one the plugin can
// route by (the plugin then runs in shadow with nothing to decide).
export function parseRouterState(text: unknown): RouterState | undefined {
  let raw: any
  try {
    raw = JSON.parse(String(text ?? ''))
  } catch {
    return undefined
  }
  if (!raw || raw.schema !== 1 || !raw.tiers) return undefined
  const tiers = {} as RouterState['tiers']
  for (const tier of TIERS) {
    const t = raw.tiers[tier]
    if (!t || typeof t.model !== 'string' || t.model === '') return undefined
    tiers[tier] = { model: t.model, effort: EFFORTS.includes(t.effort) ? t.effort : null }
  }
  const rules = Array.isArray(raw.capability_table?.rules)
    ? raw.capability_table.rules.filter((r: any) => r && typeof r.when === 'object' && TIERS.includes(r.tier))
    : []
  return {
    mode: oneOf(['on', 'shadow', 'off'] as const, raw.mode, 'shadow'),
    mode_reason: typeof raw.mode_reason === 'string' ? raw.mode_reason : undefined,
    envelope: raw.envelope ?? null,
    holdout: raw.holdout === true,
    seed: typeof raw.seed === 'string' ? raw.seed : '',
    epsilon: typeof raw.epsilon === 'number' && raw.epsilon > 0 && raw.epsilon <= 1 ? raw.epsilon : 0,
    tiers,
    capability_table: rules.length > 0 ? { version: Number(raw.capability_table.version) || 1, rules } : DEFAULT_TABLE,
    rates: raw.rates && typeof raw.rates === 'object' ? raw.rates : {},
    priors: {
      task_tokens_by_difficulty: Array.isArray(raw.priors?.task_tokens_by_difficulty)
        ? raw.priors.task_tokens_by_difficulty
        : DEFAULT_PRIORS,
      task_tokens: raw.priors?.task_tokens && typeof raw.priors.task_tokens === 'object' ? raw.priors.task_tokens : {},
    },
    pins: raw.pins && typeof raw.pins === 'object' ? raw.pins : {},
    excluded_models: Array.isArray(raw.excluded_models) ? raw.excluded_models.filter((m: unknown) => typeof m === 'string') : [],
    ...parseChecks(raw),
  }
}

const CHECK_KINDS: readonly string[] = ['test', 'lint', 'build']

// The checks and their kinds, kept in step: an entry that is no argv drops
// its kind with it.
function parseChecks(raw: any): { checks: string[][]; check_kinds: string[] } {
  const checks: string[][] = []
  const check_kinds: string[] = []
  const kinds: unknown[] = Array.isArray(raw.check_kinds) ? raw.check_kinds : []
  const list: unknown[] = Array.isArray(raw.checks) ? raw.checks : []
  list.forEach((c, i) => {
    if (!Array.isArray(c) || c.length === 0 || !c.every(a => typeof a === 'string')) return
    checks.push(c)
    const served = kinds[i]
    check_kinds.push(typeof served === 'string' && CHECK_KINDS.includes(served) ? served : kindOfArgv(c))
  })
  return { checks, check_kinds }
}

// A check's kind when router-state did not serve it: the words that name it.
export function kindOfArgv(argv: string[]): CheckKind {
  const head = argv.slice(0, 3)
  if (head.includes('test') || head[0] === 'pytest') return 'test'
  const line = head.join(' ')
  if (/\b(lint|clippy|vet|ruff|mypy|pyright)\b/.test(line)) return 'lint'
  if (/\b(build|check|typecheck|tsc)\b/.test(line) && !/^(make|just) check$/.test(line)) return 'build'
  return 'test'
}

// --- the mode ---------------------------------------------------------------

// router-state's mode (R1b: on whenever the envelope is recorded, narrowed
// by RELAIS_SESSION_ROUTING); shadow when there is no state to route by.
export function modeEffective(state: RouterState | undefined): Mode {
  return state ? state.mode : 'shadow'
}

// --- tiers and models -------------------------------------------------------

export const rank = (tier: Tier) => TIERS.indexOf(tier)
export const higher = (a: Tier, b: Tier): Tier => (rank(a) >= rank(b) ? a : b)
const stepUp = (tier: Tier): Tier => TIERS[Math.min(TIERS.length - 1, rank(tier) + 1)]
const stepDown = (tier: Tier): Tier => TIERS[Math.max(0, rank(tier) - 1)]

// The tier the table asks for a task now: one step lower for a task the
// exploration draw lowered, never below research.
const floorTier = (state: RouterState, task: TaskState): Tier => {
  const table = tableTier(state.capability_table, task.class)
  return task.exploredDown ? stepDown(table) : table
}

// The capability table's tier for a classification: the first rule whose
// conditions all hold. A condition the plugin does not know never holds.
export function tableTier(table: RouterState['capability_table'], c: Class): Tier {
  for (const rule of table.rules) {
    if (ruleHolds(rule.when, c)) return rule.tier
  }
  return 'implementation'
}

function ruleHolds(when: Record<string, unknown>, c: Class): boolean {
  for (const [key, value] of Object.entries(when)) {
    switch (key) {
      case 'difficulty_min':
        if (!(c.difficulty >= Number(value))) return false
        break
      case 'difficulty_max':
        if (!(c.difficulty <= Number(value))) return false
        break
      case 'uncertainty':
      case 'scope':
      case 'kind':
        if (!Array.isArray(value) || !value.includes((c as any)[key])) return false
        break
      case 'verifiable':
        if (c.verifiable !== value) return false
        break
      case 'verifiable_or_question':
        if ((c.verifiable || c.kind === 'question') !== value) return false
        break
      default:
        return false
    }
  }
  return true
}

// The tier a model id stands at, by the tiers' models: exactly, or by its
// family name (`opus` in `claude-opus-5-5`).
export function tierOfModel(state: RouterState, model: string | undefined): Tier | undefined {
  if (!model) return undefined
  for (const tier of [...TIERS].reverse()) if (state.tiers[tier].model === model) return tier
  const family = familyOf(model)
  if (!family) return undefined
  for (const tier of [...TIERS].reverse()) if (familyOf(state.tiers[tier].model) === family) return tier
  return undefined
}

const familyOf = (model: string): string | undefined =>
  Object.keys(DEFAULT_MODEL_IDS).find(alias => model.toLowerCase().includes(alias))

// A model the person named, as the full id `turn.step` needs; undefined
// when it cannot be resolved (it is then not acted on).
export function resolveModel(state: RouterState, named: string | null): string | undefined {
  if (!named) return undefined
  const n = named.toLowerCase()
  if (/^claude-[a-z0-9-]+$/.test(n)) return n
  const family = familyOf(n)
  if (!family) return undefined
  const tier = TIERS.find(t => familyOf(state.tiers[t].model) === family)
  return tier ? state.tiers[tier].model : DEFAULT_MODEL_IDS[family]
}

export const shortModel = (model: string) => model.replace(/^claude-/, '')

const effortOf = (state: RouterState, task: TaskState): Effort | null => task.effort ?? state.tiers[task.tier].effort

export function decisionOf(
  state: RouterState,
  task: TaskState,
  reason: Reason,
  wouldPassGate = true,
  exploration?: Exploration,
): Decision {
  return {
    tier: task.tier,
    model: task.userModel ?? state.tiers[task.tier].model,
    effort: task.userModel ? null : effortOf(state, task),
    reason,
    override: task.override,
    wouldPassGate,
    ...(exploration ? { exploration } : {}),
  }
}

// --- the cache gate ---------------------------------------------------------

const blended = (rate: { input: number; output: number; cache_read: number }, mix = DEFAULT_MIX) =>
  mix.input * rate.input + mix.output * rate.output + mix.cache_read * rate.cache_read

export function predictedTokens(state: RouterState, c: Class): number {
  const learned = state.priors.task_tokens[`${c.kind}:${c.difficulty}`]
  if (typeof learned === 'number' && learned > 0) return learned
  const prior = state.priors.task_tokens_by_difficulty[c.difficulty - 1]
  return typeof prior === 'number' && prior > 0 ? prior : DEFAULT_PRIORS[c.difficulty - 1]
}

// Whether switching from `from` to a cheaper `to` at a task start pays for
// rewriting the cache: saving > rebuild. Rates are micro-USD per million
// tokens; `cache_write` is the 1-hour rate (S0). An unknown price blocks.
export function cacheGate(input: {
  state: RouterState
  from: string
  to: string
  contextTokens: number
  class: Class
}): { pass: boolean; saving: number; rebuild: number } {
  const from = input.state.rates[input.from]
  const to = input.state.rates[input.to]
  if (!from || !to) return { pass: false, saving: 0, rebuild: Infinity }
  const rebuild = (input.contextTokens * to.cache_write) / 1e6
  const saving = (predictedTokens(input.state, input.class) * (blended(from) - blended(to))) / 1e6
  return { pass: saving > rebuild, saving, rebuild }
}

// Tasks are JSON data: a copy the caller may keep or drop.
const clone = <T>(value: T): T => JSON.parse(JSON.stringify(value))

// --- tasks ------------------------------------------------------------------

export function newTask(id: string, agentId: string | null, opening: string, c: Class, tier: Tier, now: number): TaskState {
  return {
    id,
    agentId,
    opening: clip(opening, OPENING_BYTES),
    startedAt: now,
    class: c,
    tier,
    effort: null,
    override: true,
    userModel: null,
    subagentModel: null,
    turns: 0,
    escalations: 0,
    exhausted: false,
    hasEdit: false,
    maybeWrote: false,
    editedSinceCheck: false,
    lastCountedFailure: false,
    verifications: [],
    files: [],
    subagents: [],
    subagentTasks: [],
    corrected: false,
    acceptQuote: null,
    explicitQuote: null,
    relaisAccepted: false,
    inferred: [],
    lastReason: 'table',
    exploredDown: false,
  }
}

export type MainInput = {
  state: RouterState
  task: TaskState | undefined
  // undefined: the classifier timed out or failed, or no person prompt led
  // to this turn (`timedOut` says which).
  cls: Classification | undefined
  timedOut: boolean
  // The next prompt after /clear is a new task whatever it says.
  forceNew: boolean
  text: string
  // The person's /model, and the model the last request ran on.
  sessionModel: string | undefined
  lastModel: string | undefined
  contextTokens: number
  newTaskId: string
  // The session's id, which the exploration draw is keyed by; without it
  // nothing is explored.
  session?: string
  now: number
}

export type MainOutput = {
  task: TaskState | undefined
  ended: TaskState | undefined
  decision: Decision | undefined
  relation: Relation | undefined
  reassess: ReassessRecord[]
}

export type ReassessRecord = {
  event: ReassessEvent
  tier_from: Tier
  tier_to: Tier
  effort_to: Effort | null
  escalating: boolean
  // A revert's files, hashed (fileHash).
  files?: string[]
}

// The decision for a main-session turn's first request, and the task it
// leaves. Pure: the caller keeps the task and records what it returns.
export function decideMain(input: MainInput): MainOutput {
  const { state, cls } = input
  let task = input.task ? clone(input.task) : undefined
  if (!cls) {
    if (!task) return { task, ended: undefined, decision: undefined, relation: undefined, reassess: [] }
    task.turns += 1
    const reason: Reason = input.timedOut ? 'timeout_kept' : 'explicit_kept'
    task.lastReason = reason
    return { task, ended: undefined, decision: decisionOf(state, task, reason), relation: 'continuation', reassess: [] }
  }
  const relation: Relation = input.forceNew || !task ? 'new_task' : cls.relation
  const isCorrection = !!task && (relation === 'correction' || cls.explicit.value === 'correct')
  if (relation === 'new_task' && !isCorrection) return startTask(input, cls)
  task = task as TaskState
  task.turns += 1
  if (cls.explicit.value === 'accept') {
    task.acceptQuote = cls.explicit.quote ?? clip(input.text, 200)
    task.explicitQuote = task.acceptQuote
  }
  // A continuation inherits the class; only difficulty may rise, and the
  // table may raise the tier with it, never lower it.
  task.class = { ...task.class, difficulty: Math.max(task.class.difficulty, cls.difficulty) }
  const reassess: ReassessRecord[] = []
  let reason: Reason = 'explicit_kept'
  const raised = higher(task.tier, floorTier(state, task))
  if (raised !== task.tier) {
    task.tier = raised
    task.override = true
    reason = 'table'
  }
  if (isCorrection) {
    task.corrected = true
    task.explicitQuote = cls.explicit.quote ?? task.explicitQuote
    const { task: up, record } = escalate(state, task, 'correction')
    task = up
    reassess.push(record)
    reason = 'recovery'
  }
  task.lastReason = reason
  return { task, ended: undefined, decision: decisionOf(state, task, reason), relation: isCorrection ? 'correction' : 'continuation', reassess }
}

function startTask(input: MainInput, cls: Classification): MainOutput {
  const { state } = input
  const ended = input.task ? endInferred(input.task) : undefined
  const c = classOf(cls)
  const table = tableTier(state.capability_table, c)
  const sessionTier = tierOfModel(state, input.sessionModel)
  const task = newTask(input.newTaskId, null, input.text, c, table, input.now)
  task.turns = 1
  const userModel = resolveModel(state, cls.user_model)
  // A model named for a subagent is kept for the spawns, never the session.
  if (userModel && cls.user_model_for === 'subagent') task.subagentModel = userModel
  if (userModel && cls.user_model_for !== 'subagent') {
    task.userModel = userModel
    task.tier = tierOfModel(state, userModel) ?? table
    task.lastReason = 'user_model'
    return { task, ended, decision: decisionOf(state, task, 'user_model'), relation: 'new_task', reassess: [] }
  }
  // Below 0.6 a new task stays at /model's tier: confidence alone never
  // justifies a cheaper model.
  if (cls.confidence < LOW_CONFIDENCE) {
    task.tier = sessionTier ?? higher(table, 'implementation')
    task.override = false
    task.lastReason = 'abstained'
    return { task, ended, decision: decisionOf(state, task, 'abstained'), relation: 'new_task', reassess: [] }
  }
  // Below /model only through the table's research rule.
  let tier = table
  if (sessionTier && rank(table) < rank(sessionTier) && table !== 'research') tier = sessionTier
  // Exploration: below epsilon the initial tier is one step lower, never
  // below research. At research nothing can be lowered.
  const exploration = exploreAt(input, tier)
  if (exploration?.explored) {
    tier = stepDown(tier)
    task.exploredDown = true
  }
  task.tier = tier
  let reason: Reason = 'table'
  let wouldPassGate = true
  const from = input.lastModel ?? input.sessionModel
  const fromTier = tierOfModel(state, from) ?? sessionTier
  const to = state.tiers[tier].model
  const isDown = fromTier === undefined ? from !== to : rank(tier) < rank(fromTier)
  if (isDown && from && from !== to) {
    wouldPassGate = cacheGate({ state, from, to, contextTokens: input.contextTokens, class: c }).pass
    if (!wouldPassGate) {
      reason = 'cache_gate'
      // The request stays on the model it was on.
      if (fromTier) task.tier = fromTier
      task.override = false
    }
  }
  task.lastReason = reason
  return { task, ended, decision: decisionOf(state, task, reason, wouldPassGate, exploration), relation: 'new_task', reassess: [] }
}

// The draw for a task start, keyed by (seed, session, task id) so the ledger
// can recompute it: only in mode `on`, outside the hold-out, with an epsilon
// in force and a seed and session to key it by. At research nothing can be
// lowered: not explored, propensity 1.
export function drawAt(state: RouterState, session: string | null | undefined, taskId: string, tier: Tier): Exploration | undefined {
  if (state.mode !== 'on' || state.holdout || !(state.epsilon > 0) || !state.seed || !session) return undefined
  const draw = explorationDraw(state.seed, session, taskId)
  if (tier === 'research') return { explored: false, propensity: 1, draw }
  const explored = draw < state.epsilon
  return { explored, propensity: explored ? state.epsilon : 1 - state.epsilon, draw }
}

// The draw for a main-session task start.
function exploreAt(input: MainInput, tier: Tier): Exploration | undefined {
  return drawAt(input.state, input.session, input.newTaskId, tier)
}

// A task the next one replaced: no complaint followed it.
function endInferred(task: TaskState): TaskState {
  const ended = clone(task)
  if (!ended.corrected && !ended.inferred.includes('no_complaint')) ended.inferred.push('no_complaint')
  return ended
}

// One step up on escalating evidence, at most two per task; past that, or
// already at escalation, a correction raises the effort one step instead.
export function escalate(state: RouterState, task: TaskState, event: ReassessEvent): { task: TaskState; record: ReassessRecord } {
  const t = clone(task)
  const from = t.tier
  if (t.escalations < MAX_ESCALATIONS && rank(t.tier) < rank('escalation')) {
    t.tier = stepUp(t.tier)
    t.escalations += 1
    t.override = true
  } else {
    // Evidence the tier can no longer answer: recorded as exhausted.
    t.exhausted = true
    if (event === 'correction' || event === 'flag') {
      const base = t.effort ?? state.tiers[t.tier].effort ?? 'high'
      t.effort = EFFORTS[Math.min(EFFORTS.length - 1, EFFORTS.indexOf(base) + 1)]
      t.override = true
    }
  }
  t.lastReason = 'recovery'
  return { task: t, record: { event, tier_from: from, tier_to: t.tier, effort_to: effortOf(state, t), escalating: true } }
}

// The table again with what the task now knows (scope, a delegation): it
// may raise the tier, never lower it, and is no escalation.
export function rerun(state: RouterState, task: TaskState, event: 'scope_growth' | 'spawn'): { task: TaskState; record: ReassessRecord } {
  const t = clone(task)
  const from = t.tier
  const raised = higher(t.tier, floorTier(state, t))
  if (raised !== t.tier) {
    t.tier = raised
    t.override = true
  }
  return { task: t, record: { event, tier_from: from, tier_to: t.tier, effort_to: effortOf(state, t), escalating: false } }
}

// --- evidence from tool calls -------------------------------------------------

export const EDIT_TOOLS = ['Edit', 'Write', 'MultiEdit', 'NotebookEdit']

// A shell command's words, quotes taken off: enough to match a prefix.
function words(segment: string): string[] {
  const out: string[] = []
  const re = /"([^"]*)"|'([^']*)'|(\S+)/g
  let m: RegExpExecArray | null
  while ((m = re.exec(segment)) !== null) out.push(m[1] ?? m[2] ?? m[3])
  return out
}

// Whether a Bash command runs one of the checks: each `&&`/`;` segment,
// its leading `NAME=value` assignments dropped, is matched on its first
// words against the argv prefixes router-state serves.
export function matchesCheck(command: string, checks: string[][]): boolean {
  return checkIndex(command, checks) !== undefined
}

// The index of the first check a command runs, or undefined.
export function checkIndex(command: string, checks: string[][]): number | undefined {
  for (const segment of command.split(/&&|;|\n/)) {
    const argv = words(segment.trim())
    while (argv.length > 0 && /^[A-Za-z_][A-Za-z0-9_]*=/.test(argv[0])) argv.shift()
    const at = checks.findIndex(prefix => prefix.length <= argv.length && prefix.every((w, i) => argv[i] === w))
    if (at >= 0) return at
  }
  return undefined
}

// Drops, in place, the programs that only run another one (`env`, `xargs`,
// `npx`, `bunx`, `pnpm exec`, `uv run`) with their flags and assignments, so
// the program they run is the one judged.
function stripLauncher(argv: string[]) {
  for (;;) {
    const program = (argv[0] ?? '').replace(/^.*\//, '')
    const runs = program === 'pnpm' ? argv[1] === 'exec' : program === 'uv' ? argv[1] === 'run' : ['env', 'xargs', 'npx', 'bunx'].includes(program)
    if (!runs) return
    argv.splice(0, program === 'pnpm' || program === 'uv' ? 2 : 1)
    while (argv.length > 0 && (argv[0].startsWith('-') || /^[A-Za-z_][A-Za-z0-9_]*=/.test(argv[0]))) argv.shift()
  }
}

// Whether a Bash command may write a file: a `sed -i`, a redirection or
// `tee`, a `git` command that changes the tree, a formatter with a write
// flag, or a plain file-changing program. A guess toward "yes": a task that
// might have written is `unknown`, never `answered`.
export function mayWrite(command: string): boolean {
  for (const segment of command.split(/&&|\|\||\||;|\n/)) {
    const text = segment.trim().replace(/\d*>&\d+/g, '').replace(/\d*>>?\s*\/dev\/null/g, '')
    if (/(^|[^<>=-])>>?(?![>&=])/.test(text)) return true
    const argv = words(text)
    while (argv.length > 0 && /^[A-Za-z_][A-Za-z0-9_]*=/.test(argv[0])) argv.shift()
    if (argv.length === 0) continue
    stripLauncher(argv)
    if (argv.length === 0) continue
    const program = argv[0].replace(/^.*\//, '')
    const rest = argv.slice(1)
    if (['tee', 'mv', 'cp', 'rm', 'touch', 'patch', 'truncate', 'install', 'ln'].includes(program)) return true
    if (program === 'sed' && rest.some(w => /^-[a-zA-Z]*i/.test(w) || w === '--in-place' || w.startsWith('--in-place='))) return true
    if (program === 'git' && rest.some(w => ['apply', 'checkout', 'commit', 'merge', 'rebase', 'restore', 'reset', 'stash', 'cherry-pick', 'revert', 'pull', 'switch', 'am', 'rm', 'mv', 'clean'].includes(w))) return true
    const writeFlag = rest.some(w => w === '--write' || w === '--fix' || w === '--apply' || w === '-w' || w === '-i')
    const checkFlag = rest.some(w => w === '--check' || w === '--diff' || w === '--dry-run' || w === '-l')
    if (['prettier', 'biome', 'eslint', 'gofmt', 'goimports', 'rustfmt', 'ruff', 'black', 'isort', 'clang-format'].includes(program)) {
      if (writeFlag || (['rustfmt', 'black', 'isort'].includes(program) && !checkFlag)) return true
    }
    if (program === 'cargo' && (rest[0] === 'fmt' || rest[0] === 'fix') && !checkFlag) return true
    if (program === 'go' && rest[0] === 'fmt') return true
    if (program === 'cargo' && rest[0] === 'clippy' && rest.includes('--fix')) return true
    if (program === 'perl' && rest.some(w => /^-[a-zA-Z]*i/.test(w))) return true
    if (program === 'relais' && rest[0] === 'run') return true
    if (program === 'relais' && rest[0] === 'dataset' && rest[1] === 'replay' && !rest.includes('--dry-run')) return true
  }
  return false
}

// What a finished tool call says, or undefined when it is no evidence.
export type Evidence =
  | { kind: 'edit'; file: string | undefined }
  | { kind: 'check'; command: string; passed: boolean; checkKind?: CheckKind }

// `kinds` are router-state's `check_kinds`, aligned with `checks`; without
// them the evidence carries no kind and the record reads it off the argv.
export function evidenceOf(tool: string, e: any, result: any, checks: string[][], kinds?: string[]): Evidence | undefined {
  if (!result || result.deny !== undefined) return undefined
  if (EDIT_TOOLS.includes(tool)) {
    if (result.isError) return undefined
    const file = typeof e.file_path === 'string' ? e.file_path : typeof e.notebook_path === 'string' ? e.notebook_path : undefined
    return { kind: 'edit', file }
  }
  if (tool !== 'Bash') return undefined
  // A background command returns no exit code.
  if (e.run_in_background === true) return undefined
  const command = typeof e.command === 'string' ? e.command : ''
  const at = checkIndex(command, checks)
  if (at === undefined) return undefined
  const served = kinds?.[at]
  const checkKind = served === 'test' || served === 'lint' || served === 'build' ? served : undefined
  if (result.isError) {
    // S0: a failing command is `isError` with "Exit code N"; an error
    // without one (interrupted, refused) says nothing about the code.
    return /Exit code \d+/.test(String(result.text ?? result.result ?? '')) ? { kind: 'check', command, passed: false, checkKind } : undefined
  }
  return { kind: 'check', command, passed: true, checkKind }
}

// --- reverts and file hashes (plan §5: missed failures, measured) -----------------

// SHA-256, hex: the files a task edited are recorded hashed, never named.
const K = new Uint32Array([
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01,
  0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
  0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
  0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
  0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08,
  0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
  0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
])

export function sha256Hex(text: string): string {
  return Array.from(sha256Words(encoder.encode(text)), x => x.toString(16).padStart(8, '0')).join('')
}

// The eight 32-bit words of the SHA-256 of `data`.
function sha256Words(data: Uint8Array): Uint32Array {
  const length = ((data.length + 9 + 63) >> 6) << 6
  const bytes = new Uint8Array(length)
  bytes.set(data)
  bytes[data.length] = 0x80
  const view = new DataView(bytes.buffer)
  view.setUint32(length - 8, Math.floor((data.length * 8) / 2 ** 32))
  view.setUint32(length - 4, (data.length * 8) >>> 0)
  const h = new Uint32Array([0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19])
  const w = new Uint32Array(64)
  const rotr = (x: number, n: number) => (x >>> n) | (x << (32 - n))
  for (let offset = 0; offset < length; offset += 64) {
    for (let i = 0; i < 16; i++) w[i] = view.getUint32(offset + i * 4)
    for (let i = 16; i < 64; i++) {
      const s0 = rotr(w[i - 15], 7) ^ rotr(w[i - 15], 18) ^ (w[i - 15] >>> 3)
      const s1 = rotr(w[i - 2], 17) ^ rotr(w[i - 2], 19) ^ (w[i - 2] >>> 10)
      w[i] = (w[i - 16] + s0 + w[i - 7] + s1) >>> 0
    }
    let [a, b, c, d, e, f, g, hh] = h
    for (let i = 0; i < 64; i++) {
      const t1 = (hh + (rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25)) + ((e & f) ^ (~e & g)) + K[i] + w[i]) >>> 0
      const t2 = ((rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22)) + ((a & b) ^ (a & c) ^ (b & c))) >>> 0
      hh = g
      g = f
      f = e
      e = (d + t1) >>> 0
      d = c
      c = b
      b = a
      a = (t1 + t2) >>> 0
    }
    h[0] += a
    h[1] += b
    h[2] += c
    h[3] += d
    h[4] += e
    h[5] += f
    h[6] += g
    h[7] += hh
  }
  return h
}

// --- exploration (docs/router-protocol.md, "Exploration") ----------------------------

const MASK64 = (1n << 64n) - 1n

// The first output of SplitMix64 seeded with `seed` (crate::rng::SplitMix64).
export function splitMix64First(seed: bigint): bigint {
  const state = (seed + 0x9e3779b97f4a7c15n) & MASK64
  let z = state
  z = ((z ^ (z >> 30n)) * 0xbf58476d1ce4e5b9n) & MASK64
  z = ((z ^ (z >> 27n)) * 0x94d049bb133111ebn) & MASK64
  return z ^ (z >> 31n)
}

// The exploration draw in [0, 1), the same function as Rust's
// `crate::rng::exploration_draw`: u is the first 8 bytes, big-endian, of
// SHA-256 over the UTF-8 bytes of seed, session and task, each preceded by
// its byte length as a 4-byte big-endian integer; x is SplitMix64(u)'s
// first output; the draw is (x >> 11) * 2^-53.
export function explorationDraw(seed: string, session: string, task: string): number {
  const parts = [seed, session, task].map(part => encoder.encode(part))
  const data = new Uint8Array(parts.reduce((n, part) => n + 4 + part.length, 0))
  const view = new DataView(data.buffer)
  let at = 0
  for (const part of parts) {
    view.setUint32(at, part.length)
    data.set(part, at + 4)
    at += 4 + part.length
  }
  const words = sha256Words(data)
  const u = (BigInt(words[0]) << 32n) | BigInt(words[1])
  return Number(splitMix64First(u) >> 11n) * 2 ** -53
}

// A path as the repository names it: relative to the session's cwd, no
// leading `./`.
export function repoRelative(file: string, cwd: string | undefined): string {
  const root = cwd ? cwd.replace(/\/+$/, '') + '/' : ''
  const rel = root && file.startsWith(root) ? file.slice(root.length) : file
  return rel.replace(/^(\.\/)+/, '')
}

// The contract's file hash: the first 16 hex digits of the sha256 of the
// repo-relative path.
export const fileHash = (file: string, cwd: string | undefined) => sha256Hex(repoRelative(file, cwd)).slice(0, 16)

// The paths a Bash command reverts, or undefined when it reverts nothing:
// `git revert …` (its files unknown here: []), `git checkout [<ref>] --
// <paths>`, and `git restore <paths>` unless it only unstages.
// holds-until: R2 computes missed failures. Until then a `git revert` (no
// paths) and a pathspec like `.` are recorded but can never overlap a
// task's files. R2 resolves them: the reverted commit's files via `git
// show --name-only`, and `.` as every file the task touched.
export function revertedPaths(command: string): string[] | undefined {
  let found: string[] | undefined
  for (const segment of command.split(/&&|\|\||;|\||\n/)) {
    const argv = words(segment.trim())
    while (argv.length > 0 && /^[A-Za-z_][A-Za-z0-9_]*=/.test(argv[0])) argv.shift()
    if (argv[0] !== 'git') continue
    const sub = argv[1]
    const rest = argv.slice(2)
    let paths: string[] | undefined
    if (sub === 'revert') paths = []
    else if (sub === 'checkout') {
      const dash = rest.indexOf('--')
      if (dash >= 0) paths = rest.slice(dash + 1)
    } else if (sub === 'restore') {
      const dash = rest.indexOf('--')
      const flags = dash >= 0 ? rest.slice(0, dash) : rest.filter(a => a.startsWith('-'))
      const onlyStaged =
        flags.some(f => f === '--staged' || f === '-S') && !flags.some(f => f === '--worktree' || f === '-W')
      if (!onlyStaged) paths = dash >= 0 ? rest.slice(dash + 1) : rest.filter(a => !a.startsWith('-'))
    }
    if (paths !== undefined) found = [...(found ?? []), ...paths]
  }
  return found
}

// A revert's reassessment: no escalation, the tier kept, the files hashed.
export function revertRecord(state: RouterState, task: TaskState, paths: string[], cwd: string | undefined): ReassessRecord {
  return {
    event: 'revert',
    tier_from: task.tier,
    tier_to: task.tier,
    effort_to: effortOf(state, task),
    escalating: false,
    files: [...new Set(paths.map(path => fileHash(path, cwd)))].sort(),
  }
}

// The scope the edited files show: a second top-level directory makes it
// cross-cutting; more than 8 files makes a local task a module one.
export function grownScope(scope: Scope, files: string[], cwd: string | undefined): Scope {
  if (scope === 'cross-cutting' || scope === 'unknown') return scope
  const root = cwd ? cwd.replace(/\/+$/, '') + '/' : ''
  const tops = new Set<string>()
  for (const file of files) {
    const rel = root && file.startsWith(root) ? file.slice(root.length) : file.replace(/^\/+/, '')
    const parts = rel.split('/')
    if (parts.length > 1) tops.add(parts[0])
  }
  if (tops.size >= 2) return 'cross-cutting'
  if (scope === 'local' && files.length > LOCAL_FILE_LIMIT) return 'module'
  return scope
}

// Folds one piece of evidence into a task: what it records, and the
// reassessment it triggers. Only a failed check after an edit escalates.
export function onEvidence(
  state: RouterState,
  task: TaskState,
  evidence: Evidence,
  cwd: string | undefined,
): { task: TaskState; reassess: ReassessRecord | undefined } {
  let t = clone(task)
  if (evidence.kind === 'edit') {
    t.hasEdit = true
    t.editedSinceCheck = true
    if (evidence.file && !t.files.includes(evidence.file)) t.files.push(evidence.file)
    const scope = grownScope(t.class.scope, t.files, cwd)
    if (scope === t.class.scope) return { task: t, reassess: undefined }
    t.class = { ...t.class, scope }
    const out = rerun(state, t, 'scope_growth')
    return { task: out.task, reassess: out.record }
  }
  t.verifications.push({
    command: clip(evidence.command, 200),
    passed: evidence.passed,
    kind: evidence.checkKind ?? kindOfArgv(words(evidence.command)),
  })
  if (t.verifications.length > 20) t.verifications = t.verifications.slice(-20)
  const edited = t.editedSinceCheck
  t.editedSinceCheck = false
  if (evidence.passed) {
    t.lastCountedFailure = false
    return { task: t, reassess: undefined }
  }
  // A red run before any edit (test-first, a reproduction) is no evidence.
  if (!t.hasEdit) return { task: t, reassess: undefined }
  const event: ReassessEvent = t.lastCountedFailure && edited ? 'repair_failed' : 'failed_verification'
  t.lastCountedFailure = true
  const out = escalate(state, t, event)
  t = out.task
  return { task: t, reassess: out.record }
}

// --- subagents ------------------------------------------------------------------

export type SpawnDecision = {
  decision: Decision | undefined
  // Whether the router decided the subagent's model itself: only then does
  // it get a task of its own and its requests are routed.
  isRouted: boolean
  class: Class | undefined
}

// `personModel` is a model the PERSON named for subagents (the active main
// task's `subagentModel`). A model named inside the spawn's own prompt was
// written by the parent agent: a preference relais overrides, never a pin.
// `session` and `taskId` (the subagent task's id, minted before the spawn)
// key the exploration draw; without them nothing is drawn.
export function decideSpawn(input: {
  state: RouterState
  type: string
  cls: Classification | undefined
  personModel?: string | null
  session?: string | null
  taskId?: string
}): SpawnDecision {
  const { state, type, cls } = input
  const pinned = state.pins[type]
  if (pinned) {
    return {
      decision: { tier: tierOfModel(state, pinned) ?? 'implementation', model: pinned, effort: null, reason: 'pin', override: false, wouldPassGate: true },
      isRouted: false,
      class: cls && classOf(cls),
    }
  }
  const userModel = input.personModel ? resolveModel(state, input.personModel) : undefined
  if (!cls && !userModel) return { decision: undefined, isRouted: false, class: undefined }
  const c = cls ? classOf(cls) : undefined
  if (userModel) {
    return {
      decision: { tier: tierOfModel(state, userModel) ?? 'implementation', model: userModel, effort: null, reason: 'user_model', override: true, wouldPassGate: true },
      isRouted: false,
      class: c,
    }
  }
  if (!cls || !c) return { decision: undefined, isRouted: false, class: undefined }
  let tier = tableTier(state.capability_table, c)
  const model = state.tiers[tier].model
  if (cls.confidence < LOW_CONFIDENCE || state.excluded_models.includes(model)) {
    return { decision: { tier, model, effort: null, reason: 'abstained', override: false, wouldPassGate: true }, isRouted: false, class: c }
  }
  // Exploration: below epsilon the subagent starts one tier lower, never
  // below research, and never on a model the person excluded (then nothing
  // is drawn). A new subagent has no cache, so no gate applies.
  let exploration: Exploration | undefined
  if (input.taskId) {
    const lowered = stepDown(tier)
    if (lowered === tier || !state.excluded_models.includes(state.tiers[lowered].model)) {
      exploration = drawAt(state, input.session, input.taskId, tier)
      if (exploration?.explored) tier = lowered
    }
  }
  return {
    decision: { tier, model: state.tiers[tier].model, effort: state.tiers[tier].effort, reason: 'table', override: true, wouldPassGate: true, exploration },
    isRouted: true,
    class: c,
  }
}

// --- outcomes -------------------------------------------------------------------

export function outcomeOf(task: TaskState): Outcome {
  if (task.corrected) return 'corrected'
  const last = task.verifications[task.verifications.length - 1]
  if ((last && last.passed) || task.relaisAccepted) return 'completed_verified'
  if (task.acceptQuote !== null) return 'completed_accepted'
  // Nothing was edited and nothing could have been: a question, a review.
  // An aborted task says nothing about what it would have done.
  if (task.hasEdit || task.files.length > 0 || task.maybeWrote || task.inferred.includes('aborted')) return 'unknown'
  return 'answered'
}

// --- records (docs/router-protocol.md) ---------------------------------------------

export const iso = (ms: number) => new Date(ms).toISOString()

export function decisionRecord(input: {
  task: TaskState
  turnId: string | null
  agentId: string | null
  relation: Relation | 'subagent'
  decision: Decision
  mode: Mode
  holdout: boolean
  applied: boolean
  now: number
  // The capability table's version the decision was made under.
  tableVersion?: number
}) {
  return {
    kind: 'decision',
    task_id: input.task.id,
    turn_id: input.turnId,
    agent_id: input.agentId,
    relation: input.relation,
    class: input.task.class,
    tier: input.decision.tier,
    model: input.decision.model,
    effort: input.decision.effort,
    reason: input.decision.reason,
    mode_effective: input.mode,
    holdout: input.holdout,
    applied: input.applied,
    explored: input.decision.exploration?.explored ?? false,
    propensity: input.decision.exploration?.propensity ?? null,
    draw: input.decision.exploration?.draw ?? null,
    would_pass_gate: input.decision.wouldPassGate,
    table_version: input.tableVersion ?? null,
    at: iso(input.now),
  }
}

export function usageRecord(input: {
  taskId: string
  turnId: string | null
  step: number
  agentId: string | null
  source: 'step' | 'classifier'
  model: string
  usage: any
  now: number
}) {
  const n = (v: unknown) => (typeof v === 'number' && Number.isFinite(v) ? v : 0)
  return {
    kind: 'usage',
    task_id: input.taskId,
    turn_id: input.turnId,
    step: input.step,
    agent_id: input.agentId,
    source: input.source,
    model: input.model,
    input_tokens: n(input.usage?.input_tokens),
    output_tokens: n(input.usage?.output_tokens),
    cache_read_input_tokens: n(input.usage?.cache_read_input_tokens),
    cache_creation_input_tokens: n(input.usage?.cache_creation_input_tokens),
    at: iso(input.now),
  }
}

// The classifier input the contract allows: at most this many bytes.
export const CLASSIFICATION_INPUT_BYTES = 3072

let classifierVersionMemo: string | undefined

// The classifier's prompt version: the SHA-256 of its system prompt, so a
// training set says which prompt labelled each call.
export function classifierVersion(): string {
  classifierVersionMemo ??= sha256Hex(CLASSIFIER_SYSTEM)
  return classifierVersionMemo
}

// One classifier call, kept for training (docs/router-protocol.md): the
// text sent, and the parsed answer with every field, or null when the
// reply did not parse. relais keeps the text in a local file and only its
// hash in the ledger.
export function classificationRecord(input: {
  callId: string
  purpose: 'prompt' | 'spawn'
  model: string
  text: string
  cls: Classification | undefined
  latencyMs: number | null
  now: number
}) {
  const c = input.cls
  return {
    kind: 'classification',
    call_id: input.callId,
    purpose: input.purpose,
    model: input.model,
    prompt_version: classifierVersion(),
    input: clip(input.text, CLASSIFICATION_INPUT_BYTES),
    answer: c
      ? {
          relation: c.relation,
          kind: c.kind,
          difficulty: c.difficulty,
          scope: c.scope,
          uncertainty: c.uncertainty,
          verifiable: c.verifiable,
          explicit: { value: c.explicit.value, quote: c.explicit.quote },
          user_model: c.user_model,
          user_model_for: c.user_model_for,
          confidence: c.confidence,
        }
      : null,
    parse_ok: c !== undefined,
    latency_ms: input.latencyMs !== null && Number.isFinite(input.latencyMs) ? Math.max(0, Math.round(input.latencyMs)) : null,
    at: iso(input.now),
  }
}

export function reassessRecord(task: TaskState, r: ReassessRecord, now: number) {
  return { kind: 'reassess', task_id: task.id, agent_id: task.agentId, ...r, at: iso(now) }
}

export function taskRecord(task: TaskState, endedAt: number | null, cwd: string | undefined) {
  const outcome = outcomeOf(task)
  return {
    kind: 'task',
    task_id: task.id,
    agent_id: task.agentId,
    started_at: iso(task.startedAt),
    ended_at: endedAt === null ? null : iso(endedAt),
    class: task.class,
    outcome,
    // The outcome as the task ended: relais keeps the first one it gets, so
    // a later correction is measured against it (plan §5, missed failures).
    completed_at_end: endedAt === null ? null : outcome,
    inferred: task.inferred,
    files: [...new Set(task.files.map(file => fileHash(file, cwd)))].sort(),
    escalations: task.escalations,
    exhausted: task.exhausted,
    turns: task.turns,
    explicit_quote: task.explicitQuote,
    checks: task.verifications.map(v => ({ name: v.command, kind: v.kind ?? 'test', passed: v.passed })),
  }
}

// A parent's outcome for a subagent task that ended before it: relais
// applies it only over an `unknown` outcome of its own.
export function taskOutcomeRecord(sub: { taskId: string; agentId: string | null }, outcome: Outcome, at: number) {
  return { kind: 'task_outcome', task_id: sub.taskId, agent_id: sub.agentId, outcome, at: iso(at) }
}

// --- the amont gate stamp -----------------------------------------------------------------

// `relais native gate-stamp`'s document.
export type GateStamp = {
  stamped: boolean
  tree?: string
  passed?: { gate: string; epoch: number }[]
  failed?: { gate: string; epoch: number }[]
}

export function parseGateStamp(text: unknown): GateStamp | undefined {
  try {
    const raw = JSON.parse(String(text ?? ''))
    if (!raw || typeof raw.stamped !== 'boolean') return undefined
    if (!raw.stamped) return { stamped: false }
    const runs = (list: unknown) =>
      Array.isArray(list)
        ? list.filter((r: any) => r && typeof r.gate === 'string' && Number.isFinite(r.epoch)).map((r: any) => ({ gate: r.gate, epoch: Number(r.epoch) }))
        : []
    return { stamped: true, tree: typeof raw.tree === 'string' ? raw.tree : '', passed: runs(raw.passed), failed: runs(raw.failed) }
  } catch {
    return undefined
  }
}

// What a stamp says about one task: the runs inside its window [start, end]
// (ms; stamps are in whole seconds). A pass with no fail after it in the
// window is a passing check, a fail in the window a failed one; the
// check is named for the tree, so a later pass is the last word.
export function gateChecks(stamp: GateStamp, startedAt: number, endedAt: number): { name: string; kind: 'gate'; passed: boolean }[] {
  if (!stamp.stamped || !stamp.tree) return []
  const from = Math.floor(startedAt / 1000)
  const to = Math.ceil(endedAt / 1000)
  const inside = (r: { epoch: number }) => r.epoch >= from && r.epoch <= to
  const passes = (stamp.passed ?? []).filter(inside)
  const fails = (stamp.failed ?? []).filter(inside)
  const name = `amont-gate@${stamp.tree.slice(0, 7)}`
  const out: { name: string; kind: 'gate'; passed: boolean }[] = []
  if (fails.length > 0) out.push({ name, kind: 'gate', passed: false })
  const lastFail = Math.max(-Infinity, ...fails.map(r => r.epoch))
  if (passes.some(r => r.epoch > lastFail)) out.push({ name, kind: 'gate', passed: true })
  return out
}

// --- the module's memory (Store.router) ----------------------------------------------

export type RouterMemory = {
  // `relais native router-state`, or undefined (shadow, nothing decided).
  state: RouterState | undefined
  // The session the state and the queued records belong to.
  session: string | undefined
  cwd: string | undefined
  isStale: boolean
  refreshing: Promise<void> | undefined
  task: TaskState | undefined
  // Router-created subagents' tasks, by agentId.
  subtasks: Record<string, TaskState>
  // Spawns the engine answered without an agentId, by description: keyed at
  // their first request; two with one description are left unkeyed.
  unkeyed: Record<string, { key: string; task: TaskState; ambiguous: boolean }>
  pendingRoute: { seq: number; promise: Promise<unknown> } | undefined
  route: { turnId: string; decision: Decision | undefined } | undefined
  seq: number
  forceNew: boolean
  isTaskEndRequested: boolean
  lastModel: string | undefined
  lastContext: number
  queue: { session: string; record: unknown }[]
  status: string | undefined
  // Whether the installed relais accepts `answered`: undefined until asked.
  supportsAnswered: boolean | undefined
}

export const createRouterMemory = (): RouterMemory => ({
  state: undefined,
  session: undefined,
  cwd: undefined,
  isStale: false,
  refreshing: undefined,
  task: undefined,
  subtasks: {},
  unkeyed: {},
  pendingRoute: undefined,
  route: undefined,
  seq: 0,
  forceNew: false,
  isTaskEndRequested: false,
  lastModel: undefined,
  lastContext: 0,
  queue: [],
  status: undefined,
  supportsAnswered: undefined,
})

// The status line's routing part.
export function routeStatus(decision: Decision | undefined, mode: Mode, holdout: boolean): string | undefined {
  if (!decision) return undefined
  const suffix = mode !== 'on' ? ` · ${mode}` : holdout ? ' · holdout' : ''
  return `relais · ${decision.tier} (${shortModel(decision.model)})${suffix}`
}
