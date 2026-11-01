# Relais runs on the homelab

## Destination

relais places each run, a whole worker run or heavy checks, on the laptop or
the homelab cluster. Cluster runs stay under CPU, RAM, slot and token caps,
and their sandbox adapts automatically above a fixed floor. The laptop's load
drops by the target the laptop-load baseline sets, and this runs in
production.

## Notes

- Repositories: relais (owner), homelab, amont. Every session reads the
  `worktree-task` skill; homelab work also reads `forgejo-recipes`.
- The person's answers (2026-10-31): both whole runs and heavy checks move,
  chosen per task; usage control is cluster CPU/RAM caps, concurrency slots,
  a token/cost budget and a laptop-load threshold; the sandbox improves
  itself fully automatically; the end point is production.
- Workers today: every dispatch is a native Claude Code subagent inside the
  person's session (`adapter/native.rs`, `admission/native.rs`); `claude -p`
  was deleted in #173. `Backend`/`Harness` (`backend.rs:487`) is the only
  launcher seam.
- The old sandbox (`git show 07ed5c5f^:crates/relais/src/sandbox/`) was
  Claude Code's own `sandbox` settings: credential floor, probe and
  verification store, dispatch gate, denial report. Its improvement loop was
  manual (#164, #170).
- Decisions that bear on this: ADR-0001 `routing.session-model`, ADR-0009
  `workspace.local-reuse`, ADR-0010 `execution.resource-budgets` (unchanged by
  this programme), ADR-0003 `learning.automatic-promotion` (the model for the
  sandbox loop, which needs its own record), decisions:ADR-0045 (holdouts the
  writer cannot reach), decisions:ADR-0051 `harness.tool-boundaries` (amont
  owns host slots).
- Cluster facts: wk-4 down, wk-3 reserved for AI work (relais may use it under quota, t19), Forgejo `ci` pool cut
  to 2, CephFS slow on small files (use node-local ephemeral storage), no
  gVisor or Kata, Tetragon detect-only, Cilium default-deny egress templates
  in `apps/kubeopencode/`, mirrors for npm, Go and images but none for cargo.
- Since relais#370 (2026-11-01), every check runs as `amont run --stamp <gate>`, and amont owns gates and stamps (ADR-0051). Who runs a gate remotely is t21, and the Destination's "relais places each run" is restated once t21 decides.
- Branch convention: the programme skill names ticket branches
  `prog/<slug>/<id>`, but amont's push gate rejects a `prog/` prefix
  (`amont-runtime/src/vocabulary.rs:81`). Until that is reconciled, ticket
  sessions use `docs/homelab-offload-<id>`.
- Plan that charted this map: `docs/plans/2026-10-31-relais-runs-on-the-homelab.md`.

## Decisions so far

- [How amont earns and signs a stamp for a tree a pod verified](t22-stamp-from-remote-run.md): it doesn't stamp (ADR-0015); the pod's signed attestation counts on the laptop only for gates declared `platform=any`; it amends ADR-0015 and adds a decisions ADR beside ADR-0041.
- [Which gates in the fleet are platform-independent, and which repos are Linux-primary](t23-gate-platform-inventory.md): lint, types and cycle checks (JS/TS, Python, relais scripts) are independent; relais and amont `cargo-test` stay macOS; the share waits for t13 and gate-named timings.
- [Is "every dispatch native" amended for remote runs](t01-native-dispatch-amendment.md): only when worker pods land (t02), by a relais ADR; a pod worker is relais's own worker.
- [Which credential a pod uses, and where the token budget is enforced](t03-credentials-billing.md): a long-lived `claude setup-token` in Vault, worker pods only; spend capped by relais's ledger; gated on t24.
- [What the automatic loop may never widen](t09-sandbox-floor.md): PSA restricted, non-root, no token automount, no secrets or hostPath, world, vault and the apiserver denied; the loop only tightens.
- [May relais pods run on wk-3, or does offload wait for wk-4](t19-wk3-for-relais.md): yes, under quota and `relais-batch` priority (the person, 2026-10-31).
- [May a stamp earned in a Linux pod satisfy the laptop's gate](t20-stamp-platform-rule.md): per gate; platform-independent gates only (the person, 2026-11-01).
- [How relais on the laptop reaches the cluster API off-LAN](t12-laptop-cluster-path.md): the VIP 192.168.1.65:6443 (off-LAN via the Headscale 192.168.1.0/24 route is unproven: the macOS Tailscale build installs no route; live test open for t02/t17), as SA `relais/relais-coordinator` with a short bound token in Keychain via an exec plugin; a namespaced Role.
- [Which denial signals a pod produces, and how they map onto the old denial report](t10-pod-denial-signals.md): Hubble DROPPED flows per run-labelled pod under default-deny egress are the one verified (network) signal; Tetragon is suspected context; no seccomp audit or gVisor; no bwrap in the pod.
- [What capacity relais gets on the cluster without starving the services or Forgejo runners](t05-cluster-capacity.md): almost none outside wk-3 until wk-4 returns; the binding limit is OSD disk I/O; a `relais` namespace with a 4 CPU / 8Gi / 8-pod quota, `relais-batch` priority -500 non-preempting, emptyDir scratch.
- [How a worktree gets into a pod and a patch comes back](t04-worktree-transport.md): a git bundle of an immutable snapshot commit plus a manifest; the result is rebound by sha and digest; Forgejo push optional, rsync rejected.
- [How remote capacity becomes a slot kind](t07-slot-model.md): *(superseded pending t21: verification now runs through `amont run --stamp`; t14's result contract feeds t22)* a `ClusterJob` class in a `Cluster` cap group (`CapGroup::Remote` renamed `ModelApi`), counted by one Kubernetes Lease per slot, about 60 s TTL renewed by the coordinator, reaped on expiry; amont-slots stays host-only.
- [How a heavy check runs in a pod and returns its outcome and logs](t14-remote-heavy-checks.md): *(superseded pending t21: verification now runs through `amont run --stamp`; t14's result contract feeds t22)* a remote executor replaces only spawn and wait; verdicts bound to tree, image digest and toolchain; same executor for base and candidate; holdouts stay on the host in v1.
- [Can litellm per-key budgets cap Claude spend, and does Claude Code run through it](t06-litellm-budgets.md): not today; dollar per-day caps per key only, per-run needs an in-cluster minter, and the route lacks `/v1/messages`; live test before t03.

## Not yet specified

- A Rust build cache for cold pods (sccache, a cargo mirror).

## Out of scope
