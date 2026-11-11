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

## Reopened 2026-11-11

A review of greengate and relais found the decision implemented in part, and
found a premise that fails once more than one node signs.

**What the code does** (`crates/greengate-runtime/src/cluster.rs:1762-1812`,
`attest.rs:182-260`, `dispatch.rs:1994-2021`):
- The pass is judged from Kubernetes-recorded facts: the Job's `Complete`
  state, the fetch init container's `tree <sha>` termination message, the gate
  container's exit code and `status.imageID`. Logs are not used.
- The laptop signs a second block of platform `x86_64-linux` with its own
  key, only for gates it proved on the cluster.
- Job creation is fenced to the `amont-gate-runner` ServiceAccount
  (`homelab: apps/amont-gates/rbac.yaml`), and Workflow creation by an
  admission policy.

**What does not hold:**
- **The signed block carries no image id, job or command.** Those sit in an
  unsigned note (`refs/notes/amont-gate-cluster`). A verifier cannot tell a
  cluster-derived `x86_64-linux` block from one CI signed, and the build-plan
  point above (a format change in git-attest) is still open.
- **The proven set is process memory** (`cluster.rs:75`, `PROVED`), so only
  the hook process that ran or collected the gate can sign it. Whether a pass
  read later from the stored note reaches the attestation path was not traced.
- **The key is one unencrypted file** (`~/.ssh/amont-attest`, mode 600, no
  Keychain, no agent), the same principal for every gate and platform. Anyone
  who copies it is the laptop.
- **"The laptop is the only signer" fails with several nodes.** A Coder node
  or any second host needs its own identity. `allowed_signers` (one file per
  repository) cannot say which node may sign which gate class or platform, has
  no expiry, and revocation means deleting two notes refs on origin. Blocks
  have no nonce and bind to a tree, not a node.
- relais does not verify signatures. It runs `amont attest covered` and
  matches the gate name in its output (`verify/mod.rs:2657-2696`), with the
  command still named `amont`.

The decision to reopen is the person's. The follow-on design (node identity,
a key registry, what a node may sign) belongs in the agents-many-nodes
programme (homelab repo), ticket t07. Signing and verification stay in
greengate, which is public under MIT.
