+++
type = "grilling"
mode = "hitl"
status = "open"
blocked_by = ["t22-stamp-from-remote-run"]
repos = ["amont", "homelab"]
+++
# Where the pod's attestation signing key lives

## Question

A gate run in a pod is trusted on the laptop only through a signed attestation
(t22). Which key signs it, and where does that key live, so the gate's own
command (repository code) cannot read it? Options:

- a key in the pod, mounted only into the signing step;
- a separate signer service in the cluster that signs after it verifies the
  run's evidence (the CI `verify`/`sign` split);
- signing on the laptop after it re-verifies tree, digest and log.

Each one changes `allowed_signers` and the t09 floor ("no secrets mounted").
