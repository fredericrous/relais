+++
type = "task"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais"]
+++
# The run tool shows when a contract path falls back to absolute

## Question

f26 has the run tool ask `relais native contract --relative`, so that the
driver's argv names no worktree directory. If relais cannot build the
relative path, it prints the absolute path on stdout and says so on
stderr, still exiting 0. `contractPath` reads only stdout on exit 0, so
the run starts with the worktree in its argv and nobody sees the note.

Should the run tool's reply carry that stderr line? Or should the run
refuse instead? The root is found by walking up from the same cwd, so the
fallback should never fire. Measure that before building anything.

## Look at

- `claude-plugin/hooks/runs.ts`: `contractPath`, which discards stderr on
  exit 0.
- `crates/relais/src/cli/native.rs`: the `ContractPath::Relative` arm and
  its stderr line.
- `docs/plans/2026-11-25-worker-kills-spare-the-driver.md`: the Decision
  log entry that deferred this.

## Answer looks like

Either a count showing the fallback never fired in real runs, with the
ticket closed as not needed, or the run reply naming the fallback, with a
plugin test that scripts a stderr line on exit 0.

## Answer

Closed as not needed (2026-11-25): the fallback cannot fire on a live
cwd, so there is nothing for the reply to carry.

- From the code: `project_dir()` finds the root with `locate_repo_root`,
  which only `pop()`s the cwd it was given, with no canonicalizing. The
  saved path is that root joined with `.relais/tasks/<hex>.json`, which
  is ASCII. So `relative_task_path` strips both prefixes every time.
  The one way in is the second `cwd()` read differing from the first or
  failing. No non-test code in `crates/` calls `set_current_dir`, and
  the `fingerprint_hints` git calls between the two reads are
  subprocesses. What is left is the cwd being deleted or renamed during
  the command, and then the run fails right after anyway.
- Probes with relais v0.87.0, one contract, all exit 0 with no stderr:
  the repository root (`.relais/tasks/…`), `crates/relais/src` and
  `docs/programmes/follow-ups` (`../../../.relais/tasks/…`), and
  `crates` of a detached worktree reached through the `/tmp` symlink
  (`../.relais/tasks/…`).
- Real runs: the plugin carrying `--relative` (0.86.0) was installed at
  2026-11-25 22:19. The latest run in `relais status` started at 21:42,
  so 0 runs have used `--relative` so far. That count does not show the
  fallback never fires. The code argument above does.
