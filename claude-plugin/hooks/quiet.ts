// Quiet runs: a check or command an agent runs whose output goes to a log
// file. `relais quiet watch --protocol` (quiet-watch.ts) tells this module
// about them as JSON lines; this file is the pure part: folding a line into
// the runs, and choosing which of them the pane and status line draw.
// Contract: docs/quiet-runs.md.

import { cutLine, mmss } from './timeline.ts'
import type { ProtocolLine } from './lines.ts'

// A run is drawn only once it has run this long (a failure is drawn at once).
export const QUIET_FLASH_MS = 1000
// The last lines a quiet run keeps.
export const MAX_QUIET_LINES = 10

export type QuietRun = {
  id: string
  // The Claude Code session that started it; null outside Claude Code.
  session: string | null
  tool: string
  check: string
  repo: string
  startedAt: number
  lines: string[]
  // Set when it ended failing (a null exit is a killed run). A pass is
  // dropped on `ended`, so a kept run is always a failure.
  failure?: { exit: number | null; endedAt: number }
}

export type QuietMemory = {
  runs: ReadonlyMap<string, QuietRun>
  // `live`: the watcher child runs; `down`: it exited and waits to be
  // respawned; `unsupported`: an older relais without `quiet watch`.
  watcher: 'off' | 'live' | 'down' | 'unsupported'
  // The watcher is started once per module.
  hasWatcher: boolean
  lastSpawnAt: number | undefined
  // Sessions whose first drawn quiet run already opened the pane.
  opened: Set<string>
  // Runs drawn at the last tick, to see a run cross the anti-flash.
  drawn: ReadonlySet<string>
}

export const createQuietMemory = (): QuietMemory => ({
  runs: new Map(),
  watcher: 'off',
  hasWatcher: false,
  lastSpawnAt: undefined,
  opened: new Set(),
  drawn: new Set(),
})

const asText = (value: unknown, fallback: string) => (typeof value === 'string' ? value : fallback)
const asMs = (value: unknown, fallback: number) => (typeof value === 'number' && Number.isFinite(value) ? value : fallback)

// The runs after one protocol line, or undefined when the line changes
// nothing (a heartbeat, a line for a run not known, a shape not read).
export function foldQuiet(
  runs: ReadonlyMap<string, QuietRun>,
  line: ProtocolLine,
  now: number,
): ReadonlyMap<string, QuietRun> | undefined {
  if (line.relais !== 'quiet' || typeof line.id !== 'string') return undefined
  const id = line.id
  switch (line.event) {
    case 'started': {
      const run: QuietRun = {
        id,
        session: typeof line.session === 'string' ? line.session : null,
        tool: asText(line.tool, 'quiet'),
        check: asText(line.check, ''),
        repo: asText(line.repo, ''),
        startedAt: asMs(line.started, now),
        lines: [],
      }
      const next = new Map(runs)
      // A failure is cleared by the next run of the same session, tool and check.
      for (const [key, old] of runs) {
        if (old.failure && old.session === run.session && old.tool === run.tool && old.check === run.check) {
          next.delete(key)
        }
      }
      next.set(id, run)
      return next
    }
    case 'output': {
      const run = runs.get(id)
      if (!run || !Array.isArray(line.lines)) return undefined
      const added = line.lines.filter((l): l is string => typeof l === 'string').map(cutLine)
      const next = new Map(runs)
      next.set(id, { ...run, lines: [...run.lines, ...added].slice(-MAX_QUIET_LINES) })
      return next
    }
    case 'ended': {
      const run = runs.get(id)
      if (!run) return undefined
      const next = new Map(runs)
      if (line.exit === 0) next.delete(id)
      else {
        const exit = typeof line.exit === 'number' ? line.exit : null
        next.set(id, { ...run, failure: { exit, endedAt: asMs(line.ended, now) } })
      }
      return next
    }
    default:
      return undefined
  }
}

export const isQuietLive = (run: QuietRun) => run.failure === undefined

// Whether the run is drawn at `now`: past the anti-flash, or failed.
export const isQuietDrawn = (run: QuietRun, now: number) =>
  run.failure !== undefined || now - run.startedAt >= QUIET_FLASH_MS

export const quietElapsed = (run: QuietRun, now: number) => (run.failure?.endedAt ?? now) - run.startedAt

type Seen = { sessions: ReadonlySet<string>; quiet: QuietMemory }

// The runs of the sessions this module served: live ones first (oldest
// first), then kept failures (oldest first). A run with no session, or of a
// session not served, is never shown; one added by /resume later is.
export function quietOf(store: Seen): QuietRun[] {
  const own = [...store.quiet.runs.values()].filter(r => r.session !== null && store.sessions.has(r.session))
  const live = own.filter(isQuietLive).sort((a, b) => a.startedAt - b.startedAt)
  const failed = own.filter(r => !isQuietLive(r)).sort((a, b) => a.startedAt - b.startedAt)
  return [...live, ...failed]
}

export const drawnQuiet = (store: Seen, now: number) =>
  store.quiet.runs.size === 0 ? [] : quietOf(store).filter(r => isQuietDrawn(r, now))

export const hasLiveQuiet = (store: Seen) => store.quiet.runs.size > 0 && quietOf(store).some(isQuietLive)

// The status line's part: `amont clippy 0:12`, `amont test failed 0:03`.
export function quietStatus(runs: QuietRun[], now: number): string | undefined {
  if (runs.length === 0) return undefined
  return runs
    .map(r => `${r.tool} ${r.check} ${isQuietLive(r) ? '' : 'failed '}${mmss(quietElapsed(r, now))}`)
    .join(', ')
}
