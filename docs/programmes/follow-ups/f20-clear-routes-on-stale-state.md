+++
type = "task"
mode = "afk"
status = "closed"
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

The likeliest path to a dark session: `loadRouterState` sets `r.state` to
undefined on any failure without a note; `queue()` then drops every
record, `routeSpawn` passes every call through (which also fits round 2's
unchanged calls), and `onPrompt` returns before `classifyPrompt` when
there is no state and no stale mark, so nothing refreshes again until
`/relais-routing` or the next `session.start`. A failed `router-observe`
is less likely: exit 2 (refused, all or none, nothing written) is
dropped with a note, and exit 1 (ledger failure) is retried up to
`OBSERVE_RETRIES` times, then dropped with a note; no such note appeared
in this session. Only a failed final flush (`isFinal`, at session end) is
dropped with no retry and no note.

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
when router-state fails? And should a failed final flush be noted or
kept for the next session instead of dropped silently?

## Look at

- `claude-plugin/hooks/routing.ts`: `routeSpawn`, `loadRouterState`
  (`r.session ?? fx.session.id()`, and `r.state = undefined` on failure),
  `refreshState` and `refresh` (a refresh already running is reused, and
  its end clears a stale mark set after it started), `onSessionEnd`
  (`isStale = true` on `clear`/`resume`), `onPrompt` (the early return),
  `classifyPrompt`, `queue`, and `observe` (the refresh after a successful
  non-final flush; the notes on exit 2 and after the retries; the silent
  drop of a failed final flush), and where `note(fx, store, undefined, …)`
  lands, to tell whether a note could have been missed.
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
router-state load and a failed final flush each leave a note), and the
fix. Until it
lands, every relayed session can add matched `table` rows for pinned
reviewers, and a dark session adds unmatched ones: the plan's 7-day cohort
check (~2026-11-30) must count spawns made before a session's first
person prompt apart, rather than read them as a pin regression.

## Resolution

**Four gaps in the plugin, all fixed (plan
`docs/plans/2026-11-24-route-relayed-session-own-state.md`).**

- `onSessionEnd` at `clear`/`resume` kept `r.session`, so every row after
  the clear was filed under the old id. It now resets it after the final
  flush; `routeSpawn` reads the id when it is unset.
- The relayed opening prompt comes with `origin.kind = 'plugin'` and was
  never classified: no routing task, no refresh. The first prompt after a
  clear or resume from any plugin other than relais itself now goes
  through `onPrompt`; the window closes at the first prompt of any origin.
- `routeSpawn` read whatever state was there. It now waits for a stale or
  pending refresh, within the classifier's bound (2 s, the hook budget less
  its margin), before it reads the state.
- A failed router-state read (exit, timeout, unparseable stdout) left the
  session dark with no word. It now leaves one note per failure streak,
  as a toast when no run exists, with the command to reproduce it. A
  failed final flush is kept in the plugin's store and reported at the
  next session start.

Round 2's dark session is not explained: it is made visible, so the next
occurrence says why. Seven tests went red on the base and are green after
the fix. A pinned launch with no open parent task still writes no row
(out of scope). Until the release is installed, the 11-30 cohort check
counts spawns made before a session's first typed prompt apart.
