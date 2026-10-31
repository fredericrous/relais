+++
type = "grilling"
mode = "hitl"
status = "open"
blocked_by = ["t10-pod-denial-signals"]
repos = ["relais", "homelab"]
+++
# What the automatic loop may never widen

## Question

The person chose a fully automatic sandbox loop; this ticket fixes what it can never change. Candidates: the credential deny list (the old `floor.rs`), the egress ceiling, no privilege escalation, and the restricted PodSecurity level.
