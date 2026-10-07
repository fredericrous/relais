// What the person sees: the pane, the status line and the toasts. One
// writer: the stream handler keeps the models in module memory and `flush`
// writes them to the pane's state once per tick; the `ui.render` hook only reads.

import type { Fx } from './fx.ts'
import { after, type Store } from './store.ts'
import { paneTree } from './pane.ts'
import {
  isLive,
  statusLine,
  timelineText,
  shortId,
  type RunModel,
} from './timeline.ts'

export const PANE_ID = 'relais'
const PANE_TITLE = 'relais'
export const FLUSH_MS = 100
// Redraw the elapsed time about once a second while a run is live.
const CLOCK_REDRAW_MS = 1000
const DECISION_MERGE_MS = 3000

// State is JSON data: a field left undefined is dropped.
const asJson = (value: unknown) => JSON.parse(JSON.stringify(value))

const runsOf = (store: Store): RunModel[] =>
  Object.values(store.models).sort((a, b) => a.startedAt - b.startedAt)

export const hasLiveRun = (store: Store) => runsOf(store).some(isLive)

export const markDirty = (store: Store) => {
  store.isDirty = true
}

export async function flush(fx: Fx, store: Store) {
  if (store.closed || (!store.isDirty && !hasLiveRun(store))) return
  const now = await fx.clock.now()
  const isStale = hasLiveRun(store) && now - store.flushedAt >= CLOCK_REDRAW_MS
  if (!store.isDirty && !isStale) return
  store.isDirty = false
  store.flushedAt = now
  await fx.pane.write(asJson({ runs: runsOf(store), now }))
  fx.ui.status(statusLine(store.models, now, store.heldOutcome))
}

// Opens the pane. Unasked it is placed only on a wide terminal, and asked
// (by `/relais-status`) at any width; `isPlaced` says which, never a width.
export async function openPane(fx: Fx, store: Store) {
  store.heldOutcome = undefined
  store.isDirty = true
  const opened = await fx.ui.open({ id: PANE_ID, title: PANE_TITLE })
  return opened
}

export async function announceRun(fx: Fx, store: Store) {
  const opened = await openPane(fx, store)
  if (!opened.isPlaced) fx.ui.toast('relais run started: /relais-status')
}

// A decision that needs the person is toasted; those within 3 s merge.
export function toastDecision(fx: Fx, store: Store, text: string) {
  const waiting = store.decisionToast
  if (waiting) {
    waiting.texts.push(text)
    return
  }
  const texts = [text]
  const timer = after(fx, store, DECISION_MERGE_MS, () => {
    store.decisionToast = undefined
    fx.ui.toast(`relais · ${texts.join(' · ')}`)
  })
  store.decisionToast = { texts, timer }
}

export function toastOutcome(fx: Fx, run: string, outcome: string, receipt: string | null) {
  const suffix = receipt ? ` · receipt ${receipt}` : ''
  fx.ui.toast(`relais run ${shortId(run)}: ${outcome}${suffix}`)
}

// The timeline in the transcript, for a pane that cannot be placed.
export function timelineLines(store: Store, now: number): string[] {
  return runsOf(store).flatMap(m => [...timelineText(m, now), ''])
}

// `ui.render` for the pane: reads the state the flush wrote, writes nothing.
export async function renderPane(fx: Fx, e: any) {
  const { value } = await fx.pane.read()
  return paneTree(fx, e, value)
}
