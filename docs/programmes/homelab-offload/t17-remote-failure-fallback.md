+++
type = "grilling"
mode = "hitl"
status = "open"
blocked_by = ["t02-remote-launcher", "t04-worktree-transport", "t08-placement-policy", "t12-laptop-cluster-path", "t21-remote-runner-owner", "t26-execution-service"]
repos = ["relais"]
+++
# What happens and what the person sees when a remote run cannot start or dies

## Question

Cases: the cluster is unreachable, a quota is hit or the budget is spent; the pod is evicted or OOM-killed; a patch is lost; the laptop sleeps or goes offline; SIGINT/SIGTERM or the session ends and the pod must be cancelled; an orphaned pod is reaped. Map the pod's exit status to a relais outcome and keep a local fallback safe to retry. Every message says what happened and why, gives the exact next command, and fits 80 columns.

## Update 2026-11-11

The cases for **gates** are already decided: t21's "Failure order" (Lease
wait, then local run, then defer to CI, never red) and t26 item 7 (what a
closed lid does per consumer). Only **worker pods and Coder sessions** are
open here. Treat the gate cases as closed by reference and keep this ticket
for workers. The "laptop sleeps" case also overlaps the session handoff
question in the agents-many-nodes programme (homelab repo, ticket an-t03), where
a session continues on another node instead of falling back.

## Look at

- t21 "Failure order" and t26 item 7.
- t12 (the open 6443 off-LAN route).
- `crates/relais/src/coordinator/mod.rs` (the `mod_gone` and orphan paths).

## Answer looks like

One row per failure case for a remote worker: what relais records as the
outcome, what the person sees (the message text), the next command, and
whether the local retry is safe.
