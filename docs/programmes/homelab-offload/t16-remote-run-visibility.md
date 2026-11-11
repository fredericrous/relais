+++
type = "grilling"
mode = "hitl"
status = "open"
blocked_by = ["t02-remote-launcher", "t04-worktree-transport", "t26-execution-service"]
repos = ["relais"]
+++
# How a pod's events, logs and progress reach status and the progress view on the laptop

## Question

Today `quiet::watch` and `native status` read a local run directory (`crates/relais/src/quiet.rs:445`). Options: mirror `events.jsonl` back, stream through the coordinator, or have status ask the pod. Preserve the decisions:ADR-0045 holdout boundary: person-readable evidence is distinct from the worker feedback it permits (criterion statement, failing test ids, pass/fail).

## Update 2026-11-11

For detached **gate** runs the answer largely exists: t26 item 4 reads the
Argo node status and pod logs, and greengate's `cluster collect` turns a
finished Workflow into a cluster token (`detached/collect.rs`). That is not
built as t26 describes (inline Workflow, 48 h TTL, no artifacts; see t26's
build state). The open part is **worker pods and Coder sessions**, which
produce run events, not gate verdicts. Everything in relais is host-local
(ledger, run directories, socket), so remote events need a transport the
coordinator does not have.

## Look at

- t26 items 4 and 5, and its build state.
- `crates/relais/src/quiet.rs:445` and `crates/relais/src/ipc.rs` (the
  local-only endpoint).
- The agents-many-nodes ticket an-t01 (homelab repo), which lists which state is
  host-local.

## Answer looks like

The mechanism for a remote worker's events and logs to reach `status` and the
progress view, the lag the person sees, what shows when the pod is gone, and
how the ADR-0045 boundary is kept.
