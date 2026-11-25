# Follow-ups

## Destination

Every follow-up a session notices in relais, outside the task it works, is a
ticket here until it is resolved or moved into a programme of its own.

## Notes

- The standing follow-ups map of ADR-0077 (`work.programme-map`): inbox lines
  filed at a boundary land here when no programme owns them.

## Decisions so far

- [f01](f01-flaky-end-session-tombstone-test.md) (2026-11-20): the
  claim passes wait out a brief lock (30 ms). A concurrent fork's copy of a
  freed flock read as held (#508).
- [f02](f02-claims-record-common-dir.md) (2026-11-20): claims do not
  record the git common dir. Their `repo` is already the main checkout
  (#430), which a worktree removal leaves in place; a test pins it.
- [f03](f03-pane-placed-after-resize.md) (2026-11-20): no change. A
  narrowing resize moves a placed pane inline above the prompt, never
  hides it, and `$.ui.panes()` keeps `isPlaced` true; live in tmux on
  Claude Code 2.1.329.
- [f04](f04-claim-pushes-branch.md) (2026-11-21): a claim does not push
  its `prog/` branch. A branch never expires and would make release and
  SessionEnd depend on the network; `frontier` already reads
  `origin/prog/*` with `--prune`, and the lasting fix is claims on the
  primary (homelab an-t02).
- [f05](f05-homewatch-injected.md) (2026-11-21): the engine gets its
  home as a `HomeWatch` on `RunConfig`, built at the CLI root;
  `home_override` is removed.
- [f07](f07-home-drift-output-lows.md) (2026-11-21): the three low
  output findings of #512 all held and are fixed: run-end problem lines
  carry `relais run: warning:`, the doc names `~\`, the no-home line is
  split under 80 columns.
- [f06](f06-homeprint-stream-and-absolute-paths.md) (2026-11-21): the
  home fingerprint does not read a file over 1 MiB (size, mtime and mode
  instead, as greenbelay#306 does); it caps the read and does not stream.
  `home_drift` records hold the absolute path, and `~/` is applied when
  they print.
- [f09](f09-claims-hook-lock-flake.md) (2026-11-21): the two claims hook
  tests bound a busy lock at 1 s, not 100 ms. The pass waits its 30 ms
  grace exactly; the overrun was the log append and tombstone write after
  it, which a loaded disk stretches to 25–95 ms. `ONCE_GRACE < 100 ms` is
  a compile-time check; the hook's real limit is 10 s.
- [f10](f10-blocked-run-candidate-missing.md) (2026-11-23): a run blocked
  `env_missing` keeps its worker's changed tree as the blocked attempt's
  candidate, named in its message, so `verify` and `from_candidate` find it
  (it was only exported as `candidate-final.patch`).
- [f11](f11-pricing-message-matches-toml.md) (2026-11-23): every
  unpriced-model message prints `[[pricing.models]]`, the array-of-tables
  header a person adds to machine.toml (runner, `native_unpriced`,
  fast-mode rate, `relais report`, plugin fixture); prose in docs is
  unchanged.
- [f12](f12-claim-takeover-live-session.md) (2026-11-24): the relayed
  session on 11-21 never claimed, so its handoff lapsed after 2 h. Now
  `relais programme adopt`, run by the plugin at the clear, makes it the
  successor's `work`; the board clause is not built (accepted gap).
- [f16](f16-adopt-week-count.md) (2026-11-24, open, due 2026-12-02): a
  week of relays after the plugin reinstall adopts every handoff.
- [f13](f13-lease-width-keys-shared.md) (2026-11-21, closed 2026-11-24):
  the lease env is built from `greenharness_slots::width_env`; a test pins
  its keys to `WIDTH_KEYS`. vitest/Go keys arrive with the next bump.
- [f14](f14-claim-lock-stderr-pinned.md) (2026-11-21, closed 2026-11-24):
  the claim CLI lock-timeout test asserts stderr holds `stayed held for`,
  the wording the driver's retry matches.
- [f15](f15-loop-breaker-wider-fingerprint.md) (2026-11-23, closed
  2026-11-24): no change, and no warn rung. Over 32 wall-timeout
  transcripts, Lumina's rule trips on 2 and the Bash rule reaches 3 on 2.
  Every one of them is a wait on a slow test or build. 78% of the wall time
  is inside tool calls, and no transcript has more than 140 calls.
- [f17](f17-piped-filter-polls.md) (2026-11-24, closed 2026-11-24): a
  piped filter counts only when its output shows a failure marker
  (`shows_failure`). The real wait polls went from 7 fires to 0.
- [f18](f18-timeout-wall-time-waits.md) (2026-11-24, closed 2026-11-24):
  41% of timed-out wall time passes after the worker's final answer. A
  background shell keeps the agent listed `running`, so the dispatch never
  settles. Suites take 24%, polls 16% (at most 9% wasted), model turns 14%;
  contention could not be shown to dominate. Act in stop detection (f21).
- [f19](f19-pin-row-records-call-model.md) (2026-11-24, closed 2026-11-24):
  a pin row now records the model the router infers the call ran (call
  model, else own `model:`, else the session's), and router-state serves
  `pin_sources` beside `pins`. An unknown preference is not validated: it
  cannot reach the row while the session model is known. Live check: f22.
- [f20](f20-clear-routes-on-stale-state.md) (2026-11-24, closed 2026-11-25): a
  relayed session now resets the session id at the clear, classifies its
  opening plugin prompt, waits for a pending router-state before a launch,
  and notes a failed router-state read or lost final flush.
- [f21](f21-settle-at-final-turn-with-shells.md) (2026-11-24, closed 2026-11-25):
  the plugin `TaskStop`s a worker that answered but is still listed
  `running` 5 s later, and ends the dispatch `completed` once a listing taken
  after the stop's answer shows it gone. `killed` would drop the answer.
  `TaskStop` sends SIGTERM to the whole tree, own-group children included.
  Every `TaskStop` is bounded at 30 s, and a late one may still take.
  Measure: f23.
- [f22](f22-pin-row-live-check.md) (2026-11-24, open, due 2026-12-01):
  check live that a pin row's `model` matches its subagent's step model,
  after the plugin reinstall and a relais that serves `pin_sources`.
- [f23](f23-settle-at-answer-week.md) (2026-11-25, open, due 2026-12-03):
  a week of worker stops settles within 60 s after the plugin reinstall,
  and no plugin-stopped attempt ends `TerminalResultMissing`. Also the
  live shell, process-group and slot checks.
- [f24](f24-stop-an-orphaned-worker.md) (2026-11-25, closed): `cancel` of
  a run that has ended now stops the agents its dispatches bound that still
  run, with one reply line each. The model's `TaskStop` stays denied, and
  the deny names the run to cancel, or sends the model to the person when
  the plugin no longer knows the run.
- [f25](f25-guard-fails-open-on-list-error.md) (2026-11-25, closed): the
  `SendMessage`/`TaskStop` guard reads the store before the agent list and
  catches a failed list itself: when the hook threw, the call reached the
  tool whatever its `.catch` answered (`claude plugin test`, 2.1.331).
- [f26](f26-worker-kills-spare-the-driver.md) (2026-11-25, open): keep a
  worker's `pkill -f <path>` off its own driver, through worker guidance
  or a runner change.
- [f27](f27-next-claim-stays-in-programme.md) (2026-11-25, open):
  `programme next --claim` never crosses programmes; with no ready ticket
  in `--from` it parks (ADR-0083 amended first).

## Handoff

2026-11-25, boundary 3 (f25 closed): the model's `TaskStop`/`SendMessage`
of a store-bound relais worker stays denied when the agent list throws. Before
it, f24 (`mcp__relais__cancel` of an ended run stops its workers, plan
`docs/plans/2026-11-25-cancel-stops-orphaned-workers.md`). Both plugin
changes ship with the next relais release and run after the plugin reinstall.

- Done: f12, f13, f14, f15, f17, f18, f19, f21, f24, f25.
- Next:
  - f20 (stale state after a clear) is held by session
    `route-relayed-session-state` in `relais-wt-f20`.
  - Keep a worker's kills off its own driver (f26), ready.
  - Measure the home fingerprint warnings (f08), due 2026-11-28.
  - A pin row's model matches the model its subagent ran (f22), due
    2026-12-01, after the plugin reinstall.
  - A week of relays adopts every handoff (f16), due 2026-12-02.
  - A week of worker stops settles within 60 s (f23), due 2026-12-03.
  - f16, f22 and f23 are counted from the reinstall.
- Lessons:
  - In `claude plugin test` on 2.1.331, the guard's `tool.call` `.catch`
    did not decide the call: the hook threw and the tool ran whatever
    `.catch` returned. Unverified live or for other hooks; a guard catches
    its own `$` failures inside the hook body (f25).
  - Until the reinstall, a worker that outlives its driver still cannot be
    stopped from the session: wait for its turn to end (watch its
    transcript for `end_turn`), then salvage the tree.
  - The plugin test runner has no per-file filter, and its 5 s limit per
    test is fixed. Drive any bound with the scripted clock
    (`engine.clock.advance`) and keep the call pending, as
    `cancel-orphans.test.ts` does. At load 90 to 100, unrelated tests time
    out; rerun once the load drops and read the counts.
  - A hooks module refuses `arguments` at load. A `console.log` in a hook
    did not show in the test output once (unverified whether it ever
    does). Debug through a test's own assertions.
  - To check that new tests fail on origin/main, use a detached worktree
    (`git worktree add --detach`) with the test files copied in. Restore it
    and remove it without `--force`.
- Your steps: reinstall the relais plugin with relais's own install
  command for Claude (it is at 0.72.0 here). f16, f22 and f23 count from the
  session after the reinstall; f24's cancel works from then on.

## Not yet specified

None.

## Out of scope

None.
