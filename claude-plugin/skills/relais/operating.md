# Operating relais

Read on demand from the `relais` skill. Each section is a failure that
looked like relais being broken while the work had in fact gone through.

## After upgrading relais

The CLI, the Claude Code plugin and the coordinator daemon are three
copies of relais. An upgrade replaces only the first.

- **The plugin.** Run `relais doctor`. Its `plugin` line must say
  `relais@relais-local <version> is installed and enabled`, with the
  version `relais --version` prints. Any other line names the fix,
  `relais install --claude --write`. A refreshed plugin applies from the
  next Claude Code session, so a session that started before the upgrade
  still routes with the old one.
- **The coordinator.** The daemon keeps serving the binary it started
  with. A change of wire protocol is refused with a message that says to
  run `relais coordinator stop`. A fix inside the daemon that keeps the
  protocol is not detected: the old daemon goes on serving it. When no run
  is live (`mcp__relais__status` shows nothing waiting), run
  `relais coordinator stop`; the next relais command starts one from the
  new binary.

## A run in place owns the worktree

In a linked git worktree a run works in that tree itself.

- At the start, relais takes a snapshot of the tree, uncommitted work
  included. At the end it writes the result over the working tree: the
  accepted candidate's tree, or the snapshot for every other outcome.
  **An edit you make in that worktree while the run works is overwritten.**
- So, until the outcome arrives: no edits, no `git add`, no commit in that
  worktree. Work in another worktree, or wait.
- Nothing is thrown away silently. Each run's directory under
  `~/.local/state/relais/runs/<run>/` keeps `candidate-<n>.patch` for every
  attempt and, for an in-place run, `candidate-final.patch` with the work no
  candidate holds (untracked files included). `git apply` it to get the
  work back.
- To carry on from a candidate, name it (`"kind": "verify"` to check it,
  `from_candidate` to continue it) rather than applying a patch by hand: a
  hand-applied patch is not verified or accounted as relais's.
- An **interrupted** in-place run keeps its edits and a marker in the
  worktree. The next run there is refused with
  `workspace_has_interrupted_run` until the run is answered
  (`mcp__relais__decide`) or retired (`relais resume --retire <run>`).
  Retiring resets the tree to the start snapshot, so take what you need
  from `candidate-final.patch` first.
