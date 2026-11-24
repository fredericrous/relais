// The session's programme: `relais programme session --json` is read on
// events only (never polled), outside the 100 ms flush, and the pane draws
// the answer (map.ts). relais owns the claims and the map; this module only
// asks, and keeps the newest answer for the session that asked.

import type { Fx } from './fx.ts'
import { recordEvent, withinBound } from './events.ts'
import type { ProgrammeView } from './map.ts'
import type { Adoption, Store } from './store.ts'
import { markDirty, openPane } from './ui.ts'

export const PROGRAMME_BOUND_MS = 5000
// The least a read keeps after an adoption spent the shared bound: a clear
// that must adopt waits at most PROGRAMME_BOUND_MS + READ_FLOOR_MS (6 s), all
// of it detached from the hook.
const READ_FLOOR_MS = 1000
// R, the handoff hold: past it the predecessor's handoff has lapsed, so a
// retry finds nothing to adopt and would only hide the failure.
export const ADOPT_GIVE_UP_MS = 2 * 60 * 60 * 1000
export const OLDER_RELAIS_ROW = 'programme view needs a newer relais · relais install'
const STALE_NOTE = 'programme view may be stale · relais programme session --session <id>'
const OLDER_ADOPT = /unrecognized subcommand\s+'?adopt'?/i

// A main-chain Bash command that changes which ticket the session holds.
const PROGRAMME_COMMAND = /\brelais\s+programme\s+(claim|release|next|adopt)\b/

export const isProgrammeCommand = (e: any): boolean =>
  String(e?.tool) === 'Bash' && e?.agentId === undefined && typeof e?.command === 'string' && PROGRAMME_COMMAND.test(e.command)

export type ReadSource = 'start' | 'clear' | 'resume' | 'fork' | 'command'

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

const TICKET_STATUSES: ReadonlySet<string> = new Set(['closed', 'frontier', 'blocked', 'in_flight', 'out_of_scope'])
const COUNT_FIELDS = ['closed', 'frontier', 'blocked', 'in_flight', 'open', 'out_of_scope'] as const

const isText = (field: unknown): field is string => typeof field === 'string'
const isTextList = (field: unknown): field is string[] => Array.isArray(field) && field.every(isText)
const isCount = (field: unknown) => typeof field === 'number' && Number.isInteger(field) && field >= 0

function isTicket(ticket: any): boolean {
  return (
    ticket !== null &&
    typeof ticket === 'object' &&
    isText(ticket.id) &&
    isText(ticket.name) &&
    TICKET_STATUSES.has(ticket.status) &&
    isTextList(ticket.blocked_by) &&
    isTextList(ticket.open_blockers) &&
    isTextList(ticket.dependents) &&
    (ticket.due === null || isText(ticket.due)) &&
    typeof ticket.held === 'boolean'
  )
}

// The view in a `programme` object, when every field the pane reads is there
// and of its type; anything else is not a view.
export function decodeView(programme: unknown): ProgrammeView | undefined {
  const view: any = programme
  if (view === null || typeof view !== 'object') return undefined
  const isKnown = isText(view.repo) && isText(view.slug) && isText(view.ticket) && (view.kind === 'work' || view.kind === 'handoff')
  if (!isKnown) return undefined
  if (isText(view.error)) return view as ProgrammeView
  const isMap =
    (view.holder === 'self' || view.holder === 'previous') &&
    view.counts !== null &&
    typeof view.counts === 'object' &&
    COUNT_FIELDS.every(field => isCount(view.counts[field])) &&
    Array.isArray(view.tickets) &&
    view.tickets.every(isTicket) &&
    isTextList(view.ready)
  return isMap ? (view as ProgrammeView) : undefined
}

function viewOf(stdout: string): { kind: 'none' } | { kind: 'view'; view: ProgrammeView } | { kind: 'unreadable' } {
  let parsed: any
  try {
    parsed = JSON.parse(stdout)
  } catch {
    return { kind: 'unreadable' }
  }
  if (parsed?.programme === null) return { kind: 'none' }
  const view = decodeView(parsed?.programme)
  return view ? { kind: 'view', view } : { kind: 'unreadable' }
}

// Takes the handoff of the session a /clear ended over as this session's
// `work` (`relais programme adopt`), within `ms`, so a relayed session holds
// its ticket whether or not its agent claims it. Exit 0 or any exit 2 ends
// the adoption; a timeout, exit 1 or a failed spawn leaves it `failed`, to
// run again at the next read, with the note `adoptNote` draws. The running
// state is taken before the first await, so two overlapping reads spawn one
// adopt.
async function adoptPrevious(fx: Fx, store: Store, session: string, ms: number) {
  const memory = store.programme
  const adoption = memory.adopt
  if (adoption === undefined || adoption.state === 'running' || adoption.state === 'gaveUp') return
  if (adoption.state === 'failed' && adoption.session !== session) {
    memory.adopt = undefined // Another session: the successor is gone
    return
  }
  const running: Adoption & { state: 'running' } = {
    state: 'running',
    previous: adoption.previous,
    session,
    since: adoption.state === 'failed' ? adoption.since : undefined,
  }
  memory.adopt = running
  const now = await fx.clock.now()
  if (store.closed || memory.adopt !== running) return
  // Bound at the successor's first read, seconds after the clear.
  const since = running.since ?? now
  running.since = since
  if (now - since > ADOPT_GIVE_UP_MS) {
    // No more retries; the note stays for this session.
    memory.adopt = { state: 'gaveUp', previous: running.previous, session }
    await recordEvent(fx, store, { at: now, kind: 'adopt', result: 'error', cause: 'gave-up' })
    return
  }
  const reply = await withinBound(fx, ms, () =>
    fx.process.run(['relais', 'programme', 'adopt', '--previous', running.previous, '--session', session], {
      ...(store.router.cwd ? { cwd: store.router.cwd } : {}),
      timeoutMs: ms,
    }),
  )
  if (store.closed || memory.adopt !== running) return // A clear came meanwhile
  const at = await fx.clock.now()
  const value = reply.kind === 'ok' ? reply.value : undefined
  const exitCode = value?.exitCode
  if (exitCode === 0) {
    memory.adopt = undefined
    await recordEvent(fx, store, { at, kind: 'adopt', result: 'ok' })
    return
  }
  if (exitCode === 2) {
    memory.adopt = undefined
    const stderr = String(value?.stderr ?? '')
    const older = OLDER_ADOPT.test(stderr)
    await recordEvent(fx, store, older ? { at, kind: 'adopt', result: 'older' } : { at, kind: 'adopt', result: 'error', cause: 'usage', detail: oneLine(stderr) })
    return
  }
  memory.adopt = { state: 'failed', previous: running.previous, session, since }
  const cause = reply.kind === 'late' ? 'late' : reply.kind === 'failed' ? 'spawn' : 'exit'
  await recordEvent(fx, store, { at, kind: 'adopt', result: 'error', cause, detail: oneLine(value?.stderr) })
}

