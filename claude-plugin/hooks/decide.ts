// The decide tool (ADR-0004): the orchestrating session records the answer
// to a run waiting on a person, with an actor relais types and a proof that
// the call came through this tool. relais judges what may be answered; this
// module only sets who answered and proves the call was its own.

import { note } from './agents.ts'
import type { Fx } from './fx.ts'
import type { Store } from './store.ts'

const DECIDE_TIMEOUT_MS = 60_000
const NONCE_TIMEOUT_MS = 10_000
// A quote the person's prompt must contain, in whole words: long enough
// that "y" or "ok" cannot pass for one.
const MIN_QUOTE_CHARS = 12
export const ANSWERS = ['approve', 'revise', 'reject', 'salvaged', 'abandon', 'decided'] as const

export const DECIDE_TOOL_DESCRIPTION =
  "Record the answer to a relais run waiting on a decision (needs_decision, needs_review, interrupted): approve, revise, reject, salvaged, abandon or decided. A decision is not made until it is recorded here. Answer it yourself when the request or the approved plan already answers it; otherwise ask the person, and record their answer with on_behalf_of_person and their own words as person_quote. relais refuses what may not be answered (an approval with open verification gaps) and says why."

export const DECIDE_INPUT_SCHEMA = {
  type: 'object',
  properties: {
    run: { type: 'string', description: 'The run id.' },
    answer: { type: 'string', enum: [...ANSWERS], description: 'What was decided.' },
    note: { type: 'string', description: 'Why: the evidence the answer rests on.' },
    successor: { type: 'string', description: 'The run that carries a revision, for revise.' },
    candidate: { type: 'string', description: 'The commit sha that landed the candidate, for salvaged.' },
    criterion: { type: 'string', description: "An acceptance criterion the person signs off (approve only; the person's answer only)." },
    on_behalf_of_person: { type: 'boolean', description: 'The person gave this answer in their last message.' },
    person_quote: { type: 'string', description: "The person's own words from their last message, verbatim (at least 12 characters)." },
  },
  required: ['run', 'answer'],
}

// The person's words since the run's outcome reached this session, checked
// against what they actually typed: a quote is accepted only from the
// person's latest prompt, sent after the outcome was delivered here, and
// only as whole words of it.
export function quoteRefusal(store: Store, run: string, quote: unknown): string | undefined {
  if (typeof quote !== 'string' || quote.trim().length < MIN_QUOTE_CHARS) {
    return `on_behalf_of_person needs person_quote: the person's own words from their last message, at least ${MIN_QUOTE_CHARS} characters.`
  }
  const delivered = store.outcomeDeliveredAt.get(run)
  if (delivered === undefined) {
    return `run ${run}'s outcome was not delivered in this session, so no answer here can be the person's to it. Record your own answer, or let the person answer it from their terminal.`
  }
  const prompt = store.lastPersonPrompt
  if (!prompt || prompt.at <= delivered) {
    return `the person has not written since run ${run}'s outcome arrived. Ask them, then record their answer.`
  }
  if (!containsWords(prompt.text, quote.trim())) {
    return "person_quote is not in the person's last message, word for word."
  }
  return undefined
}

// Whether `quote` occurs in `text` starting and ending at word boundaries,
// ignoring case and runs of whitespace.
export function containsWords(text: string, quote: string): boolean {
  const flat = (s: string) => s.replace(/\s+/g, ' ').toLowerCase()
  const hay = flat(text)
  const needle = flat(quote)
  const isWord = (ch: string | undefined) => ch !== undefined && /[\p{L}\p{N}_]/u.test(ch)
  for (let at = hay.indexOf(needle); at !== -1; at = hay.indexOf(needle, at + 1)) {
    const before = at === 0 ? undefined : hay[at - 1]
    const after = hay[at + needle.length]
    if (!(isWord(before) && isWord(needle[0])) && !(isWord(after) && isWord(needle[needle.length - 1]))) return true
  }
  return false
}

