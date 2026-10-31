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
- Cluster facts: wk-4 down, wk-3 reserved for AI work, Forgejo `ci` pool cut
  to 2, CephFS slow on small files (use node-local ephemeral storage), no
  gVisor or Kata, Tetragon detect-only, Cilium default-deny egress templates
  in `apps/kubeopencode/`, mirrors for npm, Go and images but none for cargo.
- Branch convention: the programme skill names ticket branches
  `prog/<slug>/<id>`, but amont's push gate rejects a `prog/` prefix
  (`amont-runtime/src/vocabulary.rs:81`). Until that is reconciled, ticket
  sessions use `docs/homelab-offload-<id>`.
- Plan that charted this map: `docs/plans/2026-10-31-relais-runs-on-the-homelab.md`.

## Decisions so far

- [How relais on the laptop reaches the cluster API off-LAN](t12-laptop-cluster-path.md): the VIP 192.168.1.65:6443, off-LAN over the Headscale 192.168.1.0/24 route, as SA `relais/relais-coordinator` with a short bound token in Keychain via an exec plugin; a namespaced Role.
- [Which denial signals a pod produces, and how they map onto the old denial report](t10-pod-denial-signals.md): Hubble DROPPED flows per run-labelled pod under default-deny egress are the one verified (network) signal; Tetragon is suspected context; no seccomp audit or gVisor; no bwrap in the pod.
- [What capacity relais gets on the cluster without starving the services or Forgejo runners](t05-cluster-capacity.md): almost none outside wk-3 until wk-4 returns; the binding limit is OSD disk I/O; a `relais` namespace with a 4 CPU / 8Gi / 8-pod quota, `relais-batch` priority -500 non-preempting, emptyDir scratch.
- [How a worktree gets into a pod and a patch comes back](t04-worktree-transport.md): a git bundle of an immutable snapshot commit plus a manifest; the result is rebound by sha and digest; Forgejo push optional, rsync rejected.
- [How remote capacity becomes a slot kind](t07-slot-model.md): a `ClusterJob` class in a `Cluster` cap group (`CapGroup::Remote` renamed `ModelApi`), counted by one Kubernetes Lease per slot, about 60 s TTL renewed by the coordinator, reaped on expiry; amont-slots stays host-only.
- [How a heavy check runs in a pod and returns its outcome and logs](t14-remote-heavy-checks.md): a remote executor replaces only spawn and wait; verdicts bound to tree, image digest and toolchain; same executor for base and candidate; holdouts stay on the host in v1.
- [Can litellm per-key budgets cap Claude spend, and does Claude Code run through it](t06-litellm-budgets.md): not today; dollar per-day caps per key only, per-run needs an in-cluster minter, and the route lacks `/v1/messages`; live test before t03.

## Not yet specified

- A Rust build cache for cold pods (sccache, a cargo mirror).

## Out of scope
