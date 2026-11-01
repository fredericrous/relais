+++
type = "research"
mode = "afk"
status = "open"
blocked_by = ["t20-stamp-platform-rule"]
repos = ["amont", "decisions"]
+++
# How amont earns and signs a stamp for a tree a pod verified

## Question

Under the per-gate rule (t20), how does a gate run in a Linux pod earn a stamp
the laptop trusts?

- The signing key stays on the laptop. What does the laptop check before it
  signs? Candidates: the tree sha, the image digest and the log sha256, per
  t14's result contract.
- What does the stamp record? The executor, the platform and the image
  digest.
- How does `amont stamps` answer per gate and platform?
- What does pre-push accept?

Read amont ADR-0010, ADR-0013, ADR-0015 and the `run --stamp` code, and
decisions ADR-0041.
