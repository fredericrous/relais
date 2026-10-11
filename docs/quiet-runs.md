# Quiet runs: the active-run entry and the watcher protocol

amont (`amont run`, pre-commit checks) and `amont-agent quiet` keep a
check's output out of the agent's context: the output goes to a
`.log.tmp` file. So that a person can still see such a run live, each
tool writes one small JSON file while the run is active, and
`relais quiet watch --protocol` turns those files into JSON lines the
plugin draws in its pane. The tools never depend on the watcher.

## The directory

`$QUIET_ACTIVE_DIR` when set (every test sets it), else
`${XDG_STATE_HOME:-$HOME/.local/state}/quiet/active`, and
`%LOCALAPPDATA%\quiet\active` on Windows. The directory is 0700.

## The entry file

`<pid>-<n>.json`, 0600, written atomically (a temp file that does not end
in `.json`, then a rename). `<n>` tells apart the concurrent checks of one
process. The file's name without `.json` is the run's `id`.

| field     | type             | meaning                                                              |
| --------- | ---------------- | -------------------------------------------------------------------- |
| `v`       | number           | format version, `1`; the watcher ignores any other                   |
| `tool`    | string           | `amont` or `amont-agent`                                             |
| `session` | string or null   | `CLAUDE_CODE_SESSION_ID` of the starter; null outside Claude Code    |
| `check`   | string           | the check name, or the program name for `amont-agent quiet`          |
| `repo`    | string           | the git toplevel, else the working directory                         |
| `log`     | string           | absolute path of the `.log.tmp` the output goes to                   |
| `pid`     | number           | the writing process                                                  |
| `started` | number           | unix milliseconds                                                    |
| `exit`    | number or null   | only once ended: the exit code, null when the run was killed         |
| `ended`   | number           | only once ended: unix milliseconds                                   |

Unknown fields are ignored.

## Lifetime

1. The tool writes the entry when its capture opens.
2. When the run ends, the tool rewrites the entry with `exit` and `ended`
   and leaves it. A run that ends inside one 250 ms poll is still seen
   with its exit.
3. The watcher emits `ended` for an entry carrying `ended` and then
   deletes it. The check for `ended` comes before the pid check: the
   writer has usually exited by then. A tool's own sweep removes ended
   entries older than 60 s and its dead-pid entries.
4. An entry without `ended` whose pid is dead (`kill(pid, 0)` on unix,
   with EPERM counted as alive; `OpenProcess` on Windows) is ignored: it
   never yields `started`, and the watcher removes it only when the file
   is older than 1 h. A run the watcher already announced whose pid dies
   without `ended` (four polls in a row) ends with `exit` null.
5. A run whose entry file disappears ends with `exit` null.

## The protocol

`relais quiet watch --protocol` prints one JSON object per line on stdout,
each with `"relais":"quiet"`. It polls every 250 ms, never reads stdin,
and exits when a stdout write fails or its parent pid changes.

- `{"relais":"quiet","event":"started","id","session","tool","check","repo","started"}`
  when an entry is first seen live, or first seen already ended.
- `{"relais":"quiet","event":"output","id","lines":[...]}`: new log lines,
  oldest first.
- `{"relais":"quiet","event":"ended","id","exit","ended"}`; `exit` is null
  when the run vanished without one.
- `{"relais":"quiet","event":"heartbeat"}` every 5 s (the first at once), so a closed reader
  shows as a failed write. The plugin ignores it.

An entry first seen already ended yields `started`, then `output` with the
last lines of its log, then `ended`, in that order.

### Log tailing

- A new row starts at the last 16 KB of its log; the partial first line
  of a window that begins mid-log is dropped.
- A poll reads at most the last 64 KiB and emits at most 10 lines per
  run (the last 10). Lines skipped by either limit are not sent later.
- An incomplete trailing line is held until its newline arrives, and is
  emitted when the entry ends (the final drain).
- Lines split on `\n`, `\r` and `\r\n`.
- A log that shrank is read again from its start; a missing log yields no
  lines.
- Each line is cut as `cutLine` in `claude-plugin/hooks/timeline.ts`
  does: CSI sequences removed, then control characters other than tab,
  tabs to two spaces, and a line longer than 240 UTF-16 units cut to 239
  plus `…`. A surrogate pair the cut would split is dropped whole.

### The plugin

The plugin keeps every entry and draws only those whose `session` is one
it served; an entry with a null session is never drawn.