// The session's nonce: minted once, at session start. A mint that fails
// leaves none, and the tool's answers then record `cli`; said once.
export async function mintNonce(fx: Fx, store: Store, session: string) {
  try {
    const result = await fx.process.run(['relais', 'native', 'decide-nonce', '--session', session], {
      timeoutMs: NONCE_TIMEOUT_MS,
    })
    const nonce = String(result.stdout ?? '').trim()
    if (result.exitCode === 0 && /^[0-9a-f]{64}$/.test(nonce)) {
      store.decideNonce = { session, nonce }
      return
    }
    await note(fx, store, undefined, `could not mint the decide proof (exit ${result.exitCode}); answers recorded here will read recorded_via=cli`)
  } catch (reason) {
    await note(fx, store, undefined, `could not mint the decide proof (${String((reason as any)?.message ?? reason).slice(0, 200)}); answers recorded here will read recorded_via=cli`)
  }
}

// Forget the session's nonce: its session ended. Fails open.
export async function endNonce(fx: Fx, store: Store) {
  const held = store.decideNonce
  store.decideNonce = undefined
  if (!held) return
  try {
    await fx.process.run(['relais', 'native', 'decide-nonce', '--session', held.session, '--end'], {
      timeoutMs: NONCE_TIMEOUT_MS,
    })
  } catch {
    // The nonce expires on its own (relais sweeps old ones when it mints).
  }
}

export type DecideInput = {
  run?: unknown
  answer?: unknown
  note?: unknown
  successor?: unknown
  candidate?: unknown
  criterion?: unknown
  on_behalf_of_person?: unknown
  person_quote?: unknown
  cwd?: unknown
}

// The decide tool: `{deny}` for an input it cannot pass on, else
// `{result}` with relais's own answer, a refusal included, as text.
export async function decideTool(fx: Fx, store: Store, input: DecideInput): Promise<{ deny: string } | { result: string }> {
  const run = input.run
  const answer = input.answer
  if (typeof run !== 'string' || run === '') return { deny: 'relais decide needs run: the run id.' }
  if (typeof answer !== 'string' || !(ANSWERS as readonly string[]).includes(answer)) {
    return { deny: `relais decide needs answer: one of ${ANSWERS.join(', ')}.` }
  }
  const forPerson = input.on_behalf_of_person === true
  if (forPerson) {
    const refused = quoteRefusal(store, run, input.person_quote)
    if (refused) return { deny: refused }
  }
  const actor = forPerson ? 'person-via-orchestrator' : 'orchestrator'
  const argv = ['relais', 'decide', run, '--answer', answer, '--actor', actor, '--json']
  if (forPerson) argv.push('--person-quote', String(input.person_quote).trim())
  for (const [flag, value] of [
    ['--note', input.note],
    ['--successor', input.successor],
    ['--candidate', input.candidate],
    ['--criterion', input.criterion],
  ] as const) {
    if (typeof value === 'string' && value !== '') argv.push(flag, value)
  }
  const proof = store.decideNonce
  if (proof) argv.push('--proof-stdin')
  const result = await fx.process.run(argv, {
    timeoutMs: DECIDE_TIMEOUT_MS,
    ...(proof ? { stdin: JSON.stringify({ session: proof.session, nonce: proof.nonce }) } : {}),
  })
  let reply: any
  try {
    reply = JSON.parse(String(result.stdout ?? '').trim())
  } catch {
    const why = String(result.stderr ?? '').trim().slice(0, 1000)
    return { result: `relais decide failed (exit ${result.exitCode})${why ? `: ${why}` : ''}` }
  }
  if (typeof reply?.error === 'string') {
    return { result: `relais refused the answer (exit ${reply.exit ?? result.exitCode}): ${reply.error}` }
  }
  const advice = reply?.advice && typeof reply.advice.p_approve === 'number' ? `, advice p(approve)=${reply.advice.p_approve.toFixed(2)}` : ''
  await note(fx, store, run, `decided ${answer} for ${run} as ${reply?.actor ?? actor}${advice}`)
  return {
    result: `Recorded ${reply?.resolution ?? answer} for ${run} by ${reply?.actor ?? actor}: ${reply?.from_state ?? '?'} -> ${reply?.to_state ?? '?'}.`,
  }
}

export const FEEDBACK_OUTCOMES = ['reverted', 'regression', 'corrected'] as const

