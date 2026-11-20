// The session's programme: `relais programme session --json` is read on
// events only (never polled), outside the 100 ms flush, and the pane draws
// the answer (map.ts). relais owns the claims and the map; this module only
// asks, and keeps the newest answer for the session that asked.

import type { Fx } from './fx.ts'
import type { ProgrammeView } from './map.ts'
import { detach, type Store } from './store.ts'
import { markDirty, openPane } from './ui.ts'

export const PROGRAMME_BOUND_MS = 5000
const EVENTS_KEY = 'relais.programmeEvents'
export const MAX_EVENTS = 200
export const OLDER_RELAIS_ROW = 'programme view needs a newer relais · relais install'
const STALE_NOTE = 'programme view may be stale · relais could not read the claim store'

// A main-chain Bash command that changes which ticket the session holds.
const PROGRAMME_COMMAND = /\brelais\s+programme\s+(claim|release|next)\b/

export const isProgrammeCommand = (e: any): boolean =>
  String(e?.tool) === 'Bash' && e?.agentId === undefined && typeof e?.command === 'string' && PROGRAMME_COMMAND.test(e.command)

export type ReadSource = 'start' | 'clear' | 'resume' | 'fork' | 'command'

export type ProgrammeEvent =
  | { at: number; kind: 'read'; result: 'shown' | 'null' | 'error' }
  | { at: number; kind: 'freeze'; chars: number }
  | { at: number; kind: 'hide'; seconds: number }

type Settled = { kind: 'ok'; value: any } | { kind: 'late' } | { kind: 'failed'; reason: unknown }

// `work`, or `late` once `ms` of the clock have passed. The timer is
// cancelled when the work settles first.
export async function withinBound(fx: Fx, ms: number, work: () => Promise<unknown>): Promise<Settled> {
  let timer: { cancel: () => void } | undefined
  const bound = new Promise<Settled>(resolve => {
    timer = fx.clock.after(ms, () => resolve({ kind: 'late' }))
  })
  let call: Promise<unknown>
  try {
    call = Promise.resolve(work())
  } catch (reason) {
    call = Promise.reject(reason)
  }
  const real = call.then(
    (value): Settled => ({ kind: 'ok', value }),
    (reason): Settled => ({ kind: 'failed', reason }),
  )
  try {
    return await Promise.race([real, bound])
  } finally {
    timer?.cancel()
  }
}

const argvOf = (session: string, dir: string | undefined, previous: string | undefined) => [
  'relais',
  'programme',
  'session',
  '--json',
  '--no-fetch',
  '--session',
  session,
  ...(dir ? ['--dir', dir] : []),
  ...(previous ? ['--previous', previous] : []),
]

const oneLine = (text: unknown) => String(text ?? '').split('\n').find(line => line.trim() !== '')?.trim() ?? ''

// The programme view in a reply, when it has the shape of one.
function viewOf(stdout: string): { kind: 'none' } | { kind: 'view'; view: ProgrammeView } | { kind: 'unreadable' } {
  let parsed: any
  try {
    parsed = JSON.parse(stdout)
  } catch {
    return { kind: 'unreadable' }
  }
  const programme = parsed?.programme
  if (programme === null) return { kind: 'none' }
  const isShape =
    programme !== undefined &&
    typeof programme === 'object' &&
    typeof programme.slug === 'string' &&
    typeof programme.ticket === 'string' &&
    (typeof programme.error === 'string' || (Array.isArray(programme.tickets) && Array.isArray(programme.ready)))
  return isShape ? { kind: 'view', view: programme as ProgrammeView } : { kind: 'unreadable' }
}

