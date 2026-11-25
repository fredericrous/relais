---
status: active
branch: prog/follow-ups/f20-clear-routes-on-stale-state
repos: [relais]
adrs: []
plan-id: p-01M8721G51AZP1C2WBWRV3H8J5
---
# Route a relayed session on its own state

## Review panel
👉 **Decide:** none — approve if a relayed opening prompt is fairly treated as the session's first request.
📍 relais · 3 phases · 1 step for you · next: Phase 1 via relais. Panel: backend, lang:typescript, tui, unix (opus-5-5).
**Changed by review:** launch waits before reading state; notes reach a toast with no run; "routing task" is not a programme ticket; Phase 2 files the "park, never cross programmes" follow-up you chose.
📄 Full reviews: [2026-11-24-route-relayed-session-own-state.reviews.md](2026-11-24-route-relayed-session-own-state.reviews.md)
**Verdicts:** 2 approve, 2 approve-with-changes; 9 carried (fx.kv marker, b2 asserts the wait, "six tests"); words 1013 → 1225 (×1.21); remove 0, merge 0.

## Picture
```
 today
 -----
 /clear --> relay prompt (origin plugin) --> not classified
            r.session = old id, r.task none, state stale
            spawn relais-review --> routes on stale state
                                    row: old id, or no row

 after this plan
 ---------------
 /clear --> r.session reset --> relay prompt --> classified
                                 (first after clear) routing task
                                                     opened,
                                                     state refreshed
            spawn relais-review --> waits for refresh (<= 2 s)
                                    row: new id, reason pin
```
A launch after a clear used to route on the old session's state and id;
after this plan the relayed opening prompt refreshes and opens a routing
task (the router's ledger unit, not a programme ticket), so
the launch is pinned and recorded under the new session.

## Context

Follow-up f20 (`docs/programmes/follow-ups/f20-clear-routes-on-stale-state.md`).
On 2026-11-24 the live check of the `preferred-model` pin ran in a session
that a `/clear` relay had started. Before any typed prompt, a pinned
`relais-review` launch was table-routed to Haiku (it should have been
pinned), and the explicit `model: "opus"` launch wrote no decision row. Both
were filed under the cleared session's id. The next session therefore
carried a workaround ("type a prompt before launching anything").

The code shows why (`claude-plugin/hooks/routing.ts`, `register.ts`):

1. `onSessionEnd` (`routing.ts:851`) at `clear`/`resume` ends the task and
   marks the state stale, but keeps `r.session`. `loadRouterState` then
   reads `r.session ?? fx.session.id()`, and `queue()` files every row
   under the old id.
2. Only `classifyPrompt` corrects the session id and opens a task, and it
   runs only for `PERSON_ORIGINS` (`composer`, `bridge`, `sdk`,
   `register.ts:59`). The clear-relay plugin submits its prompt with
   `origin: { kind: 'plugin', name: 'clear-relay' }`, so the relayed
   session's opening request is never classified: no task, no refresh.
3. `routeSpawn` (`routing.ts:385`) reads `r.state` as it is and never
   waits for a pending or stale refresh.
4. `afterSpawn` queues a decision row only when an owner task exists
   (`routing.ts:451`), so a pinned launch with no open parent task leaves
   no row.
5. `loadRouterState` sets `r.state = undefined` on any failure with no note,
   which turns routing and recording off silently ("dark session", round 2
   of the check). A failed final flush is also dropped with no note
   (`observe`, `routing.ts:952-953`).

Intended outcome: a session that a relay opens routes and records exactly
like one the person opens. The ledger check no longer needs a typed prompt
first, and a session that stops recording leaves a note saying why.

## Goal

After a `/clear` or `/resume`, every row uses the new session id. The
relayed opening prompt opens a routing task the way a typed prompt does. A launch
made while router-state is still loading waits for it, within the hook's
budget. A router-state read or final flush that fails leaves a note.

## Non-goals

- No name of another plugin in relais code (Green Harness parts stay
  optional): the rule is about origin and timing, not `clear-relay`.
- Prompts from other plugins later in a session stay unclassified.
- No change to programmes: a routing task (`r.task`, the `task_id` of
  `router_decisions`) is not a programme ticket, and the ticket a relay
  resumes is still the one `relais programme next --claim` reserved at the
  handoff.
- A launch with no open parent task still writes no decision row
  (`routing.ts:451`); out of scope.
- No change to the pin decision itself (`decideSpawn`) or to f19 (which
  model the pin row records).
- Not finding the cause of round 2's dark session: this plan makes it
  visible (a note) instead of silent.

## Code review
- relais: backend, lang:typescript (unix, tui, architect left out: plugin hook logic and its tests, no CLI or module added)

## Behaviour

- **Session id at a clear:** `onSessionEnd` with reason `clear` or
  `resume` sets `r.session = undefined` after the final flush. The
  `session.start` refresh and every later row use the new session's id;
  `routeSpawn` sets `r.session` from `fx.session.id()` when it is unset, so
  no row is dropped by `queue()`.
- **The relayed opening prompt:** the first prompt after a `clear` or
  `resume` whose origin is `plugin` (and not this plugin's own,
  `store.ownPrompts`) goes through `onPrompt` like a person's prompt:
  classified, routing task opened, state refreshed. The window closes at the first
  non-notification prompt of any origin (task-notifications do not close it), so later plugin prompts stay unclassified.
  `store.lastPersonPrompt` (the decide tool's quote source) stays typed
  prompts only.
- **Launches during a refresh:** when `r.isStale` or `r.refreshing` is
  set, `routeSpawn` first, before it reads `r.state`, awaits
  `within(fx, refresh(fx, store), waitFor(remainingMs))` (`refresh` starts
  one when none is running; the same 2 s / budget-margin bound as the
  classifier). Only then does it read `r.state` and decide. The
  classifier's wait gets `remainingMs` minus the time already waited.
- **Visible failures:** a failed `router-state` read (non-zero exit, a
  throw or the 10 s timeout, or exit 0 with stdout `parseRouterState`
  rejects) leaves one note per failure streak; a streak ends at the next
  read that parses. The note reads "session routing is off for this
  session: router-state failed (<exit code | timeout | unparseable>):
  <stderr, clip(…, 400)>; to reproduce: relais native router-state
  --session <id>", with the real id. It goes through `note()`, and when
  `store.models` holds no run (a relayed session before any launch) through
  `fx.ui.toast` instead, as `learning.ts:115-116` does. A final flush that
  fails is kept as a marker and noted once at the next session start,
  since a note at session end may never be seen.

## Phases

- [x] Phase 1 — red tests, then the fix: `claude-plugin/hooks/routing.ts`,
  `claude-plugin/hooks/register.ts`, `claude-plugin/hooks/router.ts` (new
  `RouterMemory` fields), `claude-plugin/tests/routing.test.ts`,
  `claude-plugin/tests/routing-support.ts`. Done through relais
  (`kind: change`, task type `review-fix`). The tests are red before the fix:
  (a) clear, then `script.sessionId = 'session-2'` and `session.start` /
  `classic.SessionStart(clear)` fired, then a plugin-origin prompt, then a pinned `relais-review`
  launch: the launch row has the new session id, `reason: pin`,
  `applied: false`, and the launch keeps its model;
  (b) the same launch with no prompt at all, made while a slow
  router-state is pending: it waits and sees the pins; (b2) a router-state
  that finishes after 2 s: the launch leaves within budget and keeps its
  model (no parent task, so no row: see Non-goals);
  (c) with no run in the store, a failing router-state gives one toast, for
  a non-zero exit and for unparseable stdout; failure, failure, success,
  failure gives two; (d) a failing final flush is noted at the next
  session start; (e) a plugin-origin prompt later in the session is still not
  classified.
- [ ] Phase 2 — close f20: `docs/programmes/follow-ups/f20-clear-routes-on-stale-state.md`
  (status closed, answer), `docs/plans/2026-11-23-router-keeps-preferred-model.md`
  (Observed: the typed-prompt precondition is lifted once the release is
  installed), `CHANGELOG.md`; and file a follow-up ticket (the next free
  `fNN`, f21 is taken on an open branch) in `docs/programmes/follow-ups/`:
  `relais programme next --claim` never crosses programmes; with no ready
  ticket in the `--from` programme it parks, and starting another programme
  is the person's call. It needs ADR-0083 (`work.programme-map`) amended in
  the decisions corpus first.
- [ ] Phase 3 — release relais with the `tag-release` skill (the plugin
  ships with it), after the PR merges: `CHANGELOG.md`.

### 🧑 Your steps
- `/plugin install relais@relais-local` — because a plugin reinstall is yours; the fix runs only from the reinstalled plugin

## Decision log
- 2026-11-24 — Classify the first post-clear plugin-origin prompt instead of
  naming clear-relay, because relais must not depend on another part's name
  and that prompt is the relayed session's opening request.
- 2026-11-24 — The person chose: after a clear, the next ticket never comes
  from another programme; with none ready, the handoff parks. Out of this
  plan's scope (ticket choice is `relais programme next`, not the router),
  so it is filed as a follow-up in Phase 2.

## Verification

- `scripts/plugin-test.sh` (the `plugin-test` check): the five new tests
  red on `origin/main`, green after the fix, whole suite green.
- Each test (a)-(e), (b2) by name: red on `origin/main` with the assertion
  that fails, green after the fix.
- Piloted after release and reinstall: `/clear` with a relay armed, then
  let it submit; launch `relais-review` with `model: "opus"` before typing
  anything. Read `router_decisions` by that agent id: session id = the new
  session, `reason pin`, `applied 0`. That is also the plan's owed ledger
  check, which can then be recorded in a non-fresh session.

<!-- panel: repos=relais reviewers=backend,lang:typescript,tui,unix body-sha=a34bedbe9012 -->
