# Relais runs on the homelab

## Destination

relais places worker runs, and amont places gates (only those declared
`platform=any`), on the laptop or the homelab cluster. Cluster runs stay under CPU, RAM, slot and token caps,
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
- The person, 2026-11-07: worker pods reach Claude, Codex and Qwen through a CLIProxyAPI and its management center (router-for-me/CLIProxyAPI, Cli-Proxy-API-Management-Center) run on the homelab, as the Mac does today on 127.0.0.1:8317; t03 is amended.
- Plan that charted this map: `docs/plans/2026-10-31-relais-runs-on-the-homelab.md`.
- **2026-11-11 review.** A review of greengate (formerly amont) and relais
  corrected this map: `cargo-test` passes on Linux in CI for both repositories
  (t23); the detached lane takes only pre-push gates and was never enabled
  (t15, t26); the signing decision is reopened (t25); the relais coordinator is
  portable to Linux but a run is bound to a live Claude Code session and to one
  host (t02). Names: greengate 4.0 (a local branch, not yet on origin) renames
  `amont.cluster.*`, `AMONT_*` and the `amont-gates` namespace; tickets here
  keep the old names until it ships.
- **Related programme:** agents-many-nodes (homelab repository,
  `docs/programmes/agents-many-nodes/`) takes the longer goal of sessions and
  runs on several nodes, including a Coder workspace. Its tickets decide where
  the coordinator lives, so t02, t08, t11, t16 and t17 here should be read
  against it. Its tickets are prefixed `an-` (an-t01 to an-t07), so a bare
  `tNN` in this map always means a ticket here. It depends on t24 (the live
  proxy test) and t30 (the Mac reaching the proxy off-LAN), which stay open
  here. The pod signing key (t25, closed) overlaps its an-t07, which owns the
  node identity and signer design, so keep one key model across both.

## Decisions so far

