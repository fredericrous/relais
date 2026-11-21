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

## Handoff

2026-11-21, boundary 3 (f07 closed): three home drift output fixes; the
fix and this closing note ride one PR.

- Done: f07 (unreleased), and earlier today f05 (a refactor) and f04 (a
  decision); relais v0.75.0 is out.
- Next: f06 is in flight in another session (worktree
  `relais-wt-f06-homeprint`); Measure the home fingerprint warnings (f08)
  is due 2026-11-27; the claims hook lock tests' CI flake (f09, filed
  when it held the v0.75.0 release gate) is ready for an agent.
- Lessons:
  - A relais run can wait out its whole admission budget (30 min) on a
    host at load 40+ and launch nothing; three times now. Then apply its
    kept `candidate-1.patch` and run the checks by hand.
  - The relais plugin refuses a Bash call whose text names its own
    install command, even inside a heredoc: edit such text with the Edit
    tool.
  - A tmux repro of Claude Code needs `Down` then Enter on the trust dialog
    (its default is `No, exit`); a `/clear` typed while an answer still
    streams is queued into the prompt instead of run.
- Your steps: `relais install --claude --write` (the plugin is at 0.72.0
  here; it applies from the next session).

## Not yet specified

None.

## Out of scope

None.
