// The module's memory for one load. A reload makes a fresh one, as it kills
// the run's child, so nothing here outlives the child it describes.

import type { Fx } from './fx.ts'
import type { Dispatch } from './dispatches.ts'
import type { RunModel } from './timeline.ts'
import { createRouterMemory, type RouterMemory } from './router.ts'
import type { TranscriptState } from './transcript.ts'

// What the pane shows: the runs, or one agent's transcript.
export type Shown =
  | { kind: 'runs' }
  | {
      kind: 'agent'
      agentId: string
      run: string
      transcript: TranscriptState
      generation: number
      // The pane's width at the press: the transcript is laid out at it.
      columns: number
      // The clock when the last read began.
      refreshedAt: number
      // Whether a result (rows, deny or slow) was stored for this view.
      hasLanded: boolean
      // The agent's status when the last read began: a change is read again.
      status: string | undefined
    }

// One `relais run` child, as the module started it.
export type Child = {
  // The session the child was told (RELAIS_SESSION_ID): its hello names it.
  session: string
  cwd: string
  // The request's argv as JSON: how the `process.spawn` hook recognises it.
  argv: string
  exited: boolean
  // The key of this child's run in `Store.models`: a placeholder until its
  // first event names the run.
  key: string
  // Set by its `done` line. A child that exits without one still owes the
  // model a message: the exit code and the end of its stderr.
  doneSeen?: boolean
  // The last stderr lines, for that message.
  stderrTail?: string[]
}

export type Store = {
  // Set when the module is unloading: no timer fires and no retry goes on.
  closed: boolean
  timers: { cancel: () => void }[]
  children: Child[]
  // Verdicts waiting to be submitted as prompts: the session's timer sends
  // them, because a prompt cannot be submitted from under a tool hook.
  verdicts: string[]
  // Whether that timer exists.
  hasPump: boolean
  // Children whose `process.spawn` has not yet reached the module's hook.
  starting: Child[]
  models: Record<string, RunModel>
  dispatches: Map<string, Dispatch>
  dispatchChild: Map<string, Child>
  agentDispatch: Map<string, string>
  // dispatch id -> the cwd its spawn must run in
  pending: Map<string, { cwd: string }>
  // Set around the module's own SendMessage, so its `agent.offer` hook lets
  // a relais type through (Claude Code refuses to resume a hidden one).
  resuming: number
  // Session ids the module says hello for.
  sessions: Set<string>
  isDirty: boolean
  // Why the last `relais native status` could not be read, if it could not.
  statusFailure: string | undefined
  // Whether the last look at `$.agent.list()` failed (said once a streak).
  isListFailing: boolean
  // Whether the last verdict submission failed (said once a streak).
  isSubmitFailing: boolean
  // A verdict's `prompt.submit` is in flight: the pump holds until it
  // settles instead of submitting the same verdict again on the next tick.
  isSubmitting: boolean
  // When the pane's state was last written, in the clock's ms.
  flushedAt: number
  poll: { cancel: () => void } | undefined
  // The outcome the status line keeps after `done`.
  heldOutcome: string | undefined
  // Decisions waiting to be merged into one toast.
  decisionToast: { texts: string[]; timer: { cancel: () => void } } | undefined
  // The setup question open for a repository root (consent.ts): a second
  // call waits for it rather than asking again.
  consent: Map<string, Promise<string>>
  // Repository roots the person answered Not now for in this session.
  declined: Set<string>
  // The outcome message this plugin is submitting, while its submit is in
  // flight, so `prompt.submit` does not take it for the person's own words.
  ownPrompts: Set<string>
  // Outcome messages announcing a state that waits on a person, by their
  // exact text, until submitted: the pump checks each against what the
  // ledger holds just before submitting it (runs.ts, `freshen`).
  pendingOutcomes: Map<string, PendingOutcome>
  // Whether this session was told its relais gives no `current` state.
  isCurrentMissingNoted: boolean
  // The session router's memory: router-state, the task, the pinned route,
  // the observations waiting (routing.ts).
  router: RouterMemory
  shown: Shown
  // Counts the views opened: a read belongs to the one that began it.
  generation: number
  // The shown agent's transcript is read again on the next tick.
  transcriptDirty: boolean
  // A transcript read is under way (set around the whole attempt, 5 s bound).
  isReadingTranscript: boolean
  // The real `session.messages` promise is unsettled (it may outlive the bound).
  pendingMessages: boolean
  // `generation:reason` of the notes already made on the run.
  notedReasons: Set<string>
}

// What an outcome message was built from, to rebuild it if the run's
// decision is answered before the message goes out.
export type PendingOutcome = {
  run: string
  outcome: string
  code: string | null
  // `run` or `replay`, as the headline names it.
  kind: string
  // The lines after the headline that stay true: receipt, changes, trial.
  facts: string[]
  // Which of its notes the pane already has: a submit that keeps failing
  // is checked again every tick, and each note is said once. Two, so a
  // check that failed first and found an answer later still says so.
  noted: Set<'failed' | 'answered'>
}

// A background promise nobody awaits: once the module unloads its effects
// reject, and that is no error worth an unhandled rejection.
export const detach = (work: Promise<unknown>) => {
  work.catch(() => {})
}

export const createStore = (): Store => ({
  closed: false,
  timers: [],
  children: [],
  starting: [],
  verdicts: [],
  hasPump: false,
  models: {},
  dispatches: new Map(),
  dispatchChild: new Map(),
  agentDispatch: new Map(),
  pending: new Map(),
  resuming: 0,
  sessions: new Set(),
  isDirty: false,
  statusFailure: undefined,
  isListFailing: false,
  isSubmitFailing: false,
  isSubmitting: false,
  flushedAt: 0,
  poll: undefined,
  heldOutcome: undefined,
  decisionToast: undefined,
  consent: new Map(),
  declined: new Set(),
  ownPrompts: new Set(),
  pendingOutcomes: new Map(),
  isCurrentMissingNoted: false,
  router: createRouterMemory(),
  shown: { kind: 'runs' },
  generation: 0,
  transcriptDirty: false,
  isReadingTranscript: false,
  pendingMessages: false,
  notedReasons: new Set(),
})

// Timers go through here so that unloading can clear every one.
export function every(fx: Fx, store: Store, ms: number, fn: () => void) {
  const timer = fx.clock.every(ms, () => {
    if (!store.closed) fn()
  })
  store.timers.push(timer)
  return timer
}

export function after(fx: Fx, store: Store, ms: number, fn: () => void) {
  const timer = fx.clock.after(ms, () => {
    if (!store.closed) fn()
  })
  store.timers.push(timer)
  return timer
}

export function close(store: Store) {
  store.closed = true
  for (const timer of store.timers) timer.cancel()
  store.timers = []
  store.poll = undefined
}
