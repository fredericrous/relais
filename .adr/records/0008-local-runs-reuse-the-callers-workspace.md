---
id: ADR-0008
status: accepted
decisions:
  - key: workspace.local-reuse
    choice: a local run forks the caller's workspace and clones its dependency directories copy-on-write, or, when the caller is a linked git worktree, edits that worktree in place under a lock with a persisted end; a setup step is skipped only when it declared `reuse` and proved its outputs; a CI or server run keeps today's full isolation, selected by a machine setting and the CI variable
    first: true
    reason: A run built its own worktree and verification trees and ran every setup step in each, 6.5 GB and 10-25 minutes of `yarn install` per tree on happier, and one run died of ENOSPC while the caller's own worktree held a working install; the person chose copy-on-write forks by default and in-place for a disposable task worktree, with the server unchanged (2026-10-13)
---
# 0008 — local runs reuse the caller's workspace

## Context

Until v0.19.0 every run created a task worktree and verification trees (`verify-base`, one per candidate) and ran every `[[verification.profiles.*.setup]]` step in each. A worktree is a checkout and nothing else, so on happier (a Yarn 1 monorepo) that was a full `yarn install` per tree before the worker started. Run `run-65dbb7f001856-1064a` died of ENOSPC on a disk with 16 GB free while the caller's task worktree held a 6.2 GB install, idle.

SPEC §8 required "an owned task worktree" and refused a dirty tree; §10 ran every setup step in every tree; §18 said a cached baseline skips the base's setup "not the task worktree's". Those sentences were right for a server runtime and wrong for one person's workstation with little disk and memory.

## Decision

1. **Where a run works** is resolved once: `--workspace`, `RELAIS_WORKSPACE`, `CI`, the machine setting `[workspace] mode`, then `auto`. `CI` or `RELAIS_WORKSPACE=isolated` force `isolated` and a flag cannot widen it. A repository's relais.toml can only turn in-place off.
2. **Fork** (the default on a primary checkout): a fresh worktree whose git-ignored dependency directories (`node_modules`, `target`, or `[workspace] deps`) are cloned copy-on-write from the caller (`clonefile`, `FICLONE`), never copied. A filesystem that cannot clone says so and runs today's setup. Python environments are never seeded. Every setup step still runs afterwards.
3. **Skipping a setup step** is opt-in per step: it declares `reuse = { inputs, outputs, verify }` and runs unless its key (argv, tracked input contents, toolchain, earlier steps) equals the source's recorded result, its outputs exist and its `verify` command passes. Timestamps never authorize a skip.
4. **In place** (the default in a linked worktree, never in the primary checkout): the worker edits the caller's worktree under a kernel lock, from a start snapshot that includes uncommitted work. relais builds that snapshot in a private index and never stages the person's. The end tree (the accepted change, or the start) is written to a marker before the tree is touched and applied idempotently; a restore that would replace files it did not make is refused, and a moved `HEAD` leaves tree and branch alone. An interrupted run blocks the next until `relais resume --retire`.
5. **Base verdicts** are keyed by the base's tree, not its commit.
6. **Isolated** (server, CI, replay, live trials) is unchanged.

## Consequences

- The repo grant (ADR-0007) covers where declared commands run, not which; in-place is reachable only in a linked worktree of the granted repository, and the person can force `isolated`. `[workspace]` and `reuse` are skipped from the authority hash when absent; a step's `reuse` joins its line in the commands-changed declaration.
- A machine.toml with `[workspace]` cannot be read by v0.19.0 (`deny_unknown_fields`); it is absent by default.
- A worker's Bash is not confined (SPEC §8): relais promises detection and recovery evidence, not byte-identity of the real index against a worker that bypasses its private one. It keeps a copy of the real index and reports `real_index_changed`.
- Concurrent runs in one worktree are not supported: a second run's base would contain the first's half-written edits.
