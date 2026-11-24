+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# A relayed session routes on the state from before /clear

## Question

On 2026-11-24 the live check of the `preferred-model` pin (plan
`docs/plans/2026-11-23-router-keeps-preferred-model.md`) ran in a session a
`/clear` relay had started. Before the person typed anything, one of two
`relais-review` spawns was table-routed, which is consistent with a router
state that had no pins (likely the pre-clear one):

- relais v0.81.0 (the pin) was installed at 10:17Z; the cleared session's
  final flush landed at 10:27:50Z; the spawns came at 10:28Z.
- The marked fallback spawn (no `model`) was table-routed to Haiku:
  `task-1795516121648-249`, reason `table`, tier `research`, applied 1,
  and its steps ran on `claude-haiku-5-5`.
- The explicit `model: "opus"` spawn wrote no decision row at all and ran
  on Opus. A spawn with no pin in the state would have been classified and
  table-routed, and a routed spawn opens its own task and writes a row. So
  it either saw the pin while no parent task existed (a pin row is queued
  only when `r.task` is open, and the clear ends the session's task), or
  its spawn classifier timed out. Either way the two spawns did not see
  the same state.
- `relais native router-state` returned `relais-review = opus` for both the
  old and the new session id, so the binary served the pin.
- The rows were filed under the cleared session's id (`52845f00…`), not
  the new one (`8bea5982…`): `r.session` survives the clear until the
  first `classifyPrompt`.
- The person's first typed prompt flushed round 1 (10:35:03Z). The same
  two spawns made after it ran on Opus with their calls unchanged, but no
  record of any kind from this session reached the ledger after that
  flush, over two more typed prompts. The ledger was searched by the four
  agent ids across all sessions and by `recorded_at` after 10:36Z across
  all sessions (only another session, `181a312d…`, wrote): no decision,
  `router_usage` or `continuation` rows under either id. So the session
  may have gone dark, not routed correctly. `router-state` answered in
  0.26 s, under the plugin's 10 s `STATE_TIMEOUT_MS`.

Two paths fit a dark session. `loadRouterState` sets `r.state` to
undefined on any failure without a note; `queue()` then drops every
record, and `onPrompt` returns before `classifyPrompt` when there is no
state and no stale mark, so nothing refreshes again. Or a `router-observe`
batch exited 2 (refused: the binary writes each batch in one transaction,
all or none) or 1, and every record in it was lost.

The installed plugin (0.79.0) `routeSpawn` reads `store.router.state` and
never checks `isStale`; the refreshes after a clear are the
`session.start` handler, a successful non-final `router-observe` flush
(`observe` refreshes on exit 0), `/relais-routing`, and `classifyPrompt` on
a person's prompt. The clear-relay plugin opens every relayed session with
a plugin-submitted prompt, which does not refresh, so this is the normal
relay path. Why did none of those refreshes give the spawns the pins, why
did the session go dark after round 1's flush, and what is the fix:
`routeSpawn` awaiting a refresh when the state is stale, a refresh on the
plugin-submitted prompt, resetting `r.session` at the clear, or a note
when router-state or router-observe fails?

## Look at

- `claude-plugin/hooks/routing.ts`: `routeSpawn`, `loadRouterState`
  (`r.session ?? fx.session.id()`, and `r.state = undefined` on failure),
  `refreshState` and `refresh` (a refresh already running is reused, and
  its end clears a stale mark set after it started), `onSessionEnd`
  (`isStale = true` on `clear`/`resume`), `onPrompt` (the early return),
  `classifyPrompt`, `queue`, and `observe` (the refresh after a successful
  non-final flush; what happens to a batch on exit 2 or 1, and where its
  stderr goes).
- `claude-plugin/hooks/register.ts`: the `session.start` handler, its order
  against `session.end` at a `/clear`, and whether the module reloads then.
- `crates/relais/src/cli/native.rs` `router-observe` and
  `crates/relais/src/ledger/router.rs` `record_router_batch`: the
  all-or-none batch.
- `router_decisions` rows for session `52845f00-76e2-4902-bf83-b7baf8b08c07`
  with `at` 2026-11-24T10:28Z, and any `router-observe` refusal for that
  session after 10:35Z.

## Answer looks like

The cause, shown by a test that goes red on it (a spawn right after a
clear, before any person prompt, sees the new state's pins; a failed
router-state or router-observe leaves a note), and the fix. Until it
lands, every relayed session can add matched `table` rows for pinned
reviewers, and a dark session adds unmatched ones: the plan's 7-day cohort
check (~2026-11-30) must count spawns made before a session's first
person prompt apart, rather than read them as a pin regression.
