// What came before a /clear: the last messages of the conversation, text
// only, at most TAIL_CHARS characters, kept in module memory on each main
// turn and frozen when the session ends by /clear. The pane shows the frozen
// tail until `[ hide ]`; it is also kept in the plugin's own store, so a
// module reload still shows it.

import type { Fx } from './fx.ts'
import type { ClearTail, Store, TailMessage } from './store.ts'
import { recordEvent, withinBound } from './events.ts'
import { detach } from './store.ts'
import { TAIL_CHARS } from './limits.ts'
import { markDirty, openPane } from './ui.ts'

export { TAIL_CHARS }
// A turn's read is bounded like every other read of the session.
export const LIVE_READ_BOUND_MS = 5000
// The last read, at the clear, only has to be quick: the live tail stands in.
export const FINAL_READ_BOUND_MS = 500
const KEY = 'relais.clearTail'
const ELLIPSIS = '…'

const isNotification = (text: string) => text.trimStart().startsWith('<task-notification')

// Role and text of the user and assistant messages that have text: no tool
// use, no tool result, no task notification. Newest last, as given.
export function textMessages(messages: unknown): TailMessage[] {
  if (!Array.isArray(messages)) return []
  const kept: TailMessage[] = []
  for (const message of messages) {
    const role = message?.role
    const text = typeof message?.text === 'string' ? message.text.trim() : ''
    if ((role !== 'user' && role !== 'assistant') || text === '' || isNotification(text)) continue
    if (message?.origin?.kind === 'task-notification') continue
    kept.push({ role, text })
  }
  return kept
}

const lengthOf = (text: string) => Array.from(text).length

// The messages trimmed from the front to `limit` characters (code points):
// whole messages from the newest back, then the first older one that does
// not fit whole, cut from its front with `…` to fill the budget. With fewer
// than 2 characters left there is no room for `…` and a character, so that
// message is dropped.
export function trimTail(messages: readonly TailMessage[], limit = TAIL_CHARS): TailMessage[] {
  const out: TailMessage[] = []
  let used = 0
  for (let i = messages.length - 1; i >= 0; i -= 1) {
    const size = lengthOf(messages[i].text)
    if (used + size <= limit) {
      out.unshift(messages[i])
      used += size
      continue
    }
    const room = limit - used
    if (room >= 2) {
      const cut = Array.from(messages[i].text).slice(-(room - 1)).join('')
      out.unshift({ role: messages[i].role, text: `${ELLIPSIS}${cut}` })
    }
    break
  }
  return out
}

// Every text message of the read goes to `trimTail`: the character budget
// alone bounds the tail, so neither tool turns nor many short messages can
// push an older long answer out of it. The read is the whole session
// (`$.session.messages({})`): Claude Code 2.1.291 refuses `{ last }`.
export const tailOf = (messages: unknown): TailMessage[] => trimTail(textMessages(messages))

export const charsOf = (messages: readonly TailMessage[]) => messages.reduce((total, message) => total + lengthOf(message.text), 0)

const readLast = (fx: Fx) => () => fx.session.messages({})

// One read on a main turn. Only the newest read of the session that began it
// updates the live tail: a reply that comes after a newer read began, after
// its bound, or after a session transition is dropped.
export async function readLiveTail(fx: Fx, store: Store) {
  const memory = store.tail
  if (store.closed) return
  const epoch = memory.epoch
  memory.seq += 1
  const seq = memory.seq
  memory.newest = seq
  let session: unknown
  try {
    session = await fx.session.id()
  } catch {
    return // Fail open: no session id, no live tail
  }
  if (typeof session !== 'string' || session === '') return // Fail open: no session id, no live tail
  const settled = await withinBound(fx, LIVE_READ_BOUND_MS, readLast(fx))
  if (settled.kind !== 'ok') return
  if (store.closed || memory.epoch !== epoch || memory.newest !== seq) return
  store.liveTail = { session, seq, messages: tailOf(settled.value) }
}