export const FEEDBACK_TOOL_DESCRIPTION =
  'Record what became of a change relais accepted, the moment you revert it, fix it or find it regressed: reverted, regression (a confirmed regression) or corrected (needs magnitude, 0 to 1, how large the correction was). Name the run, or the task whose accepted run it is. evidence is a commit sha or a note; relais refuses what it cannot record and says why.'

export const FEEDBACK_INPUT_SCHEMA = {
  type: 'object',
  properties: {
    run: { type: 'string', description: 'The run id whose accepted change this is about (or task).' },
    task: { type: 'string', description: 'The task whose accepted run this is about (or run).' },
    outcome: { type: 'string', enum: [...FEEDBACK_OUTCOMES], description: 'What happened to the change.' },
    evidence: { type: 'string', description: 'A commit sha, URL or note supporting it.' },
    candidate: { type: 'string', description: 'The candidate this is about; relais checks it against the run.' },
    magnitude: { type: 'number', description: 'How large the correction was; required for corrected.' },
    note: { type: 'string', description: 'Free-form context.' },
  },
  required: ['outcome'],
}

export type FeedbackInput = {
  run?: unknown
  task?: unknown
  outcome?: unknown
  evidence?: unknown
  candidate?: unknown
  magnitude?: unknown
  note?: unknown
}

// The feedback tool (ADR-0004, D23): the orchestrator reports what became
// of an accepted change. The actor is fixed here, never typed by the
// model, and the nonce travels on stdin only.
export async function feedbackTool(fx: Fx, store: Store, input: FeedbackInput): Promise<{ deny: string } | { result: string }> {
  const outcome = input.outcome
  if (typeof outcome !== 'string' || !(FEEDBACK_OUTCOMES as readonly string[]).includes(outcome)) {
    return { deny: `relais feedback needs outcome: one of ${FEEDBACK_OUTCOMES.join(', ')}.` }
  }
  const run = typeof input.run === 'string' && input.run !== '' ? input.run : undefined
  const task = typeof input.task === 'string' && input.task !== '' ? input.task : undefined
  if ((run === undefined) === (task === undefined)) return { deny: 'relais feedback needs exactly one of run (the run id) or task.' }
  const magnitude = typeof input.magnitude === 'number' && Number.isFinite(input.magnitude) ? input.magnitude : undefined
  if (outcome === 'corrected' && magnitude === undefined) return { deny: 'outcome `corrected` requires a correction magnitude' }
  const argv = ['relais', 'feedback', ...(run !== undefined ? [run] : ['--task', task!]), '--outcome', outcome, '--actor', 'orchestrator', '--json']
  if (magnitude !== undefined) argv.push('--magnitude', String(magnitude))
  for (const [flag, value] of [
    ['--evidence', input.evidence],
    ['--candidate', input.candidate],
    ['--note', input.note],
  ] as const) {
    if (typeof value === 'string' && value !== '') argv.push(flag, value)
  }
  const proof = store.decideNonce
  if (proof) argv.push('--proof-stdin')
  const result = await fx.process.run(argv, {
    timeoutMs: DECIDE_TIMEOUT_MS,
    ...(proof ? { stdin: JSON.stringify({ session: proof.session, nonce: proof.nonce }) } : {}),
  })
  let reply: any
  try {
    reply = JSON.parse(String(result.stdout ?? '').trim())
  } catch {
    const why = String(result.stderr ?? '').trim().slice(0, 1000)
    return { result: `relais feedback failed (exit ${result.exitCode})${why ? `: ${why}` : ''}` }
  }
  if (typeof reply?.error === 'string') {
    return { result: `relais refused the feedback (exit ${reply.exit ?? result.exitCode}): ${reply.error}` }
  }
  const subject = run ?? `task ${task}`
  await note(fx, store, run, `recorded ${outcome} for ${subject} as ${reply?.actor ?? 'orchestrator'}`)
  return { result: `Recorded ${reply?.outcome ?? outcome} for ${reply?.run ?? subject} by ${reply?.actor ?? 'orchestrator'} (source ${reply?.source ?? '?'}).` }
}