- [Does the Mac reach the homelab proxy from anywhere, before the cutover](t30-mac-reaches-homelab-proxy.md): yes, off-LAN over Headscale on cellular, nothing more needed (the person tested it, 2026-11-11); the LAN leg, latency and the cluster-down case were not reported; t12's apiserver VIP route stays unproven.
- [How the CLIProxyAPI auth volume is backed up and restored](t29-cliproxy-auth-backup.md): the existing Velero daily/weekly/monthly schedules with `cliproxy` added; a restore assumes fresh sign-ins because refresh tokens rotate; off-site data copy and a drill go to the deploy plan.
- [Which proxy owns each OAuth account once the homelab runs one](t28-oauth-account-owner.md): the homelab proxy holds every account and the Mac points at it, its own proxy stopped (the person, 2026-11-07); cutover never runs both; the off-LAN path (t30) is confirmed by the person, 2026-11-11.
- [How CLIProxyAPI and its management center run on the homelab](t27-cliproxy-on-homelab.md): upstream `eceasy/cli-proxy-api` by digest, one replica on an RWO PVC (refresh rewrites the OAuth files), per-consumer access keys from Vault, the bundled panel with management not routed, `cliproxy` namespace egress to Anthropic/OpenAI/the LiteLLM gateway; deployed since (the proxy runs on the homelab).
- [How work handed to the cluster outlives the laptop](t26-execution-service.md): two lanes; commit-blocking gates keep the kubectl executor, everything else becomes an Argo Workflow (already installed) fed by a Forgejo snapshot (real parent) or a pull-mirror; Argo holds slots, cancel and 7-day results; the laptop stays the only signer and collects on wake (t25 reopens with worker pods); next: a build plan.
- [Where the pod's attestation signing key lives](t25-pod-signing-key.md): *(reopened 2026-11-11, now open)* decided as nowhere new, the laptop signs from Kubernetes-recorded facts, one Job per gate, Job-create fenced; a code review found it built in part and failing once a second node signs.
- [Which slice ships first and alone](t15-first-slice.md): amont runs `platform=any` gates (lint, types, cycles) on wk-3, one Job per gate, laptop-signed; enabled after the t13 window. *(2026-11-11 review: never enabled, and as planned it cannot run: the detached lane takes only pre-push gates and relais's cycle gates are pre-commit. See t15.)*
- [Which tool runs a gate on the cluster](t21-remote-runner-owner.md): amont, behind a `platform=any` gate and a per-repo opt-in; the pod returns a signed attestation; relais keeps calling `amont run --stamp` (architect seat).
- [Who owns the cluster lease protocol](t18-cluster-lease-owner.md): amont, in a new std-only crate relais links for worker pods.
- [How amont earns and signs a stamp for a tree a pod verified](t22-stamp-from-remote-run.md): it doesn't stamp (ADR-0015); the pod's signed attestation counts on the laptop only for gates declared `platform=any`; it amends ADR-0015 and adds a decisions ADR beside ADR-0041.
- [Is "every dispatch native" amended for remote runs](t01-native-dispatch-amendment.md): only when worker pods land (t02), by a relais ADR; a pod worker is relais's own worker.
- [Which credential a pod uses, and where the token budget is enforced](t03-credentials-billing.md): *(amended 2026-11-07 by the person)* worker pods call a homelab CLIProxyAPI with a proxy access key; the OAuth tokens stay in the proxy; spend capped by relais's ledger (per ledger, so per node); t27 and t28 are closed and t24 is the remaining live test.
- [What the automatic loop may never widen](t09-sandbox-floor.md): PSA restricted, non-root, no token automount, no secrets or hostPath, world, vault and the apiserver denied; the loop only tightens.
- [May relais pods run on wk-3, or does offload wait for wk-4](t19-wk3-for-relais.md): yes, under quota and `relais-batch` priority (the person, 2026-10-31).
- [May a stamp earned in a Linux pod satisfy the laptop's gate](t20-stamp-platform-rule.md): per gate; platform-independent gates only (the person, 2026-11-01).
- [How relais on the laptop reaches the cluster API off-LAN](t12-laptop-cluster-path.md): the VIP 192.168.1.65:6443 (off-LAN via the Headscale 192.168.1.0/24 route is unproven: the macOS Tailscale build installs no route; live test open for t02/t17), as SA `relais/relais-coordinator` with a short bound token in Keychain via an exec plugin; a namespaced Role.
- [Which denial signals a pod produces, and how they map onto the old denial report](t10-pod-denial-signals.md): Hubble DROPPED flows per run-labelled pod under default-deny egress are the one verified (network) signal; Tetragon is suspected context; no seccomp audit or gVisor; no bwrap in the pod.
- [What capacity relais gets on the cluster without starving the services or Forgejo runners](t05-cluster-capacity.md): almost none outside wk-3 until wk-4 returns; the binding limit is OSD disk I/O; a `relais` namespace with a 4 CPU / 8Gi / 8-pod quota, `relais-batch` priority -500 non-preempting, emptyDir scratch.
- [How a worktree gets into a pod and a patch comes back](t04-worktree-transport.md): a git bundle of an immutable snapshot commit plus a manifest; the result is rebound by sha and digest; Forgejo push optional, rsync rejected.
- [How remote capacity becomes a slot kind](t07-slot-model.md): *(superseded by t21, now closed: verification now runs through `amont run --stamp`; t14's result contract feeds t22)* a `ClusterJob` class in a `Cluster` cap group (`CapGroup::Remote` renamed `ModelApi`), counted by one Kubernetes Lease per slot, about 60 s TTL renewed by the coordinator, reaped on expiry; amont-slots stays host-only.
- [How a heavy check runs in a pod and returns its outcome and logs](t14-remote-heavy-checks.md): *(superseded by t21, now closed: verification now runs through `amont run --stamp`; t14's result contract feeds t22)* a remote executor replaces only spawn and wait; verdicts bound to tree, image digest and toolchain; same executor for base and candidate; holdouts stay on the host in v1.
- [Can litellm per-key budgets cap Claude spend, and does Claude Code run through it](t06-litellm-budgets.md): not today; dollar per-day caps per key only, per-run needs an in-cluster minter, and the route lacks `/v1/messages`; live test before t03.

## Not yet specified

Open tickets, as of the 2026-11-11 review (`relais programme frontier` lists
the ones with no open blocker):

- [t13](t13-laptop-load-baseline.md): the window ended 2026-11-08; the closing
  table and target are unwritten.
- [t23](t23-gate-platform-inventory.md): reopened; which Rust gates may be
  `platform=any`, and the policy conflict over test suites.
- [t24](t24-litellm-live-test.md): only cluster reachability to the proxy is
  confirmed.
- [t25](t25-pod-signing-key.md): reopened; a second signing node.
- [t31](t31-rust-build-cache.md): the Rust build cache, a desk answer.
- [t02](t02-remote-launcher.md), [t08](t08-placement-policy.md),
  [t11](t11-auto-adjust-loop.md), [t16](t16-remote-run-visibility.md),
  [t17](t17-remote-failure-fallback.md): worker pods and placement. These
  assume the laptop is the controller. The programme agents-many-nodes (homelab
  repository) decides where sessions and the coordinator live; revisit these
  against it before working them.

## Out of scope
