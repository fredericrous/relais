// An agent's transcript inside the pane: the rows of `$.session.messages
// ({ agentId })` laid out at the pane's width (pure), and the guarded read
// that fetches them and refreshes them outside the flush.

import type { Fx } from './fx.ts'
import { activityText, note } from './agents.ts'
import { cutLine, shortId } from './timeline.ts'
import { detach, type Store } from './store.ts'
import { flush, markDirty } from './ui.ts'

export type TranscriptRow = { text: string; dim?: boolean; color?: string }

// What the pane stores of a read: the laid-out rows (with, after a later
// deny or slow, the line that says so), a deny's reason, or a slow read.
export type TranscriptState =
  | { kind: 'loading' }
  | { kind: 'rows'; rows: TranscriptRow[]; note?: string }
  | { kind: 'deny'; reason: string }
  | { kind: 'slow' }

// holds-until: a transcript the person wants to read whole in the pane;
// the full one is Claude Code's own view (`↓ to manage`).
export const MAX_TRANSCRIPT_ROWS = 400
export const MAX_MESSAGE_ROWS = 12
export const MAX_RESULT_ROWS = 3
export const READ_BOUND_MS = 5000
export const REFRESH_MS = 1000
export const SLOW_TEXT = 'transcript slow · ↓ to manage: full view'
export const denyText = (reason: string) => `transcript unavailable · ${reason}`

const PERSON = 'person │ '
const AGENT = 'agent  │ '
const CONTINUED = '       │ '
const RESULT = '  │ '

// A row is at most `columns × 3` bytes: a one-cell character is at most 3
// bytes, so combining marks cannot push a stored row past the bound.
const encoder = new TextEncoder()
const utf8Bytes = (text: string): number => encoder.encode(text).length

export function cutBytes(text: string, columns: number): string {
  const limit = Math.max(1, columns) * 3
  if (utf8Bytes(text) <= limit) return text
  let out = ''
  let bytes = 0
  for (const ch of text) {
    const n = utf8Bytes(ch)
    if (bytes + n > limit) break
    out += ch
    bytes += n
  }
  return out
}

// `text` cut to `width` code points, the last one `…` when it was cut.
function cutCells(text: string, width: number): string {
  const chars = Array.from(text)
  if (chars.length <= width) return text
  return chars.slice(0, Math.max(1, width - 1)).join('') + '…'
}

// One line wrapped at `width` code points, at a space when there is one.
function wrap(line: string, width: number): string[] {
  const chars = Array.from(line)
  if (chars.length <= width) return [line]
  const out: string[] = []
  let rest = chars
  while (rest.length > width) {
    const space = rest.slice(0, width + 1).lastIndexOf(' ')
    const at = space > 0 ? space : width
    out.push(rest.slice(0, at).join(''))
    rest = rest.slice(space > 0 ? at + 1 : at)
  }
  out.push(rest.join(''))
  return out
}

const linesOf = (text: string): string[] => {
  const lines = text.split('\n').map(cutLine)
  while (lines.length > 0 && lines[lines.length - 1].trim() === '') lines.pop()
  return lines
}

function messageRows(m: any, columns: number): TranscriptRow[] {
  const out: TranscriptRow[] = []
  const text = typeof m?.text === 'string' ? m.text : ''
  const lead = m?.role === 'user' ? PERSON : AGENT
  if (text.trim() !== '') {
    const room = Math.max(1, columns - lead.length)
    const wrapped = linesOf(text).flatMap(line => wrap(line, room))
    wrapped.slice(0, MAX_MESSAGE_ROWS).forEach((row, i) => out.push({ text: (i === 0 ? lead : CONTINUED) + row }))
    if (wrapped.length > MAX_MESSAGE_ROWS) {
      out.push({ text: `${CONTINUED}… ${wrapped.length - MAX_MESSAGE_ROWS} more rows`, dim: true })
    }
  }
  const uses: any[] = Array.isArray(m?.toolUses) ? m.toolUses : []
  for (const use of uses) {
    const what = cutLine(activityText({ ...(use?.input ?? {}), tool: use?.tool }))
    const isRunning = typeof use?.text !== 'string'
    const suffix = ' … running'
    const row = isRunning
      ? cutCells(`▸ ${what}`, Math.max(1, columns - suffix.length)) + suffix
      : cutCells(`▸ ${what}`, columns)
    out.push({ text: row })
    if (isRunning) continue
    const lines = linesOf(use.text)
    const failed = use.isError === true
    if (lines.length === 0 && failed) lines.push('error')
    lines.slice(0, MAX_RESULT_ROWS).forEach((line, i) => {
      if (failed && i === 0) out.push({ text: cutCells(`FAIL ${line}`, columns), color: 'error' })
      else out.push({ text: cutCells(RESULT + line, columns), dim: true })
    })
    if (lines.length > MAX_RESULT_ROWS) {
      out.push({ text: `${RESULT}… ${lines.length - MAX_RESULT_ROWS} more rows`, dim: true })
    }
  }
  return out
}

// The newest `cap` rows of the messages, laid out at `columns`. Pure.
export function transcriptRows(messages: readonly unknown[], columns: number, cap: number): TranscriptRow[] {
  const width = Math.max(8, columns)
  let rows: TranscriptRow[] = []
  // From the end: older messages are not laid out once the cap is filled.
  for (let i = messages.length - 1; i >= 0 && rows.length < cap; i--) {
    rows = [...messageRows(messages[i], width), ...rows]
  }
  return rows.slice(-cap).map(row => ({ ...row, text: cutBytes(row.text, width) }))
}

