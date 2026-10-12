// The quiet-run watcher: `relais quiet watch --protocol`, a child with its
// own spawn path and read loop. It is not a run: no model, no `starting`
// entry, no announcement, and its exit draws no run.

import type { Fx } from './fx.ts'
import { parseProtocolLine, splitLines } from './lines.ts'
import { drawnQuiet, foldQuiet } from './quiet.ts'
import { after, detach, type Store } from './store.ts'
import { markDirty, openPane } from './ui.ts'

const WATCHER_ARGV = ['relais', 'quiet', 'watch', '--protocol']
// At most one respawn per this long.
export const RESPAWN_MS = 30_000
// clap's usage error: a relais older than `quiet watch`.
const UNSUPPORTED_EXIT = 2

// Starts the watcher once per module; it outlives /clear and /resume.
export function ensureWatcher(fx: Fx, store: Store) {
  if (store.quiet.hasWatcher) return
  store.quiet.hasWatcher = true
  detach(watch(fx, store))
}

async function watch(fx: Fx, store: Store) {
  const memory = store.quiet
  memory.lastSpawnAt = await fx.clock.now()
  memory.watcher = 'live'
  markDirty(store)
  let carry = ''
  let code: number | null = null
  try {
    const request = {
      argv: WATCHER_ARGV,
      ...(store.router.cwd ? { cwd: store.router.cwd } : {}),
      env: { RELAIS_HOST: 'claude-code-mod' },
    }
    const stream = fx.process.spawn(request)
    let step = await stream.next()
    while (!step.done) {
      if (step.value?.stream === 'stdout') carry = await onChunk(fx, store, carry, String(step.value.text ?? ''))
      step = await stream.next()
    }
    code = typeof step.value?.code === 'number' ? step.value.code : null
  } catch (error) {
    // A watcher that could not start is a watcher that exited; why is kept.
    memory.lastError = error instanceof Error ? error.message : String(error)
    code = null
  }
  if (store.closed) return
  if (code === UNSUPPORTED_EXIT) {
    // Noted once by this state; never respawned, nothing drawn.
    memory.watcher = 'unsupported'
    return
  }
  memory.watcher = 'down'
  // Its live runs go with it: entries still live are announced again by the
  // respawned watcher, and those that ended meanwhile would never end here.
  memory.runs = new Map([...memory.runs].filter(([, run]) => run.failure !== undefined))
  markDirty(store)
  const since = (await fx.clock.now()) - (memory.lastSpawnAt ?? 0)
  after(fx, store, Math.max(0, RESPAWN_MS - since), () => detach(watch(fx, store)))
}

// One stdout chunk of the watcher, folded; returns the unfinished line.
async function onChunk(fx: Fx, store: Store, carry: string, text: string): Promise<string> {
  const split = splitLines(carry, text)
  const now = await fx.clock.now()
  for (const raw of split.lines) {
    const line = parseProtocolLine(raw)
    if (!line) continue
    const next = foldQuiet(store.quiet.runs, line, now)
    if (!next) continue
    store.quiet.runs = next
    markDirty(store)
  }
  return split.carry
}

// Whether a tick has anything to look at: the pump asks before awaiting,
// because most ticks have no quiet run.
export const hasQuietWork = (store: Store) => store.quiet.runs.size > 0 || store.quiet.drawn.size > 0

// One pump tick: a run that crossed the anti-flash (or ended failed) is
// drawn, and the first in a session opens the pane. Only runs of the
// sessions this module served count.
export async function tickQuiet(fx: Fx, store: Store) {
  const memory = store.quiet
  const now = await fx.clock.now()
  const drawn = drawnQuiet(store, now)
  const fresh = drawn.filter(r => !memory.drawn.has(r.id))
  memory.drawn = new Set(drawn.map(r => r.id))
  if (fresh.length === 0) return
  markDirty(store)
  for (const run of fresh) {
    if (run.session === null || memory.opened.has(run.session)) continue
    memory.opened.add(run.session)
    const opened = await openPane(fx, store)
    if (!opened.isPlaced) fx.ui.toast(`${run.tool} ${run.check} running: /relais-status`)
  }
}
