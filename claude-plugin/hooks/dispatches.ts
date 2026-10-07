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
}

export type Listed = { id: string; status: string }

const ENDED = ['completed', 'failed', 'killed']
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
export function withTurn(
  d: Dispatch,
  turn: { usage?: TurnUsage; answer?: string },
): Dispatch {
  return {
    ...d,
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
export function observe(
  d: Dispatch,
  listed: Listed | undefined,
): { dispatch: Dispatch; ended: string | undefined } {
  const moved = d.moved || (listed !== undefined && listed.status !== d.startStatus)
  const seen = d.seen || listed !== undefined
  const next = { ...d, moved, seen, last: listed ? listed.status : d.last }
  if (d.isStopped) return { dispatch: next, ended: undefined }
  if (!listed) {
    // Evicted after it ended: a turn was its answer; with none, it vanished.
    // A repair's agent was listed (stale) at the continue already, so its
    // vanishing counts only once its status has moved since: before that,
    // the stale listing may simply have been evicted ahead of the resumed one.
    if (d.turns > 0) return { dispatch: next, ended: d.last && ENDED.includes(d.last) ? d.last : 'completed' }
    const vanished = seen && (d.startStatus === undefined || moved)
    return { dispatch: next, ended: vanished ? 'failed' : undefined }
  }
  if (!ENDED.includes(listed.status)) return { dispatch: next, ended: undefined }
  if (d.turns > 0) return { dispatch: next, ended: listed.status }
  const isEndedWithoutTurn = moved && ENDED_WITHOUT_TURN.includes(listed.status)
  return { dispatch: next, ended: isEndedWithoutTurn ? listed.status : undefined }
}

export function stoppedPayload(d: Dispatch, status: string) {
  return { agent: d.agent, status, usage: d.usage, answer: d.answer }
}
