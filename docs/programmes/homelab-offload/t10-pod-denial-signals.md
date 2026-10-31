+++
type = "research"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais", "homelab"]
+++
# Which denial signals a pod produces, and how they map onto the old denial report

## Question

The old `denials.rs` (`git show 07ed5c5f^:crates/relais/src/sandbox/denials.rs`) read Claude Code's `<sandbox_violations>` and denial lines from transcripts. Candidate sources in a pod: Cilium Hubble drops, Tetragon events (detect-only today), seccomp/AppArmor, gVisor. Also check whether `bwrap` inside a pod is possible under restricted PSA (the old Linux path needed `bwrap` and `socat`).

## Resolution

Explore research with read-only `kubectl`, 2026-10-31. The relais inspect run
was blocked by low disk.

**Hubble is the one verified signal inside a pod, and it covers network only.**
bwrap does not go in the pod.

- **The old model** (`07ed5c5f^:crates/relais/src/sandbox/denials.rs`):
  - verified: only `deny network-outbound` lines inside `<sandbox_violations>`;
  - suspected: other lines that look like a refusal;
  - coverage: Complete, Incomplete or Unknown. An empty report counts as clean
    only when coverage is Complete.
- **Signals on this cluster:**

  | Signal | Present? | Class | Coverage |
  |---|---|---|---|
  | Hubble DROPPED flows | yes | verified (network) | Complete only with a default-deny policy and a query window that covers the run |
  | Tetragon | yes, detect-only (Post only) | suspected context | Incomplete |
  | seccomp/AppArmor audit | no | — | Unknown |
  | gVisor | no | — | n/a |

  - Hubble: enabled with relay, policy correlation and drop metrics
    (`infrastructure/homelab/cilium-values.yaml:120-138`). It is read per pod
    with `hubble observe --pod <ns>/<pod> --verdict DROPPED`, through relay
    with TLS. Latency is seconds; retention is an in-memory ring.
  - Tetragon: chart 1.7.0, 18 escape/ptrace/ebpf policies, kprobe/LSM events
    exported to Loki. It does not record "write refused".
  - seccomp/AppArmor: no profiles, and Talos has no AppArmor.
  - gVisor: only the `nvidia` RuntimeClass exists.
- **bwrap.** `user.max_user_namespaces` is 11255
  (`infrastructure/homelab/patch/sysctls-patch.yaml:37`). Under `restricted`
  PSA with RuntimeDefault seccomp, though, unshare and CLONE_NEWUSER are
  very likely blocked. Making bwrap work would need a node-installed
  Localhost seccomp profile, Unconfined seccomp (not allowed by restricted),
  or `hostUsers: false` (unverified). The pod already isolates, so Claude
  Code's own sandbox inside it adds little: egress is enforced by Cilium.
- **Shape for t09 and t11:**
  - each worker pod gets a `relais/run-id` label and a default-deny-egress
    CiliumNetworkPolicy, using `kubeopencode-agent-egress` and
    `diag-sandbox-egress-lockdown` as templates;
  - at run end, Hubble drops for that pod become the verified denial list;
  - Tetragon events are suspected context;
  - the `Coverage` enum stays, and reports Unknown when relay is unreachable
    or the policy is absent.
- **Open:**
  - whether the laptop can reach Hubble relay (certs, network; see t12);
  - ring retention for an exited pod;
  - how a drop looks to the worker (a timeout, not a 403);
  - the worker namespace's PSA level;
  - read access to Tetragon or Loki.
