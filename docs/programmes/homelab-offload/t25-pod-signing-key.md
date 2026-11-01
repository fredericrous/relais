+++
type = "grilling"
mode = "hitl"
status = "closed"
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

## Resolution

**No pod key and no cluster signer.** The laptop's amont signs, with the
`amont-attest` key it already has in `allowed_signers`. It signs only after
reading the facts Kubernetes itself recorded about the run.

Decided as staff engineer (the person delegated on 2026-10-31), after an
advisor review on 2026-11-01.

- **Why the laptop.** A cluster signer could trust only what the Kubernetes
  API records: a container's exit code, `status.imageID`, and the Job spec.
  The laptop reads exactly those facts through its scoped ServiceAccount, so
  both rest on the same trust root. The laptop adds no new key, Vault
  secret, service or `allowed_signers` entry. It is the CI precedent: the
  `sign` job signs on the platform-recorded job conclusion, never on a file
  the test job wrote (amont `docs/ci.md:304-330`).
  - A key inside the pod is rejected: repo code would read it.
  - A cluster signer is rejected: under the clusterwide `dns-visibility`
    allow-all, any signing endpoint is reachable from repo code, and it
    buys nothing over the laptop.
- **What this requires of the slice** (it supersedes parts of t15's notes):
  1. **One Job per gate.** The verdict is the gate container's own exit
     status, recorded by Kubernetes. An in-pod helper's trailer is
     forgeable, because the helper shares a uid and filesystem with the
     repo code. So no long-lived exec pod and no trailer as evidence.
  2. **Identity from sources the repo code cannot reach:**
     - the tree, from a trusted init container's termination message (it
       checks out the bundle and verifies `HEAD^{tree}` before any repo
       code runs), or rederived on the laptop from the bundle it sent;
     - the image, from `status.imageID`;
     - the command, from the Job spec the laptop created.
  3. **Job creation is fenced.** Only the coordinator's ServiceAccount may
     create Jobs in the runner namespace. Future worker pods (t02) live in
     another namespace with no Job-create, or they could mint a pass with
     `command: true`.
  4. **Signing scope.** The laptop signs `platform x86_64-linux` only for
     gates `amont.conf` declares `platform=any`. Such an attestation also
     lets CI's ubuntu legs skip that gate. That is wanted for
     `platform=any` gates (CI already accepts `platform: any`), and for no
     other gate.
- **What it does not claim.** A signature proves key isolation and a
  platform-recorded outcome. Like CI and local runs, it does not stop repo
  code from making its own check pass. The sandbox (t09) bounds what that
  code can reach; it does not judge the code.
- **For the build plan:**
  - `amont-attest-v2` has no image-digest or command field, so carrying
    them needs a format change in git-attest, a separate repo.
  - Does `passed` at amont `dispatch.rs:1340-1361` include remote-derived
    gates?
  - Gate identity for a remote run.
  - The Lease wait inside hooks.
