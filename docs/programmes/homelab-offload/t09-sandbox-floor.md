+++
type = "grilling"
mode = "hitl"
status = "closed"
blocked_by = ["t10-pod-denial-signals"]
repos = ["relais", "homelab"]
+++
# What the automatic loop may never widen

## Question

The person chose a fully automatic sandbox loop; this ticket fixes what it can never change. Candidates: the credential deny list (the old `floor.rs`), the egress ceiling, no privilege escalation, and the restricted PodSecurity level.

## Resolution

Decided as staff engineer, as delegated on 2026-10-31. Every relais pod gets
this floor, and the automatic loop (t11) may only tighten it:

- PSA `restricted`;
- runs as non-root;
- `allowPrivilegeEscalation: false`;
- all capabilities dropped;
- RuntimeDefault seccomp;
- `automountServiceAccountToken: false`;
- no secrets mounted, with one carve-out: a worker pod (t02) gets its own Claude Code token (t03), mounted read-only and never into a check pod;
- no hostPath;
- egress: `world` denied, plus egressDeny to the `vault` namespace and to the
  `kube-apiserver` (the diag-sandbox pattern).

The real floor, recorded as such: the clusterwide `dns-visibility` policy
allows everything (`networking/cilium-dns-visibility.yaml`), and an allow rule
cannot narrow it. Other in-cluster services stay reachable.
