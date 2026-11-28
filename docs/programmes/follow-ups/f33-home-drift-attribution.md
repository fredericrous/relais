+++
type = "task"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais"]
+++
# Attribute each home drift to a tool call

## Question

A `home_drift` row holds only the path and `modified`. About 98% of drifts are
changes the person asked for; the point of the net is the 1 to 2% the LLM
makes alone. Which per-run attribution makes an unexplained drift stand out:
the run's tool calls (Edit, Write, Bash naming the path) that touched the
path, and known tool writers (plugin and marketplace JSON, settings writes
through `SharedState`, greengate keys in `.git/config`)?

## Look at

- `crates/relais/src/runner` home watch (`HomeWatch`) and `home-drift-N.json`.
- relais ledger `evidence` rows of kind `home_drift` (46 on 2026-11-26).
- f08, which counts unexplained and explained drifts.

## Answer looks like

Each drift row says explained (by which call or tool) or unexplained, and the
run-end warning shows unexplained drifts first and collapses the explained
ones to a count. A test replays the 46 rows' path mix and checks that the
three known writers are explained and a path no call touched is not.

## Resolution

2026-11-28: shipped in relais v0.93.0 (#637, released in #640), under the
plan `docs/plans/2026-11-26-attribute-home-drift-writer.md`. Each new
`home_drift` row carries a `cause`. It is `call` when a worker's own tool call
wrote the path (an Edit/Write file path, a Bash write target, or a writing git
form for `.git/config`). It is `writer` when only a known writer's
bookkeeping changed (`claude code plugin refresh` for the two plugin JSONs,
`git branch tracking` for `[branch]` sections). Otherwise it is
`unexplained`. The ticket's third writer was wrong: `SharedState` writes
`machine.toml`, which is not watched. The warning, `status` and `explain` list
unexplained drifts first, then worker-named ones, then one count line per
known writer. The replay test covers 49 rows, not 46. Piloted on
run-6614c34d3f6a2-141f7: a `git worktree add -b` during the run gave one
`writer` row and only the count line.
