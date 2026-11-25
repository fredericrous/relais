// The bookkeeping of one dispatch: its agent, the usage its turns summed to,
// and whether its agent's run has ended. Pure: the caller owns the maps.

export type Usage = {
  input_tokens: number
  output_tokens: number
  cache_read_input_tokens: number
  cache_creation_input_tokens: number
  model: string
}

export type Dispatch = {
  id: string
  agent: string
  // The agent's status when a `continue` began this dispatch; undefined for a fresh spawn.
  startStatus: string | undefined
  // Whether a status other than `startStatus` has been seen since.
  moved: boolean
  // Whether the agent has been seen in `$.agent.list()` since the dispatch began.
  seen: boolean
  // The status last listed, for an agent evicted from the list after it ended.
  last: string | undefined
  turns: number
  usage: Usage
  answer: string
  isBound: boolean
  isStopped: boolean
  // An answer turn came; `answeredAt` is stamped at the next look.
  isAnswered: boolean
  // The clock at the first look after the last answer turn.
  answeredAt: number | undefined
  // The plugin stopped the agent itself: its end is `completed`, answer kept.
  isEnding: boolean
  // `TaskStop` was refused or errored: it is not tried again.
  stopFailed: boolean
  // The look count when `TaskStop` answered success; only a later look confirms the stop.
  stopSeq: number | undefined
  // The clock when `TaskStop` answered success.
  stopSentAt: number | undefined
  // The 30 s note of an unconfirmed stop was written (once per dispatch).
  isNoted: boolean
}

// One look at the list: the clock, and its number in the order of looks.
export type Look = { now: number; seq: number }

// How long an answered agent may stay `running` before the plugin stops it.
export const ANSWER_GRACE_MS = 5000
// How long after the stop the agent may stay `running` before the person is told.
export const STOP_CONFIRM_MS = 30000
export const PLUGIN_STOP_NOTE =
  'relais plugin: the worker ended its turn with background work still running; it was stopped before the snapshot, and its output was not read.'

export type Listed = { id: string; status: string }

export const ENDED = ['completed', 'failed', 'killed']
// Statuses a dispatch may end on with no turn, once the status has moved.
const ENDED_WITHOUT_TURN = ['failed', 'killed']

export const emptyUsage = (): Usage => ({
  input_tokens: 0,
  output_tokens: 0,
  cache_read_input_tokens: 0,
  cache_creation_input_tokens: 0,
  model: '',
})

export function openDispatch(id: string, agent: string, startStatus?: string): Dispatch {
  return {
    id,
    agent,
    startStatus,
    moved: false,
    seen: false,
    last: undefined,
    turns: 0,
    usage: emptyUsage(),
    answer: '',
    isBound: false,
    isStopped: false,
    isAnswered: false,
    answeredAt: undefined,
    isEnding: false,
    stopFailed: false,
    stopSeq: undefined,
    stopSentAt: undefined,
    isNoted: false,
  }
}

type TurnUsage = Partial<Omit<Usage, 'model'>> & { model?: string }

export function addUsage(total: Usage, turn: TurnUsage | undefined): Usage {
  if (!turn) return total
  return {
    input_tokens: total.input_tokens + (turn.input_tokens ?? 0),
    output_tokens: total.output_tokens + (turn.output_tokens ?? 0),
    cache_read_input_tokens: total.cache_read_input_tokens + (turn.cache_read_input_tokens ?? 0),
    cache_creation_input_tokens:
      total.cache_creation_input_tokens + (turn.cache_creation_input_tokens ?? 0),
    model: turn.model || total.model,
  }
}

// A turn of the dispatch's agent: its usage is added, and its answer kept
// unless it is empty (a stopped agent's last turn is empty).
//
// An answer turn (`reason: 'answer'`; with no reason, a turn that was not
// aborted and has an answer) restarts the end rule; any other turn, such as
// the empty aborted one a stopped agent ends on, leaves it as it was.
export function withTurn(
  d: Dispatch,
  turn: { usage?: TurnUsage; answer?: string; reason?: string; isAborted?: boolean },
): Dispatch {
  const isAnswer = turn.reason === undefined ? turn.isAborted !== true && !!turn.answer : turn.reason === 'answer'
  const restarted = isAnswer
    ? {
        isAnswered: true,
        answeredAt: undefined,
        isEnding: false,
        stopFailed: false,
        stopSeq: undefined,
        stopSentAt: undefined,
        isNoted: false,
      }
    : {}
  return {
    ...d,
    ...restarted,
    turns: d.turns + 1,
    usage: addUsage(d.usage, turn.usage),
    answer: turn.answer ? turn.answer : d.answer,
  }
}

