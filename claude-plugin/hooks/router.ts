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
export type Outcome = 'completed_verified' | 'completed_accepted' | 'corrected' | 'unknown'
export type ReassessEvent = 'failed_verification' | 'repair_failed' | 'scope_growth' | 'spawn' | 'correction' | 'flag'

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
  r3: { id: string; passed: boolean } | null
  holdout: boolean
  tiers: Record<Tier, { model: string; effort: Effort | null }>
  capability_table: { version: number; rules: { when: Record<string, unknown>; tier: Tier }[] }
  rates: Record<string, { input: number; output: number; cache_read: number; cache_write: number } | null>
  priors: { task_tokens_by_difficulty: number[]; task_tokens: Record<string, number> }
  pins: Record<string, string>
  excluded_models: string[]
  checks: string[][]
}

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
  // Edits since the last verification, and whether the last verification
  // that counted as escalating evidence failed with no pass since.
  editedSinceCheck: boolean
  lastCountedFailure: boolean
  verifications: { command: string; passed: boolean }[]
  files: string[]
  subagents: string[]
  corrected: boolean
  acceptQuote: string | null
  // The person's latest explicit words (accept or correct), for the record.
  explicitQuote: string | null
  relaisAccepted: boolean
  inferred: string[]
  lastReason: Reason
}

export type Decision = {
  tier: Tier
  model: string
  effort: Effort | null
  reason: Reason
  // Whether a request is to be rewritten at all; applied only when the
  // mode is `on` and the session is not held out.
  override: boolean
  wouldPassGate: boolean
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

// Fixed, so the prompt cache holds it from one call to the next.
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
    r3: raw.r3 && typeof raw.r3.id === 'string' ? { id: raw.r3.id, passed: raw.r3.passed === true } : null,
    holdout: raw.holdout === true,
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
    checks: Array.isArray(raw.checks)
      ? raw.checks.filter((c: unknown) => Array.isArray(c) && c.length > 0 && c.every(a => typeof a === 'string'))
      : [],
  }
}

// --- the mode ---------------------------------------------------------------

export type Consents = { envelope: unknown; r3: unknown }

// The narrower of relais's mode and the plugin's own two store records: an
// envelope or an R3 pass that reached machine.toml or the ledger any other
// way than through the person's answer here switches nothing on.
export function modeEffective(state: RouterState | undefined, consents: Consents): Mode {
  if (!state) return 'shadow'
  if (state.mode === 'off') return 'off'
  if (state.mode !== 'on') return 'shadow'
  const envelope = consents.envelope as any
  const r3 = consents.r3 as any
  const hasEnvelope = !!envelope && typeof envelope === 'object' && typeof envelope.answer === 'string'
  const hasR3 = !!r3 && typeof r3 === 'object' && !!state.r3 && state.r3.passed && r3.id === state.r3.id
  return hasEnvelope && hasR3 ? 'on' : 'shadow'
}

// Why the mode stays shadow although relais says on, for the one toast.
export function shadowWhy(state: RouterState | undefined, consents: Consents): string | undefined {
  if (!state || state.mode === 'off') return undefined
  const envelope = consents.envelope as any
  if (state.envelope && !(envelope && typeof envelope.answer === 'string')) {
    return 'session routing stays in shadow: the envelope in machine.toml was not granted through /relais-routing here'
  }
  if (state.mode === 'on' && modeEffective(state, consents) !== 'on') {
    return 'session routing stays in shadow: the R3 pass was not recorded through /relais-r3 here'
  }
  return undefined
}

// --- tiers and models -------------------------------------------------------

export const rank = (tier: Tier) => TIERS.indexOf(tier)
export const higher = (a: Tier, b: Tier): Tier => (rank(a) >= rank(b) ? a : b)
const stepUp = (tier: Tier): Tier => TIERS[Math.min(TIERS.length - 1, rank(tier) + 1)]

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

