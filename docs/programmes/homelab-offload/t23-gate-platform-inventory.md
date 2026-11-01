+++
type = "research"
mode = "afk"
status = "open"
blocked_by = []
repos = ["amont", "relais", "homelab"]
+++
# Which gates in the fleet are platform-independent, and which repos are Linux-primary

## Question

For each repository that declares amont gates:

- Which gates are platform-independent? For relais: fmt, clippy and the cycle
  checks are candidates; `cargo-test` is likely macOS-sensitive under
  ADR-0041.
- Which repositories are Linux-primary? Candidates: homelab, services,
  trade-agents.
- What share of laptop check-seconds do the platform-independent gates
  account for, from the relais#367 per-check timings?
