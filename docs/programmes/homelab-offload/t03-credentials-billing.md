+++
type = "grilling"
mode = "hitl"
status = "open"
blocked_by = ["t06-litellm-budgets"]
repos = ["relais", "homelab"]
+++
# Which credential a pod uses, and where the token budget is enforced

## Question

A subscription OAuth token mounted in the pod, or an API key routed through litellm with a per-key budget? The answer is what makes the person's token/cost budget enforceable, and it must keep the credential out of the worker's readable files (the old sandbox floor, `git show 07ed5c5f^:crates/relais/src/sandbox/floor.rs`).