// Reads the programme the session holds. Detached by every caller: it never
// delays a hook, and a reply that is older than the newest read, or belongs
// to another session, is dropped.
export async function readProgramme(fx: Fx, store: Store, source: ReadSource = 'start') {
  const memory = store.programme
  if (store.closed || memory.isOlder) return
  if (source === 'resume' || source === 'fork') memory.previous = undefined
  let session: unknown
  try {
    session = await fx.session.id()
  } catch {
    return
  }
  if (typeof session !== 'string' || session === '') return
  if (memory.session !== session) {
    memory.session = session
    memory.shown = undefined
    memory.note = undefined
    markDirty(store)
  }
  memory.seq += 1
  const seq = memory.seq
  memory.newest = seq
  const result = await withinBound(fx, PROGRAMME_BOUND_MS, () =>
    fx.process.run(argvOf(session as string, store.router.cwd, memory.previous), {
      ...(store.router.cwd ? { cwd: store.router.cwd } : {}),
      timeoutMs: PROGRAMME_BOUND_MS,
    }),
  )
  if (store.closed || memory.session !== session || memory.newest !== seq) return
  const now = await fx.clock.now()
  if (store.closed || memory.session !== session || memory.newest !== seq) return
  if (result.kind !== 'ok') {
    // A timeout or a failed spawn keeps the view of this session.
    await recordEvent(fx, store, { at: now, kind: 'read', result: 'error' })
    return
  }
  const { exitCode, stdout, stderr } = result.value ?? {}
  if (exitCode === 2 && /unrecognized subcommand\s+'?session'?/i.test(String(stderr ?? ''))) {
    memory.isOlder = true
    memory.shown = undefined
    memory.note = OLDER_RELAIS_ROW
    markDirty(store)
    await recordEvent(fx, store, { at: now, kind: 'read', result: 'error' })
    return
  }
  if (exitCode === 1 || exitCode === 2) {
    memory.note = exitCode === 1 ? STALE_NOTE : `programme view unavailable · ${oneLine(stderr) || 'usage error'}`
    markDirty(store)
    await recordEvent(fx, store, { at: now, kind: 'read', result: 'error' })
    return
  }
  const read = exitCode === 0 ? viewOf(String(stdout ?? '')) : ({ kind: 'unreadable' } as const)
  if (read.kind === 'unreadable') {
    await recordEvent(fx, store, { at: now, kind: 'read', result: 'error' })
    return
  }
  memory.note = undefined
  if (read.kind === 'none') {
    memory.shown = undefined
    markDirty(store)
    await recordEvent(fx, store, { at: now, kind: 'read', result: 'null' })
    return
  }
  memory.shown = { view: read.view, readAt: now, seq }
  markDirty(store)
  await recordEvent(fx, store, { at: now, kind: 'read', result: 'shown' })
  // Unasked, once per session, as quiet runs do: the pane opens without taking focus.
  if (!memory.opened.has(session)) {
    memory.opened.add(session)
    await openPane(fx, store)
  }
}

// A session ended: its view and every read still pending are dropped, and a
// /clear's session becomes `--previous` for the one that follows.
export function onProgrammeSessionEnd(store: Store, reason: string, ended: string | undefined) {
  const memory = store.programme
  memory.session = undefined
  memory.shown = undefined
  memory.note = memory.isOlder ? OLDER_RELAIS_ROW : undefined
  memory.newest = memory.seq + 1
  memory.seq = memory.newest
  memory.previous = reason === 'clear' ? ended : undefined
  markDirty(store)
}

// What the pane's state carries of the programme, or nothing.
export function programmeState(store: Store): { view?: ProgrammeView; note?: string } | undefined {
  const { shown, note } = store.programme
  const view = shown !== undefined && 'view' in shown ? shown.view : undefined
  if (view === undefined && note === undefined) return undefined
  return { ...(view ? { view } : {}), ...(note ? { note } : {}) }
}

// ---- the event list: one entry per read, freeze and hide, the last 200 ----

const listOf = (raw: unknown): ProgrammeEvent[] => {
  const value = raw && typeof raw === 'object' && 'value' in (raw as object) ? (raw as { value: unknown }).value : raw
  return Array.isArray(value) ? (value as ProgrammeEvent[]) : []
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
  if (reads('shown') + freezes.length + hides.length === 0) return undefined
  const kept = freezes.filter(e => e.kind === 'freeze' && e.chars > 0).length
  const quick = hides.filter(e => e.kind === 'hide' && e.seconds <= 60).length
  return `programme reads: ${reads('shown')} shown, ${reads('null')} none, ${reads('error')} failed · tails: ${freezes.length} frozen, ${kept} non-empty, ${hides.length} hidden (${quick} within 60 s)`
}

export const detachRead = (fx: Fx, store: Store, source: ReadSource) => detach(readProgramme(fx, store, source))
