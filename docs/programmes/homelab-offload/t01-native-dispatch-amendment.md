+++
type = "grilling"
mode = "hitl"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# Is "every dispatch native" amended for remote runs

## Question

Since #173 (`docs/plans/2026-10-06-all-native-mod.md`) every dispatch is a Claude Code subagent spawned by the relais plugin inside the person's interactive session; `claude -p` and `ClaudeBackend::launch` were deleted on purpose. A worker in a pod needs a launcher outside that session. Is the all-native choice amended for remote runs, and how is that recorded (a new ADR, or an amendment)? Which model does a pod worker get, given ADR-0001 (`routing.session-model`: relais "never re-routes its own workers")?
