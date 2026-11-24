// The programme and tail event list, kept in the plugin's own store: one
// entry per read, freeze, hide and open, the last MAX_EVENTS, and the bounded wait
// every one of those reads and writes goes through.

import type { Fx } from './fx.ts'
import type { Store } from './store.ts'

const EVENTS_KEY = 'relais.programmeEvents'
export const MAX_EVENTS = 200

export type ProgrammeEvent =
  | { at: number; kind: 'read'; result: 'shown' | 'null' | 'error' }
  | { at: number; kind: 'adopt'; result: 'ok'; detail?: string }
  | { at: number; kind: 'adopt'; result: 'older' }
  | { at: number; kind: 'adopt'; result: 'error'; cause: 'late' | 'spawn' | 'exit' | 'usage' | 'gave-up'; detail?: string }
  | { at: number; kind: 'freeze'; chars: number }
  | { at: number; kind: 'hide'; seconds: number }
  | { at: number; kind: 'open'; rows: number; bodyRows: number }

export type Settled<T> = { kind: 'ok'; value: T } | { kind: 'late' } | { kind: 'failed'; reason: unknown }

// `work`, or `late` once `ms` of the clock have passed. The timer is
// cancelled when the work settles first.
export async function withinBound<T>(fx: Fx, ms: number, work: () => Promise<T>): Promise<Settled<T>> {
  let timer: { cancel: () => void } | undefined
  const bound = new Promise<Settled<T>>(resolve => {
    timer = fx.clock.after(ms, () => resolve({ kind: 'late' }))
  })
  let call: Promise<T>
  try {
    call = Promise.resolve(work())
  } catch (reason) {
    call = Promise.reject(reason)
  }
  const real = call.then(
    (value): Settled<T> => ({ kind: 'ok', value }),
    (reason): Settled<T> => ({ kind: 'failed', reason }),
  )
  try {
    return await Promise.race([real, bound])
  } finally {
    timer?.cancel()
  }
}

const listOf = (raw: unknown): ProgrammeEvent[] => {
  const stored = raw && typeof raw === 'object' && 'value' in (raw as object) ? (raw as { value: unknown }).value : raw
  return Array.isArray(stored) ? (stored as ProgrammeEvent[]) : []
}

// Appends one event; writes follow one another, so none is lost to a race.
export function recordEvent(fx: Fx, store: Store, event: ProgrammeEvent): Promise<void> {
  store.eventChain = store.eventChain
    .then(async () => {
      const events = listOf(await fx.kv.get(EVENTS_KEY))
      await fx.kv.set(EVENTS_KEY, [...events, event].slice(-MAX_EVENTS))
    })
    .catch(() => {
      // A count not kept is never a reason to fail what it counts.
    })
  return store.eventChain
}

// The one line `/relais-status` prints, or nothing when no programme or tail was ever shown.
export async function eventsLine(fx: Fx, store: Store): Promise<string | undefined> {
  await store.eventChain
  let events: ProgrammeEvent[]
  try {
    events = listOf(await fx.kv.get(EVENTS_KEY))
  } catch {
    return undefined
  }
  const reads = (result: string) => events.filter(e => e.kind === 'read' && e.result === result).length
  const freezes = events.filter(e => e.kind === 'freeze')
  const hides = events.filter(e => e.kind === 'hide')
  const opens = events.filter(e => e.kind === 'open').length
  if (reads('shown') + freezes.length + hides.length + opens === 0) return undefined
  const kept = freezes.filter(e => e.kind === 'freeze' && e.chars > 0).length
  const quick = hides.filter(e => e.kind === 'hide' && e.seconds <= 60).length
  return `programme reads: ${reads('shown')} shown, ${reads('null')} none, ${reads('error')} failed · tails: ${freezes.length} frozen, ${kept} non-empty, ${hides.length} hidden (${quick} within 60 s), ${opens} opened`
}
