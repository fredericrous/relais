+++
type = "task"
mode = "afk"
status = "open"
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
