+++
type = "grilling"
mode = "hitl"
status = "closed"
blocked_by = ["t06-litellm-budgets"]
repos = ["relais", "homelab"]
+++
# Which credential a pod uses, and where the token budget is enforced

## Question

A subscription OAuth token mounted in the pod, or an API key routed through litellm with a per-key budget? The answer is what makes the person's token/cost budget enforceable, and it must keep the credential out of the worker's readable files (the old sandbox floor, `git show 07ed5c5f^:crates/relais/src/sandbox/floor.rs`).

## Resolution

Decided as staff engineer, as delegated on 2026-10-31. **Direction:** a
long-lived Claude Code OAuth token (`claude setup-token`) stored in Vault and
mounted only into worker pods, never into check pods. This is the one carve-out from the t09 floor's "no secrets mounted".

- **Spend:** capped by relais's own ledger and cap, not by litellm. t06 found
  no per-key budgets, and no `/v1/messages` route.
- **Gate:** the direction holds only after the end-to-end test in
  t24-litellm-live-test, which blocks t02.
