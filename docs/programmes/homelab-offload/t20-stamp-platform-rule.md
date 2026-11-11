+++
type = "grilling"
mode = "hitl"
status = "closed"
blocked_by = []
repos = ["amont", "decisions"]
+++
# May a stamp earned in a Linux pod satisfy the laptop's gate

## Question

relais v0.44+ runs every check as `amont run --stamp <gate>`, and amont's
stamps are keyed by platform (`x86_64-macos`). decisions ADR-0041 makes macOS
primary. Can a check run in a Linux pod on wk-3 satisfy the laptop's commit
and push gate for that tree?

## Resolution

**Per gate.** The person chose this on 2026-11-01.

- Each gate in `amont.conf` declares whether it is platform-independent.
- A stamp earned on Linux counts on the laptop only for those gates.
- macOS-sensitive gates stay local. The example given here, relais's
  `cargo-test` under ADR-0041, was wrong: it passes on Linux in CI (see
  t23's Correction, 2026-11-11). The per-gate rule stands; which gates
  qualify is t23's reopened question.

The rule is written as a decisions ADR amending ADR-0041 when t22 is built.

## Notes

Corrected by t22: amont stamps carry no platform; only attestations do. A pod
never stamps (amont ADR-0015). "Counts on the laptop" here means the laptop
honours the pod's signed attestation for a gate declared platform-independent.
