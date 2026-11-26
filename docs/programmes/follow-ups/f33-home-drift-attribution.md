+++
type = "task"
mode = "afk"
status = "open"
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
