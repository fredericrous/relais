# Quiet runs: the watcher protocol

amont (`amont run`, pre-commit checks) and `amont-agent quiet` keep a
check's output out of the agent's context, and write one small JSON file
while each such run is active. `relais quiet watch --protocol` turns those
files into JSON lines the plugin draws in its pane. The writers never
depend on the watcher.

The file itself (its directory, its name, its fields and the `v` version
token, how and when a writer writes and sweeps it) is amont's contract:
amont `docs/quiet-runs.md` (decisions ADR-0039, `harness.cross-tool-reads`).
This page says only what relais does with it.

## What the watcher does with an entry

1. Each watcher emits `ended` for an entry carrying `ended` exactly once
   and then holds it as a closed run. Watchers share the directory, so
   none deletes an ended entry younger than 60 s; any watcher or writer
   sweep removes it after that. An ended entry older than 60 s that a
   watcher never announced is deleted without an event; a watcher that
   starts within 60 s of an end (a new or restarted session) announces it
   once, so a failure that recent is drawn there too. The 60 s compare
   `ended` with the watcher's clock; both are the same machine's. The check for
   `ended` comes before the pid check: the writer has usually exited by
   then.
2. An entry without `ended` whose pid is dead (`kill(pid, 0)` on unix,
   with EPERM counted as alive; `OpenProcess` on Windows) is ignored: it
   never yields `started`, and the watcher removes it only when the file
   is older than 1 h. A run the watcher already announced whose pid dies
   without `ended` (four polls in a row) ends with `exit` null.
3. A run whose entry file disappears ends with `exit` null.
4. An entry whose `v` is not 1 is ignored.

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