// What one look at `$.agent.list()` tells: the dispatch with its latches
// updated, and the status it ended on, if it did.
//
// A turn alone never says the run is over, and right after a `continue` the
// list can still show the previous run's `completed`. So the end needs a
// turn of this dispatch, or (for a repair of a run that had failed or been
// killed) a status that moved since the continue.
//
// An answered agent still `running` ANSWER_GRACE_MS after the answer is
// `stop`: the caller stops it, and the dispatch is then `isEnding`. Once the
// stop is answered, a look numbered after that answer shows whether it took:
// `killed`, `completed` or gone ends the dispatch `completed`. Still `running`
// STOP_CONFIRM_MS after the stop, `isUnconfirmed` says so once.
export function observe(
  d: Dispatch,
  listed: Listed | undefined,
  look: Look,
): { dispatch: Dispatch; ended: string | undefined; stop: boolean; isUnconfirmed: boolean } {
  const moved = d.moved || (listed !== undefined && listed.status !== d.startStatus)
  const seen = d.seen || listed !== undefined
  const answeredAt = d.isAnswered && d.answeredAt === undefined ? look.now : d.answeredAt
  const next = { ...d, moved, seen, last: listed ? listed.status : d.last, answeredAt }
  const quiet = { stop: false, isUnconfirmed: false }
  if (d.isStopped) return { dispatch: next, ended: undefined, ...quiet }
  if (d.isEnding) {
    const isConfirmable = d.stopSeq !== undefined && look.seq > d.stopSeq
    if (listed && !ENDED.includes(listed.status)) {
      const isLate = d.stopSentAt !== undefined && look.now - d.stopSentAt >= STOP_CONFIRM_MS && !d.isNoted
      return { dispatch: isLate ? { ...next, isNoted: true } : next, ended: undefined, stop: false, isUnconfirmed: isLate }
    }
    if (!isConfirmable) return { dispatch: next, ended: undefined, ...quiet }
    return { dispatch: next, ended: listed?.status === 'failed' ? 'failed' : 'completed', ...quiet }
  }
  if (
    listed?.status === 'running' &&
    d.turns > 0 &&
    d.isAnswered &&
    !d.stopFailed &&
    answeredAt !== undefined &&
    look.now - answeredAt >= ANSWER_GRACE_MS
  ) {
    return { dispatch: { ...next, isEnding: true }, ended: undefined, stop: true, isUnconfirmed: false }
  }
  const settled = observeEnd(next, listed)
  return { dispatch: next, ended: settled, ...quiet }
}

// The end today's rule reads from one look, given the dispatch with this
// look's `moved` and `seen` already latched.
function observeEnd(dispatch: Dispatch, listed: Listed | undefined): string | undefined {
  const { moved, seen } = dispatch
  if (!listed) {
    // Evicted after it ended: a turn was its answer; with none, it vanished.
    // A repair's agent was listed (stale) at the continue already, so its
    // vanishing counts only once its status has moved since: before that,
    // the stale listing may simply have been evicted ahead of the resumed one.
    if (dispatch.turns > 0) return dispatch.last && ENDED.includes(dispatch.last) ? dispatch.last : 'completed'
    const vanished = seen && (dispatch.startStatus === undefined || moved)
    return vanished ? 'failed' : undefined
  }
  if (!ENDED.includes(listed.status)) return undefined
  if (dispatch.turns > 0) return listed.status
  const isEndedWithoutTurn = moved && ENDED_WITHOUT_TURN.includes(listed.status)
  return isEndedWithoutTurn ? listed.status : undefined
}

// An agent the plugin stopped reports its kept answer and, after a blank line, why it was cut short.
export function stoppedPayload(d: Dispatch, status: string) {
  const answer = d.isEnding && status === 'completed' ? `${d.answer}\n\n${PLUGIN_STOP_NOTE}` : d.answer
  return { agent: d.agent, status, usage: d.usage, answer }
}

// What `relais native usage` takes: the dispatch's usage cumulative over its
// turns so far, and the ids of the turn just added.
export function usagePayload(d: Dispatch, messageIds: string[]) {
  return { agent: d.agent, turns: d.turns, usage: d.usage, message_ids: messageIds }
}