// The write of the frozen tail (or its removal) to the plugin's store, after
// every write before it: a late write cannot bring back a hidden tail.
function persist(fx: Fx, store: Store, tail: ClearTail | null): Promise<void> {
  store.tail.chain = store.tail.chain
    .then(async () => {
      await fx.kv.set(KEY, tail)
    })
    .catch(() => {
      // The tail in memory still shows; a reload would not find it.
    })
  return store.tail.chain
}

const frozen = (endedSession: string, messages: readonly TailMessage[], at: number): ClearTail =>
  Object.freeze({
    endedSession,
    messages: Object.freeze(messages.map(m => Object.freeze({ role: m.role, text: m.text }))),
    at,
  })

// The session `ended` ends by /clear: one last read, else its live tail, is
// frozen as the pane's tail. An empty one keeps the tail already frozen.
// The live tail is read before the transition resets it.
// `live` is the live tail as the session's end found it, before any await.
export async function freezeTail(
  fx: Fx,
  store: Store,
  ended: string | undefined,
  now: number,
  live: Store['liveTail'] = store.liveTail,
) {
  const memory = store.tail
  resetLiveTail(store)
  const last = await withinBound(fx, FINAL_READ_BOUND_MS, readLast(fx))
  let messages = last.kind === 'ok' ? tailOf(last.value) : []
  if (messages.length === 0 && live !== undefined && (ended === undefined || live.session === ended)) {
    messages = [...live.messages]
  }
  // Memory and the dirty mark come first; the event, the store write and the
  // pane opening are detached, so the session's end waits on none of them.
  const tail = messages.length > 0 && !store.closed ? frozen(ended ?? live?.session ?? '', messages, now) : undefined
  if (tail !== undefined) {
    store.clearTail = tail
    memory.version += 1
    markDirty(store)
  }
  detach(recordEvent(fx, store, { at: now, kind: 'freeze', chars: charsOf(messages) }))
  if (tail === undefined) return
  detach(persist(fx, store, tail))
  detach(openPane(fx, store))
}

// A session transition: reads begun before it never update the live tail.
export function resetLiveTail(store: Store) {
  store.tail.epoch += 1
  store.liveTail = undefined
}

// `[ hide ]`: the tail leaves memory and the store; a freeze begun later is a new tail.
export async function hideTail(fx: Fx, store: Store, now: number) {
  const tail = store.clearTail
  if (tail === undefined) return
  store.clearTail = undefined
  store.tail.version += 1
  // Every way a tail goes leaves its view too.
  if (store.shown.kind === 'tail') store.shown = { kind: 'runs' }
  markDirty(store)
  const write = persist(fx, store, null)
  await recordEvent(fx, store, { at: now, kind: 'hide', seconds: Math.max(0, Math.round((now - tail.at) / 1000)) })
  await write
}

function restorable(raw: unknown): ClearTail | undefined {
  const stored: any = raw && typeof raw === 'object' && 'endedSession' in (raw as object) ? raw : (raw as any)?.value
  if (!stored || typeof stored.endedSession !== 'string' || typeof stored.at !== 'number' || !Array.isArray(stored.messages)) {
    return undefined
  }
  const messages = textMessages(stored.messages)
  return messages.length > 0 ? frozen(stored.endedSession, messages, stored.at) : undefined
}

// After a module reload: the tail the plugin's store kept, unless one was
// frozen or hidden while it was being read.
export async function restoreTail(fx: Fx, store: Store) {
  const version = store.tail.version
  if (store.clearTail !== undefined) return
  try {
    await store.tail.chain
    const tail = restorable(await fx.kv.get(KEY))
    if (!tail || store.closed || store.tail.version !== version || store.clearTail !== undefined) return
    store.clearTail = tail
    markDirty(store)
  } catch {
    // Nothing restored: the tail is a convenience, never a reason to fail the session.
  }
}
