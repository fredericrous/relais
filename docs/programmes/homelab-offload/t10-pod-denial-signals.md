+++
type = "research"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais", "homelab"]
+++
# Which denial signals a pod produces, and how they map onto the old denial report

## Question

The old `denials.rs` (`git show 07ed5c5f^:crates/relais/src/sandbox/denials.rs`) read Claude Code's `<sandbox_violations>` and denial lines from transcripts. Candidate sources in a pod: Cilium Hubble drops, Tetragon events (detect-only today), seccomp/AppArmor, gVisor. Also check whether `bwrap` inside a pod is possible under restricted PSA (the old Linux path needed `bwrap` and `socat`).
