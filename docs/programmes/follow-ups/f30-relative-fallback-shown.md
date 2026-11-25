+++
type = "task"
mode = "afk"
status = "open"
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
