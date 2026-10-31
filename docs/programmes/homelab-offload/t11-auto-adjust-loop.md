+++
type = "grilling"
mode = "hitl"
status = "open"
blocked_by = ["t02-remote-launcher", "t09-sandbox-floor", "t10-pod-denial-signals"]
repos = ["relais", "homelab"]
+++
# How the sandbox policy adapts from denials by itself

## Question

The old loop was manual (#164, #170: people edited rules after reading denials). Settle the mechanism, the rollback and the evidence threshold. The Resolution is a new decision record modelled on ADR-0003 (`learning.automatic-promotion`): a granted envelope, automatic rollback, anti-flap, and it never edits the floor that t09 sets.
