+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["amont"]
+++
# A greengate land collects its own cluster gate result

## Question

Filed from the amont inbox (2026-11-25, session aa9f7e4c). During a greengate 5.0.0 land in greenrack, `pre-push-registry` (executor `cluster`) was submitted to the cluster on every land, `cluster collect` found 0 pending, and land refused with `evidence-missing` each time. Running `greengate run --stamp pre-push-registry` in the cluster, which stamped it, got the land through. Should land wait for its own cluster result, or collect it, before it judges the evidence?

## Look at

- amont `crates/` land pre-stage and `cluster collect` (the `executor = "cluster"` path).
- amont ADR-0022's `hooks.gates-before-connection` (land's pre-stage).

## Answer looks like

A failing test of a land that owes a cluster gate, the fix that makes land collect or wait for its own submission, and the greenrack land reproduced green.