export function decisionOf(state: RouterState, task: TaskState, reason: Reason, wouldPassGate = true): Decision {
  return {
    tier: task.tier,
    model: task.userModel ?? state.tiers[task.tier].model,
    effort: task.userModel ? null : effortOf(state, task),
    reason,
    override: task.override,
    wouldPassGate,
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
    editedSinceCheck: false,
    lastCountedFailure: false,
    verifications: [],
    files: [],
    subagents: [],
    corrected: false,
    acceptQuote: null,
    explicitQuote: null,
    relaisAccepted: false,
    inferred: [],
    lastReason: 'table',
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
  const raised = higher(task.tier, tableTier(state.capability_table, task.class))
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
  return { task, ended, decision: decisionOf(state, task, reason, wouldPassGate), relation: 'new_task', reassess: [] }
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
  const raised = higher(t.tier, tableTier(state.capability_table, t.class))
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
  for (const segment of command.split(/&&|;|\n/)) {
    const argv = words(segment.trim())
    while (argv.length > 0 && /^[A-Za-z_][A-Za-z0-9_]*=/.test(argv[0])) argv.shift()
    if (checks.some(prefix => prefix.length <= argv.length && prefix.every((w, i) => argv[i] === w))) return true
  }
  return false
}

// What a finished tool call says, or undefined when it is no evidence.
export type Evidence =
  | { kind: 'edit'; file: string | undefined }
  | { kind: 'check'; command: string; passed: boolean }

export function evidenceOf(tool: string, e: any, result: any, checks: string[][]): Evidence | undefined {
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
  if (!matchesCheck(command, checks)) return undefined
  if (result.isError) {
    // S0: a failing command is `isError` with "Exit code N"; an error
    // without one (interrupted, refused) says nothing about the code.
    return /Exit code \d+/.test(String(result.text ?? result.result ?? '')) ? { kind: 'check', command, passed: false } : undefined
  }
  return { kind: 'check', command, passed: true }
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
  t.verifications.push({ command: clip(evidence.command, 200), passed: evidence.passed })
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
export function decideSpawn(input: {
  state: RouterState
  type: string
  cls: Classification | undefined
  personModel?: string | null
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
  const tier = tableTier(state.capability_table, c)
  const model = state.tiers[tier].model
  if (cls.confidence < LOW_CONFIDENCE || state.excluded_models.includes(model)) {
    return { decision: { tier, model, effort: null, reason: 'abstained', override: false, wouldPassGate: true }, isRouted: false, class: c }
  }
  return { decision: { tier, model, effort: state.tiers[tier].effort, reason: 'table', override: true, wouldPassGate: true }, isRouted: true, class: c }
}

// --- outcomes -------------------------------------------------------------------

export function outcomeOf(task: TaskState): Outcome {
  if (task.corrected) return 'corrected'
  const last = task.verifications[task.verifications.length - 1]
  if ((last && last.passed) || task.relaisAccepted) return 'completed_verified'
  if (task.acceptQuote !== null) return 'completed_accepted'
  return 'unknown'
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
    explored: false,
    propensity: null,
    draw: null,
    would_pass_gate: input.decision.wouldPassGate,
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

export function reassessRecord(task: TaskState, r: ReassessRecord, now: number) {
  return { kind: 'reassess', task_id: task.id, agent_id: task.agentId, ...r, at: iso(now) }
}

export function taskRecord(task: TaskState, endedAt: number | null) {
  return {
    kind: 'task',
    task_id: task.id,
    agent_id: task.agentId,
    started_at: iso(task.startedAt),
    ended_at: endedAt === null ? null : iso(endedAt),
    class: task.class,
    outcome: outcomeOf(task),
    inferred: task.inferred,
    escalations: task.escalations,
    exhausted: task.exhausted,
    turns: task.turns,
    explicit_quote: task.explicitQuote,
  }
}

// --- the module's memory (Store.router) ----------------------------------------------

export type RouterMemory = {
  // `relais native router-state`, or undefined (shadow, nothing decided).
  state: RouterState | undefined
  // The plugin's own two store records, read with the state.
  consents: { envelope: unknown; r3: unknown }
  // The session the state and the queued records belong to.
  session: string | undefined
  cwd: string | undefined
  isStale: boolean
  refreshing: Promise<void> | undefined
  hasToastedShadow: boolean
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
}

export const createRouterMemory = (): RouterMemory => ({
  state: undefined,
  consents: { envelope: undefined, r3: undefined },
  session: undefined,
  cwd: undefined,
  isStale: false,
  refreshing: undefined,
  hasToastedShadow: false,
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
})

// The status line's routing part.
export function routeStatus(decision: Decision | undefined, mode: Mode, holdout: boolean): string | undefined {
  if (!decision) return undefined
  const suffix = mode !== 'on' ? ` · ${mode}` : holdout ? ' · holdout' : ''
  return `relais · ${decision.tier} (${shortModel(decision.model)})${suffix}`
}
