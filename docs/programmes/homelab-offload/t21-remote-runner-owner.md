+++
type = "grilling"
mode = "hitl"
status = "closed"
blocked_by = ["t22-stamp-from-remote-run"]
repos = ["amont", "relais"]
+++
# Which tool runs a gate on the cluster

## Question

relais now runs every check as `amont run --stamp <gate>` (relais#370).
decisions ADR-0051 gives amont the gates, the stamps and the host slots.

Options:
- amont's `run` gains a cluster executor (with its lease), and relais calls
  `amont run` unchanged. This would also offload amont's own pre-commit and
  pre-push test runs.
- relais keeps an executor of its own, the reviewed but superseded design.

The architect seat decides, as the person asked for t18.

## Resolution

**amont runs gates on the cluster.** Decided by the architect seat on
2026-11-01, as the person asked ("let the architect decide").

- **The shape:**
  - `amont run` gains a cluster executor. It runs only when the repo opts in
    (`set executor cluster`, off by default) **and** the gate line declares
    `platform=any`.
  - relais keeps calling `amont run --stamp <gate>`, unchanged.
  - amont is not installed in the pod: the pod runs the gate command
    directly, as CI does.
  - The result comes back as a signed attestation, verified with
    `git_attest::verify` and reported as `attested`.
- **Why:**
  - A pod never stamps (amont `adr/0015-a-runner-earns-the-stamp.md:67`).
    The only verdict that counts is an attestation, and signing and
    attestation are amont's and git-attest's (decisions ADR-0051).
  - relais only reads `amont stamps --json`
    (`crates/relais/src/verify/stamps.rs:4-19,145`).
  - ADR-0049's std-only rule covers the `amont-slots` crate, not amont as a
    whole. amont already spawns `git` and `ssh-keygen`, so calling
    `kubectl --kubeconfig` through std `Command` adds no crate.
- **What it buys:** only `platform=any` gates move, meaning lint, types and
  cycle checks (t23). That is a real win for duro, trade-agents and relais's
  cycle scripts. It is **not** a win for relais or amont `cargo-test`, which
  stay on macOS: the 36-minute pre-push `cargo-test` hold seen on
  2026-11-01 is not offloadable.
- **Failure order:**
  1. Try the Lease with a short wait.
  2. If it is not free or the cluster is off, run locally under a host
     slot.
  3. If the host is busy too, ADR-0013 applies: `onBusy defer` means
     Deferred, no stamp, and CI runs the gate.
  4. If the cluster is lost mid-run (renew fails, trailer missing, OOM),
     re-run locally. Never red. Under relais, base and candidate both
     re-run.
- **Who owns what:**
  - **amont:** the executor; a new std-only crate (e.g. `amont-cluster`)
    for the Lease protocol t18 pinned, published like `amont-slots`; the
    in-pod helper's wire contract.
  - **relais:** drops the superseded `Executor` seam and its lease code. It
    must not charge a host slot around a stamp command amont places on the
    cluster. It keeps a `Cluster` cap group only for worker pods (t02),
    taken by linking `amont-cluster`.
  - **homelab:** the runner pod, image, PVC, egress rules, crates proxy and
    Role, plus the t25 signer if that option wins.
- **Records to write in the build:**
  - a new amont ADR, "a gate may run on the cluster";
  - amend amont ADR-0015 (the attestation carve-out) and ADR-0014 (a
    cluster lease stands in for a host slot);
  - amend decisions ADR-0051's `harness.tool-boundaries` ("and the cluster
    leases");
  - the decisions ADR for `platform: any`, amending ADR-0041 (t20).
- **Open, for the build plan:**
  - Does `passed` at `dispatch.rs:1340-1361` include remote-derived gates?
    If so, exclude them, or attest them as `x86_64-linux`.
  - The signing key (t25).
  - Remote gate identity (the command plus the image digest).
  - The Lease wait inside a pre-commit hook.
  - Whether CI's `platform: any` reads the same declaration.
