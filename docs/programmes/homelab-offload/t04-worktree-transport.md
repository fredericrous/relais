+++
type = "research"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais"]
+++
# How a worktree gets into a pod and a patch comes back

## Question

Today the owned worktree is local only (`crates/relais/src/workspace/`). Options: a Forgejo push, a git bundle, or an rsync; not every repo is on Forgejo. Research how an immutable input tree and run/attempt identity bind the remote workspace, the returned patch and the verification evidence, and how local edits made during remote execution are detected before a result is applied. t14 researches the matching result contract at the same time: whichever Resolution is written second aligns with the first.

## Resolution

Explore research, 2026-10-31 (the relais inspect run was blocked by low disk).
Recommended: **a git bundle of an immutable snapshot commit `S`** plus a
manifest. The pod returns a bundle or a patch, which relais rebinds to the
ledger by sha and digest before using it.

- **Today.**
  - Base: `resolve_base` resolves `<ref>^{commit}` once
    (`crates/relais/src/workspace/mod.rs:219`). A fork is created from the base
    commit and refuses dirty work (`mod.rs:1-12,231`).
  - Candidate: `snapshot_candidate` writes a deterministic commit, parent =
    base, with a fixed author and date (`mod.rs:391`), named
    `refs/relais/candidates/<run>/<attempt>` (`mod.rs:851`). Its patch digest
    is recorded as evidence (`runner/attempt.rs:632-653`).
  - `kind: verify` applies the ledger's commit and refuses on
    `candidate_digest_mismatch` (`runner/candidate.rs:156-215`).
  - In-place runs already snapshot dirty work as a start commit
    (`workspace/inplace.rs:346-420`), and detect `HeadMoved` (`:749`) and
    collisions (`:541`) before applying.
  - Copy-on-write dependency clones (`fsclone.rs`) are local only and cannot
    cross to a pod.
- **Why a bundle.**
  - It works for any repo, Forgejo or not.
  - It is small when the pod already holds the base objects.
  - It binds a commit sha, checked with `git bundle verify`.
  - It needs no server. A Forgejo push loses ref updates, as already
    observed, and needs a credential.
- **Alternatives.** A Forgejo push stays an optional accelerator for repos
  that live there. rsync is rejected as the carrier because it binds no
  identity.
- **Manifest.** `{run_id, attempt, base_sha, S tree sha, contract_hash,
  policy_hash}`. Remote evidence references the same `base_sha` and
  `candidate_sha` that `VerificationReport` carries (`verify/mod.rs:240-246`).
- **Before applying a result:** reuse `HeadMoved` and the collision checks.
  A fork-style remote result lands on a fresh branch, so it cannot conflict
  with local edits.
- **Agrees with t14,** which delivers the tree by sha with the same identity
  fields.
- **Open, for t02 and the plan:**
  - whether the pod holds the base objects (bundle size);
  - whether dependency directories come from the image;
  - whether remote runs accept dirty work through `S`, as in-place runs do;
  - whether a pod retry reuses the attempt index.
