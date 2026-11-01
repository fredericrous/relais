+++
type = "research"
mode = "afk"
status = "closed"
blocked_by = ["t20-stamp-platform-rule"]
repos = ["amont", "decisions"]
+++
# How amont earns and signs a stamp for a tree a pod verified

## Question

Under the per-gate rule (t20), how does a gate run in a Linux pod earn a stamp
the laptop trusts?

- The signing key stays on the laptop. What does the laptop check before it
  signs? Candidates: the tree sha, the image digest and the log sha256, per
  t14's result contract.
- What does the stamp record? The executor, the platform and the image
  digest.
- How does `amont stamps` answer per gate and platform?
- What does pre-push accept?

Read amont ADR-0010, ADR-0013, ADR-0015 and the `run --stamp` code, and
decisions ADR-0041.

## Resolution

Explore research, 2026-11-01. The relais inspect run in amont ended
`baseline_unrunnable`: amont's `make` hit its 3600 s limit under laptop load.

**A pod never stamps. It produces a signed attestation, and the laptop honours
it only for gates `amont.conf` declares platform-independent.**

- **Today:**
  - **Stamps:** unsigned local git notes (`refs/notes/amont-gate`,
    `amont-gate-v1`), never pushed. Their token identity hashes the host
    binary's path, size and mtime (`gate_stamp.rs`, ADR-0010/0011/0012).
    **Platform is not in a stamp.** Only `attested` carries platform
    (`run_stamp.rs:498,598`).
  - **Attestations:** `ssh-keygen`-signed notes in `refs/notes/amont-attest`,
    holding the tree, the gates, the platform and the amont version. They
    are verified by `git_attest::verify` against the repo's committed
    `allowed_signers`, and they already cross machines (CI `sign` job,
    `docs/ci.md:112-180`).
  - **ADR-0015 already decides** "a remote executor must never stamp":
    pre-push would sign its result under the laptop's platform
    (`adr/0015-a-runner-earns-the-stamp.md:60-65`).
- **The shape:**
  1. A pod run yields a signed attestation: platform `x86_64-linux`, the
     exact tree, the gate, the image digest and the log sha256.
  2. `states()` reports `attested` from it only when the gate is declared
     platform-independent. For any other gate it reports absent.
  3. The laptop may convert a verified attestation into a local stamp,
     after checking tree equality (it rederives `HEAD^{tree}` itself) and a
     remote gate identity. The host-binary identity cannot match a pod, so
     the remote identity is the command plus the image digest.
- **Declaring a gate:** an option such as `platform=any` on the gate line,
  e.g. `tree eslint eslint * attest platform=any npm run lint`, or a prefix
  marker like `heavy` with a `minVersion` bump. Older amont reads an unknown
  prefix as part of the command (`custom-checks.md:148-150`). The
  declaration is reviewed in the trust-gated `amont.conf`, the consumer's
  side, which follows `docs/ci.md:181-183`.
- **Records:** amend ADR-0015, whose "stamps stay on one machine" gains a
  carve-out for attestations. Add a decisions ADR carving `platform: any`
  per gate out of ADR-0041's "each tree proven once per platform". That ADR
  is the t20 rule. ADR-0010 stays untouched unless the token format changes.
- **Open, for t21 and t25:**
  - Does `passed` at `dispatch.rs:1340-1361` include stamp-satisfied gates?
    If so, a remote-derived stamp becomes laptop-signed, which is the main
    risk to settle.
  - Should CI's `platform: any` read the same declaration?
  - `gate_identity` when the binary is absent on the host.
  - Where the pod's signing key lives: t25.
- **Correction to the map:** stamps are not keyed by platform; attestations
  are.
