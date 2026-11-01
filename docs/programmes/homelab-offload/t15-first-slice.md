+++
type = "grilling"
mode = "hitl"
status = "open"
blocked_by = ["t13-laptop-load-baseline", "t22-stamp-from-remote-run", "t23-gate-platform-inventory", "t25-pod-signing-key"]
repos = ["relais"]
+++
# Which slice ships first and alone

## Question

The candidate is heavy checks first (no model credentials, no dispatch amendment), decided against the laptop-load baseline.

## Notes

The heavy-checks slice was planned and reviewed by eight seats and Codex. It
went stale before it was approved, when relais#370 moved verification into
`amont run --stamp`. The record is
`docs/plans/2026-11-01-first-slice-heavy-checks.md` (status `abandoned`, full
reviews in its sidecar).

Still valid, whoever runs the gate:
- **Runner pod on wk-3:** one replica; 10Gi memory with request equal to
  limit; CPU 2/8; `relais-batch`; a local-path cache with a 20% free-space
  floor and a 150Gi prune.
- **Downloads:** a crates and rustup nginx caching proxy in `packages`
  (bounded, `dl` rewritten); corepack and npm through verdaccio.
- **Egress:** the t09 floor.
- **Kubeconfig:** a ServiceAccount, with the token in the Keychain behind an
  ExecCredential wrapper and `interactiveMode: Never`.
- **Lease:** held per check, fenced by an epoch.
- **In-pod helper:** stdlib Python. It reports exit, signal or OOM (via
  `memory.events`; a group OOM kill takes the helper too) behind a
  relais-made nonce trailer. A missing trailer means lost, never red. It
  self-kills at a deadline and checks start time against pgid reuse.
- **Accounting:** one cluster token per check, whatever the weight;
  `PROTOCOL_VERSION` bumped for a new resource class.
- **Rollback:** ordered, because the config structs deny unknown fields.

## Update 2026-11-01 (t21)

The slice is now **amont running `platform=any` gates** (lint, types, cycle
checks) on the wk-3 runner pod, returning signed attestations. It is no
longer relais heavy checks. relais `cargo-test` stays local.
