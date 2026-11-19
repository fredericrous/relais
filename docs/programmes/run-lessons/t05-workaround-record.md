+++
type = "grilling"
mode = "hitl"
status = "open"
blocked_by = ["t04-invalidation"]
+++
# What the session records when it found a workaround

## Question

What does `relais lesson add` (MCP tool and CLI) take, and when does the
hook nudge the session to call it? Candidate fields: the symptom (command,
error text), the fix, the files or globs it is about, the evidence (a run,
a commit, a log), and who recorded it. Candidate nudges: a run that passed
only after a repair, a session command that failed then passed with a
changed flag, an orchestrator edit that unblocked a run.
