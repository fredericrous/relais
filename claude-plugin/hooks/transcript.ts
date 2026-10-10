// An agent's transcript inside the pane: the rows of `$.session.messages
// ({ agentId })` laid out at the pane's width (pure), and the guarded read
// that fetches them and refreshes them outside the flush.

import type { Fx } from './fx.ts'
import { activityText, note } from './agents.ts'
import { cutLine, shortId } from './timeline.ts'
import { detach, type Store } from './store.ts'
import { flush, markDirty } from './ui.ts'
import { MAX_TRANSCRIPT_ROWS } from './limits.ts'

export type TranscriptRow = { text: string; dim?: boolean; color?: string }

// What the pane stores of a read: the laid-out rows (with, after a later
// deny or slow, the line that says so), a deny's reason, or a slow read.
export type TranscriptState =
  | { kind: 'loading' }
  | { kind: 'rows'; rows: TranscriptRow[]; note?: string }
  | { kind: 'deny'; reason: string }
  | { kind: 'slow' }

export { MAX_TRANSCRIPT_ROWS }
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
const FAIL = 'FAIL '
const TOOL = '▸ '
const TOOL_CONTINUED = '  '
const RUNNING = ' … running'
const MAX_TOOL_ROWS = 2

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

// The terminal cells one code point takes: 0 for a combining mark or a
// zero-width joiner or selector, 2 for an East Asian wide or fullwidth
// character or an emoji, else 1.
// holds-until: a script the ranges below miss draws as one cell too few;
// then the full Unicode East Asian Width table replaces them.
const ZERO_WIDTH = /^[\p{Mn}\p{Me}​-‏⁠︀-️]$/u
const EMOJI = /^\p{Extended_Pictographic}$/u
const WIDE_RANGES: ReadonlyArray<readonly [number, number]> = [
  [0x1100, 0x115f],
  [0x2e80, 0x303e],
  [0x3041, 0x33ff],
  [0x3400, 0x4dbf],
  [0x4e00, 0x9fff],
  [0xa000, 0xa4cf],
  [0xac00, 0xd7a3],
  [0xf900, 0xfaff],
  [0xfe30, 0xfe4f],
  [0xff00, 0xff60],
  [0xffe0, 0xffe6],
  [0x20000, 0x3fffd],
]

export function cellsOf(ch: string): number {
  if (ZERO_WIDTH.test(ch)) return 0
  const code = ch.codePointAt(0) ?? 0
  if (WIDE_RANGES.some(([low, high]) => code >= low && code <= high)) return 2
  // Pictographs below U+2600 (©, ®, ‼) are narrow unless a selector asks.
  return code >= 0x2600 && EMOJI.test(ch) ? 2 : 1
}

export const widthOf = (text: string): number => Array.from(text).reduce((n, ch) => n + cellsOf(ch), 0)

// The longest start of `chars` that fits in `width` cells: how many code points.
function fitting(chars: readonly string[], width: number): number {
  let cells = 0
  let count = 0
  for (const ch of chars) {
    const n = cellsOf(ch)
    if (cells + n > width) break
    cells += n
    count += 1
  }
  return count
}

// One line wrapped at `width` cells, at a space when there is one in the
// row; a run with no space (a path, CJK text) breaks at the width.
function wrap(line: string, width: number): string[] {
  if (widthOf(line) <= width) return [line]
  const out: string[] = []
  let rest = Array.from(line)
  while (widthOf(rest.join('')) > width) {
    const fit = Math.max(1, fitting(rest, width))
    const space = rest.slice(0, fit + 1).lastIndexOf(' ')
    const at = space > 0 ? space : fit
    out.push(rest.slice(0, at).join(''))
    rest = rest.slice(space > 0 ? at + 1 : at)
  }
  out.push(rest.join(''))
  return out
}

// `text` wrapped at `width` cells into at most `rows` rows; when more would
// follow, the last kept row ends in `…`.
function wrapThenCut(text: string, width: number, rows: number): string[] {
  const wrapped = wrap(text, width)
  if (wrapped.length <= rows) return wrapped
  const kept = wrapped.slice(0, rows)
  const last = Array.from(kept[rows - 1])
  kept[rows - 1] = last.slice(0, fitting(last, Math.max(1, width - 1))).join('') + '…'
  return kept
}

const linesOf = (text: string): string[] => {
  const lines = text.split('\n').map(cutLine)
  while (lines.length > 0 && lines[lines.length - 1].trim() === '') lines.pop()
  return lines
}

// A tool use's `▸ <tool> <what>` row: wrapped onto at most MAX_TOOL_ROWS
// rows, the last cut with `…` past them; a call in flight ends `… running`.
function toolRows(use: any, columns: number): TranscriptRow[] {
  const what = cutLine(activityText({ ...(use?.input ?? {}), tool: use?.tool }))
  const room = Math.max(1, columns - widthOf(TOOL))
  const rows = wrapThenCut(what, room, MAX_TOOL_ROWS)
  if (typeof use?.text !== 'string') {
    const last = Array.from(rows[rows.length - 1])
    const fit = fitting(last, Math.max(1, room - widthOf(RUNNING)))
    rows[rows.length - 1] = last.slice(0, fit).join('') + RUNNING
  }
  return rows.map((row, i) => ({ text: (i === 0 ? TOOL : TOOL_CONTINUED) + row }))
}

function messageRows(m: any, columns: number): TranscriptRow[] {
  const out: TranscriptRow[] = []
  const text = typeof m?.text === 'string' ? m.text : ''
  const lead = m?.role === 'user' ? PERSON : AGENT
  if (text.trim() !== '') {
    const room = Math.max(1, columns - widthOf(lead))
    const wrapped = linesOf(text).flatMap(line => wrap(line, room))
    wrapped.slice(0, MAX_MESSAGE_ROWS).forEach((row, i) => out.push({ text: (i === 0 ? lead : CONTINUED) + row }))
    if (wrapped.length > MAX_MESSAGE_ROWS) {
      out.push({ text: `${CONTINUED}… ${wrapped.length - MAX_MESSAGE_ROWS} more rows`, dim: true })
    }
  }
  const uses: any[] = Array.isArray(m?.toolUses) ? m.toolUses : []
  for (const use of uses) {
    out.push(...toolRows(use, columns))
    if (typeof use?.text !== 'string') continue
    const lines = linesOf(use.text)
    const failed = use.isError === true
    if (lines.length === 0 && failed) lines.push('error')
    // Each result line wraps; the first MAX_RESULT_ROWS wrapped rows show.
    const rows = lines.flatMap((line, i) => {
      const lead = failed && i === 0 ? FAIL : RESULT
      return wrap(line, Math.max(1, columns - widthOf(lead))).map((part, j) => ({
        text: (j === 0 ? lead : RESULT) + part,
        isFail: failed && i === 0,
      }))
    })
    rows.slice(0, MAX_RESULT_ROWS).forEach(row =>
      out.push(row.isFail ? { text: row.text, color: 'error' } : { text: row.text, dim: true }),
    )
    if (rows.length > MAX_RESULT_ROWS) {
      out.push({ text: `${RESULT}… ${rows.length - MAX_RESULT_ROWS} more rows`, dim: true })
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
