+++
type = "grilling"
mode = "hitl"
status = "open"
blocked_by = ["t01-verdict-consumers"]
repos = ["relais"]
+++
# Where review happens once relais's reviewer is removed

## Question

relais's semantic reviewer goes; a relais run inside a planned
worktree-task gets implementation-review at F4b. What reviews a relais run
outside one (a one-sentence change, a run in a repository without the
workflow skills): nothing, an opt-in reviewer, or the reviewer kept only
there? Two consumers found in t01 must be answered too: the inspect report
review is the only verification an `inspect` run has, and review is forced
when a candidate edits test or verification inputs, the guard against a
worker weakening the tests.

## Resolution
