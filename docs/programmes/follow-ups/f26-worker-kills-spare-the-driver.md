+++
type = "task"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais"]
+++
# Keep a worker's process kills off its own driver

## Question

On 2026-11-25, a worker of run `run-6610ef826b9a0-5ea8` ran
`pkill -f "relais-wt-f21"`. The pattern matched its own relais driver's
argv, which holds the worktree path. The run ended `interrupted` and the
worker kept editing in place. f24 made `cancel` of the ended run stop that
worker; nothing keeps it from happening again.

Which one prevents it?

- Worker guidance: kill by pid, never `pkill -f` on a path.
- A runner-side change: the driver's argv no longer holds the worktree
  path, so such a pattern cannot match it.
- Both.

## Look at

- The worker prompt the runner builds for a native dispatch.
- The driver's argv (`relais run --task … --protocol`, its cwd), and what
  `ps -o args` shows for it.
- f24's plan: `docs/plans/2026-11-25-cancel-stops-orphaned-workers.md`.

## Answer looks like

A decision with its reason, and the change: guidance text (with its pinned
test, `cargo test -p relais --lib install::`) or a runner change, with a
test.

## Resolution

2026-11-25. Both changes were made, in
`docs/plans/2026-11-25-worker-kills-spare-the-driver.md`.

- **Runner.** `relais native contract --relative` prints the saved path
  relative to its physical cwd (`.relais/tasks/<hash>.json`, or `../` steps
  up to it). The run tool asks for that form, so `relais run --task` names
  no worktree directory, and a worker's `pkill -f <worktree>` misses the
  driver. Piloted with the built binary: without the flag the path is
  `/private/tmp/…/relais-wt-pilot/.relais/tasks/…`, the shape that matched
  on 2026-11-25; with it, `.relais/tasks/…`.
  `decide` keeps the absolute path for a successor contract.
- **Guidance.** The worker rules say: stop a process only by the pid you
  recorded (`$!`); never by a pattern (`pkill`, `killall`, `pgrep -f`,
  `ps | grep`), and never `kill 0` or `kill -1`. For a command that starts
  children, use `kill $(pgrep -P <pid>) <pid>`, run again on each child,
  killing the deepest first. `set -m` was dropped because `zsh -c` refuses
  it. Both prompt pins assert the rule.
- Argv alone misses `pkill relais`, and guidance alone depends on the
  worker obeying, so it is both.
- Open: the live `pgrep -fl <worktree>` check on a real driver waits for
  the release and the plugin reinstall. That the plugin drops the stderr
  note when `--relative` falls back is f30.
