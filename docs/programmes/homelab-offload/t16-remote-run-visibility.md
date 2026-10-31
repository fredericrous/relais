+++
type = "grilling"
mode = "hitl"
status = "open"
blocked_by = ["t02-remote-launcher", "t04-worktree-transport"]
repos = ["relais"]
+++
# How a pod's events, logs and progress reach status and the progress view on the laptop

## Question

Today `quiet::watch` and `native status` read a local run directory (`crates/relais/src/quiet.rs:445`). Options: mirror `events.jsonl` back, stream through the coordinator, or have status ask the pod. Preserve the decisions:ADR-0045 holdout boundary: person-readable evidence is distinct from the worker feedback it permits (criterion statement, failing test ids, pass/fail).
