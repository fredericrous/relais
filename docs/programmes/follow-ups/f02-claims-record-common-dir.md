+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# Claims record the git common dir

## Question

Should a claim record its repository's git common dir, so `relais programme session` finds the map after the task worktree that claimed it was removed, without `--dir`?

## Look at

`crates/relais/src/programme/claim/rules.rs` (`ClaimRecord.repo` is a display path), `cli/programme.rs` `session_map_dir`, and `docs/plans/2026-11-20-pane-shows-programme-and-clear.md` Decision log.

## Answer looks like

A decision, and if yes a new optional field written by `claim` / `next --claim`, read first by `session_map_dir`, with a test on a removed worktree.