export type ReadResult =
  | { kind: 'rows'; rows: TranscriptRow[] }
  | { kind: 'deny'; reason: string }
  | { kind: 'slow' }

// Reads the agent's messages, at most READ_BOUND_MS: a read that passes it is
// `slow`, a deny and every throw are `deny`. `settled(isLate)` runs when the
// real call settles, however long after.
export async function readTranscript(
  fx: Fx,
  agentId: string,
  columns: number,
  settled?: (isLate: boolean) => void,
): Promise<ReadResult> {
  let isLate = false
  let call: Promise<unknown>
  try {
    call = Promise.resolve(fx.session.messages({ agentId }))
  } catch (reason) {
    call = Promise.reject(reason)
  }
  const real = call.then(
    (value): { value: unknown } => ({ value }),
    (error): { error: unknown } => ({ error }),
  )
  detach(real.then(() => settled?.(isLate)))
  let timer: { cancel: () => void } | undefined
  const bound = new Promise<'slow'>(resolve => {
    timer = fx.clock.after(READ_BOUND_MS, () => {
      isLate = true
      resolve('slow')
    })
  })
  const first = await Promise.race([real, bound])
  timer?.cancel()
  if (first === 'slow') return { kind: 'slow' }
  const reason = (text: string): ReadResult => ({ kind: 'deny', reason: cutBytes(cutLine(text), columns) })
  if ('error' in first) return reason(String((first.error as any)?.message ?? first.error))
  const value: any = first.value
  if (Array.isArray(value)) return { kind: 'rows', rows: transcriptRows(value, columns, MAX_TRANSCRIPT_ROWS) }
  if (typeof value?.deny === 'string') return reason(value.deny)
  return reason('the transcript could not be read')
}

// One tick: reads the shown agent's transcript when it is due. Outside the
// flush; the read is detached and guarded by the two flags.
export async function refreshTranscript(fx: Fx, store: Store) {
  const shown = store.shown
  if (store.closed || shown.kind !== 'agent') return
  if (store.isReadingTranscript || store.pendingMessages) return
  // Held before the first await, so two overlapping pumps cannot both read;
  // released below when no read is due.
  store.isReadingTranscript = true
  let now: number
  try {
    now = await fx.clock.now()
  } catch (reason) {
    store.isReadingTranscript = false
    throw reason
  }
  const row = Object.values(store.models)
    .flatMap(m => m.agents)
    .find(a => a.agentId === shown.agentId)
  const isRunning = row?.status === 'running'
  const isChanged = shown.status !== undefined && row?.status !== shown.status
  const isDue =
    store.transcriptDirty ||
    isChanged ||
    !shown.hasLanded ||
    (isRunning && now - shown.refreshedAt >= REFRESH_MS)
  if (!isDue) {
    store.isReadingTranscript = false
    return
  }
  shown.refreshedAt = now
  shown.status = row?.status
  store.transcriptDirty = false
  store.isReadingTranscript = true
  store.pendingMessages = true
  detach(
    (async () => {
      try {
        const result = await readTranscript(fx, shown.agentId, shown.columns, isLate => {
          store.pendingMessages = false
          // A view opened while this call was pending reads once it is over.
          const current = store.shown
          const isSameView = current.kind === 'agent' && current.generation === shown.generation
          if (current.kind === 'agent' && current.agentId === shown.agentId && (isLate || !isSameView)) {
            store.transcriptDirty = true
          }
        })
        await storeResult(fx, store, shown.generation, shown.agentId, result)
      } catch (reason) {
        // A failed store or flush says why the view stopped updating, once,
        // on the run; the note itself failing leaves nothing else to tell.
        const message = String((reason as any)?.message ?? reason)
        await note(fx, store, shown.run, `agent ${shortId(shown.agentId)}: transcript read failed: ${message}`).catch(
          () => undefined,
        )
      } finally {
        store.isReadingTranscript = false
      }
    })(),
  )
}

// Stores a read's result for the view that began it; a stale one is dropped.
async function storeResult(fx: Fx, store: Store, generation: number, agentId: string, result: ReadResult) {
  const current = store.shown
  if (store.closed || current.kind !== 'agent' || current.generation !== generation) return
  const line = result.kind === 'deny' ? denyText(result.reason) : result.kind === 'slow' ? SLOW_TEXT : undefined
  const prior = current.transcript
  if (result.kind === 'rows') current.transcript = { kind: 'rows', rows: result.rows }
  else if (prior.kind === 'rows') current.transcript = { kind: 'rows', rows: prior.rows, note: line }
  else current.transcript = result.kind === 'deny' ? { kind: 'deny', reason: result.reason } : { kind: 'slow' }
  current.hasLanded = true
  markDirty(store)
  await flush(fx, store)
  if (result.kind === 'rows' || line === undefined) return
  // One note on the run per view and reason, so it shows in the status reply.
  const key = `${generation}:${result.kind}`
  if (store.notedReasons.has(key)) return
  store.notedReasons.add(key)
  await note(fx, store, current.run, `agent ${shortId(agentId)}: ${line}`)
}
