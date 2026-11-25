+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
due = "2026-12-03"
+++
# A week of worker stops settles within 60 s

## Question

[f21](f21-settle-at-final-turn-with-shells.md) made the plugin stop a
worker that answered but is still held open by its own background shell,
and report it `completed`. Before it, 9 of 9 workers that stopped with a
shell running settled 124–1,108 s late. Does the change hold live?

- Over a week from the plugin reinstall, does any relais worker's
  `SubagentStop` in `hook_journal` settle more than 60 s before its
  dispatch's `dispatch_ended`?
- Does any attempt end `TerminalResultMissing` after the plugin stopped
  its worker (the answer carries the `relais plugin: the worker ended its
  turn …` note)? It should be none.
- How many `TaskStop`s did the plugin send per dispatch? It should be at
  most 1.
- How many "still runs 30 s after its stop" and "no answer in 30 s" notes
  were written? A rising count points to stops that do not take.

## Look at

- `~/.local/state/relais/hook_journal.jsonl*`: `SubagentStop` payloads of
  relais workers (`background_tasks` with a `running` shell), joined to the
  ledger's `dispatch_ended` by agent id, as f18's
  [`wall_time_split.py`](f18-assets/wall_time_split.py) does.
- The attempts' `WorkerResult` evidence for the note.
- One live run, in the first session after the reinstall: a worker that
  ends with a background shell. Check that `pgrep` shows the shell and every
  process group it started (`pgrep -g`) gone, and that the attempt is
  verified. With a free host slot, run the same probe with
  `greengate slot run -- sleep 988`. The slot should come back in
  `greengate slots --json`, and the job's own pid should be gone
  (`kill -0` fails).

## Answer looks like

The counts above, with the week's dates. Close it if no stop settles more
than 60 s late and no plugin-stopped attempt ends `TerminalResultMissing`.
Otherwise, name the runs and the cause.
