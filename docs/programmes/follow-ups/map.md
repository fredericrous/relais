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
- [f12](f12-claim-takeover-live-session.md) (2026-11-21, open): a work
  claim was taken over from a live session after a `/clear` relay, and two
  sessions edited one worktree (filed from the inbox, orphan of 0f2c38ac).
- [f13](f13-lease-width-keys-shared.md) (2026-11-21, open): the lease's
  width variables are hand-listed in `verify/heavy.rs`. Build them from
  `greenharness_slots::width_env` once it ships (orphan of edf02f7e).
- [f14](f14-claim-lock-stderr-pinned.md) (2026-11-21, open): pin the
  `stayed held for` wording that the driver's retry matches with a claim
  CLI test (orphan of 4bca21ec).
- [f15](f15-loop-breaker-wider-fingerprint.md) (2026-11-23, open): replay
  Lumina's no-progress rule (every tool, normalized results, failures
  stale) over the wall-timeout transcripts the #418 Bash rule found no
  loop in; widen the shadow fingerprint, add a warn rung, or no change.

## Handoff

2026-11-23, boundary 3 (f11 closed): the unpriced-model messages print
`[[pricing.models]]`, as machine.toml writes the entry (message-only
change, unreleased).

- Done: f11; f10 earlier today (unreleased).
- Next: f12 (claim takeover from a live session), f13, f14, f15 are
  ready; Measure the home fingerprint warnings (f08) is due 2026-11-28.
- Lessons:
  - A replace of `[pricing.models]` must not touch the TOML fixtures'
    `[[pricing.models]]`: match with lookarounds
    (`(?<!\[)\[pricing\.models\](?!\])`), and check none read
    `[[[`.
  - The plugin's `unpriced_reason` fixture and comment mirror the Rust
    message text; change them together.
  - A relais run can wait out its whole admission budget (30 min) on a
    host at load 40+ and launch nothing; then apply its kept
    `candidate-1.patch` and run the checks by hand.
  - The relais plugin refuses a Bash call whose text names its own
    install command, even inside a heredoc: edit such text with the Edit
    tool.
- Your steps: reinstall the relais plugin with relais's own install
  command for Claude (the plugin is at 0.72.0 here; it applies from the
  next session).

## Not yet specified

None.

## Out of scope

None.
