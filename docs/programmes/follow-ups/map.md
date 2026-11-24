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
- [f18](f18-timeout-wall-time-waits.md) (2026-11-24, open): what a
  timed-out worker's wall time waits on (suite, host contention, polling).

## Handoff

2026-11-24, boundary 4 (f17 closed): the loop guard counts a piped
`tail`/`head`/`grep` only when its output shows a failure marker. The
markers are `FAIL` as a word, `error:`/`error[`/`Error:`, `panicked`,
`fatal:`, `Traceback`, and a count of at least one before
`failed`/`error`. Replayed over the two wait-poll transcripts, the fires
went from 7 to 0. This is unreleased, like f10, f11 and f12.
`loop_threshold` stays unset until the Rollout bar is met: at least 5 shadow
events, at most 1 in 5 passing verification.

- Done: f12, f13, f14, f15, f17.
- Next: What a timed-out worker's wall time waits on (f18) is ready.
  Measure the home fingerprint warnings (f08) is due 2026-11-28. A week of
  relays adopts every handoff (f16) is due 2026-12-02, counted from the
  plugin reinstall.
- Lessons:
  - A timed-out dispatch has no `agent_id` in the ledger. Find its
    transcript by the dispatch's `session_id` and the subagent file whose
    first line comes within 2 min of `dispatch_started`.
  - `grep 'relais: shadow:'` also matches run prose. Count only lines whose
    event text starts with it.
  - A Bash result with no output is stored as the literal text
    `(Bash completed with no output)`, not as an empty string. Build
    transcript tests from that text.
  - Replay a real transcript through `LoopGuard::push_chunk` in a
    temporary `#[ignore]` test, once with the old rule and once with the new.
    It costs two builds and gives a before and after.
  - The 11-21 takeover was no fault of the takeover rule: the successor
    held no claim. Read `claims/log.jsonl` and the transcripts before
    blaming a rule.
  - Mock-clock tests in the plugin cannot advance hours: every 100 ms
    flush timer fires. Test a long wait by calling the hook function with
    a fake `fx` whose clock is already past it.
  - The claim CLI tests move time by backdating `claimed_at` and
    `refreshed_at` in the claim files (`World::age_claims`).
  - A commit here runs the whole cargo test suite (about 10 min); make no
    tracked edit while it runs, since the gate stashes them.
- Your steps: reinstall the relais plugin with relais's own install
  command for Claude (it is at 0.72.0 here; the adoption applies from the
  next session after the reinstall, and f16 counts from then).

## Not yet specified

None.

## Out of scope

None.
