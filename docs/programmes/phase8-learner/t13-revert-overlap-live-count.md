+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
due = "2026-12-01"
+++
# Count revert rows that overlap their own task's files

## Question

After t10's fix ships in a release and the plugin is reinstalled, how many
new `revert` rows in `router_reassess` share a file hash with their own
task's `files_json`?

## Look at

- The installed plugin version (it must include t10's commit). If it does
  not, re-date this ticket a week on and say why.
- `router_reassess` rows with `event = 'revert'` written after the
  reinstall, joined to `router_tasks` on session and task.
- The transcripts' Bash commands for the `git revert` rows that have no
  files.

## Answer looks like

Counted in three groups:
- rows from explicitly named paths that overlap their task;
- rows from a `.` or directory expansion, which overlap by construction;
- `git revert` rows with no files. Pair these with their commands to give
  the share whose lookup failed (expected under 10%).

If too few rows exist yet, re-date the ticket instead. If the overlap is
still zero, name the shape that misses.
