// The module calls back into relais with short subcommands. The payload goes
// on stdin (a hooks module has no files), every call has an explicit
// timeout, and a failure is retried until relais acknowledges (exit 0) or the
// run's child is gone.

import type { Fx } from './fx.ts'
import { after, detach, type Child, type Store } from './store.ts'

const CALL_TIMEOUT_MS = 20_000
// Backoff in ms; the last step repeats.
const BACKOFF_MS = [1000, 2000, 4000, 8000, 15000]
export const HELLO_EVERY_MS = 30_000

const backoff = (attempt: number) => BACKOFF_MS[Math.min(attempt, BACKOFF_MS.length - 1)]

// Runs `relais native …` once; true when acknowledged.
async function callOnce(fx: Fx, argv: string[], payload: unknown): Promise<boolean> {
  try {
    const stdin = payload === undefined ? undefined : JSON.stringify(payload)
    const result = await fx.process.run(['relais', 'native', ...argv], {
      stdin,
      timeoutMs: CALL_TIMEOUT_MS,
    })
    return result.exitCode === 0
  } catch {
    return false
  }
}

// Sends until acknowledged. Nothing is written to disk: the payload waits in
// memory, and is dropped when the child exits or the module unloads.
export function deliver(fx: Fx, store: Store, child: Child, argv: string[], payload: unknown) {
  const attempt = async (n: number): Promise<void> => {
    if (store.closed || child.exited) return
    if (await callOnce(fx, argv, payload)) return
    if (store.closed || child.exited) return
    after(fx, store, backoff(n), () => detach(attempt(n + 1)))
  }
  detach(attempt(0))
}

export const sendBound = (fx: Fx, store: Store, child: Child, dispatch: string, agent: string) =>
  deliver(fx, store, child, ['bound', '--dispatch', dispatch, '--agent', agent], undefined)

export const sendStopped = (fx: Fx, store: Store, child: Child, dispatch: string, payload: unknown) =>
  deliver(fx, store, child, ['stopped', '--dispatch', dispatch], payload)

// Hello is a heartbeat: a missed one is replaced by the next, never retried.
export async function sendHello(fx: Fx, store: Store) {
  if (store.closed) return
  for (const session of store.sessions) {
    await callOnce(fx, ['hello', '--session', session], undefined)
  }
}