// The dim note of an adoption that failed for this session: the command
// that takes the ticket by hand, when the view names the ticket.
function adoptNote(store: Store, view: ProgrammeView | undefined): string | undefined {
  const memory = store.programme
  const adoption = memory.adopt
  if (adoption === undefined || adoption.session !== memory.session) return undefined
  const claim = view === undefined ? undefined : `relais programme claim ${view.slug} ${view.ticket}`
  switch (adoption.state) {
    case 'unbound':
    case 'running':
      return undefined
    case 'failed':
      return `ticket not adopted · ${claim ?? `relais programme adopt --previous ${adoption.previous}`}`
    case 'gaveUp':
      // The handoff has lapsed by now: adopt would find nothing.
      return claim === undefined ? 'ticket not adopted in 2 h · its handoff lapsed' : `ticket not adopted in 2 h · ${claim}`
  }
}

// Reads the programme the session holds. Detached by every caller: it never
// delays a hook, and a reply that is older than the newest read, or belongs
// to another session, is dropped. A pending adoption runs first, and the
// read gets what is left of the same bound (at least READ_FLOOR_MS).
export async function readProgramme(fx: Fx, store: Store, source: ReadSource = 'start') {
  const memory = store.programme
  if (store.closed || memory.isOlder) return
  // A resume or fork passes no --previous. A pending adoption stays: it is
  // bound to its session, and is dropped at the first read of another one.
  if (source === 'resume' || source === 'fork') memory.previous = undefined
  let session: unknown
  try {
    session = await fx.session.id()
  } catch {
    return // Fail open: no session id, no read
  }
  if (typeof session !== 'string' || session === '') return // Fail open: no session id, no read
  if (memory.session !== session) {
    memory.session = session
    memory.shown = undefined
    memory.note = undefined
    markDirty(store)
  }
  const started = await fx.clock.now()
  await adoptPrevious(fx, store, session, PROGRAMME_BOUND_MS)
  if (store.closed || memory.session !== session) return
  const bound = Math.max(READ_FLOOR_MS, PROGRAMME_BOUND_MS - ((await fx.clock.now()) - started))
  memory.seq += 1
  const seq = memory.seq
  memory.newest = seq
  const reply = await withinBound(fx, bound, () =>
    fx.process.run(argvOf(session as string, store.router.cwd, memory.previous), {
      ...(store.router.cwd ? { cwd: store.router.cwd } : {}),
      timeoutMs: bound,
    }),
  )
  if (store.closed || memory.session !== session || memory.newest !== seq) return
  const now = await fx.clock.now()
  if (store.closed || memory.session !== session || memory.newest !== seq) return
  if (reply.kind !== 'ok') {
    // A timeout or a failed spawn keeps the view of this session.
    await recordEvent(fx, store, { at: now, kind: 'read', result: 'error' })
    return
  }
  const { exitCode, stdout, stderr } = reply.value ?? {}
  if (exitCode === 2 && /unrecognized subcommand\s+'?session'?/i.test(String(stderr ?? ''))) {
    memory.isOlder = true
    memory.shown = undefined
    memory.note = OLDER_RELAIS_ROW
    markDirty(store)
    await recordEvent(fx, store, { at: now, kind: 'read', result: 'error' })
    return
  }
  if (exitCode === 2) {
    memory.note = `programme view unavailable · ${oneLine(stderr) || 'usage error'}`
    markDirty(store)
    await recordEvent(fx, store, { at: now, kind: 'read', result: 'error' })
    return
  }
  // Exit 1, any other code and a signal: the answer cannot be trusted, the last view stays beside the note.
  const read = exitCode === 0 ? viewOf(String(stdout ?? '')) : ({ kind: 'unreadable' } as const)
  if (read.kind === 'unreadable') {
    memory.note = STALE_NOTE
    markDirty(store)
    await recordEvent(fx, store, { at: now, kind: 'read', result: 'error' })
    return
  }
  memory.note = adoptNote(store, read.kind === 'view' ? read.view : undefined)
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
  // The successor binds the adoption at its first read; any other end drops it.
  memory.adopt =
    reason === 'clear' && ended !== undefined ? { state: 'unbound', previous: ended, session: undefined } : undefined
  markDirty(store)
}
