---
id: ADR-0009
status: accepted
decisions:
  - key: workspace.local-reuse
    choice: a local run forks the caller's workspace and clones its dependency directories copy-on-write, or, when the caller is a linked git worktree, edits that worktree in place under a lock with a persisted end and gives the person's real git index back byte for byte when the run ends; a fork never seeds from a worktree an in-place run holds; a setup step is skipped only when it declared `reuse` and proved its outputs; a baseline cache hit keeps the base's check outcomes and their logs; a CI or server run keeps today's full isolation, selected by a machine setting and the CI variable
    replaces: [ADR-0008]
    reason: ADR-0008 promised detection of a worker that stages into the real index and left the person to repair it, and a cache hit lost the base's outcomes so an identical candidate re-ran every check; the person chose to restore the index at the end rather than report it or rewrite the worker's Bash (2026-10-14)
---
# 0009 — the real index is restored; the baseline cache keeps outcomes

## Context

ADR-0008 shipped local workspace reuse in v0.20.1 and recorded six gaps. Two of them change a promise it made:

- A worker's own `git add` (its Bash is not confined) changes the person's real index. relais kept a copy and reported `real_index_changed`, which told the person what happened and left them to undo it.
- A baseline cache hit returned the base's failures and nothing else, so an inspect run, whose candidate is identical to its base, ran the base's checks again.

## Decision

1. **The index.** At the end of the run that did the work, once the end tree is applied, relais restores `<git-dir>/index` from its byte copy: it takes git's own `index.lock` (a git running concurrently fails instead of racing), writes the copy into it, renames it over the index, and requires `git ls-files -s` to equal the listing at start. Anything else (the lock exists, the copy is missing, the listing still differs) leaves the index as it is, keeps the copy and reports `real_index_changed` with its path, as before. Staging the person does during a run is lost, and the start message says so.
2. **A killed run.** `relais resume --retire` of a run that died never overwrites the index: staging done after the death is not the worker's to discard. It reports `changed` and prints the copy's path. With no run id it also finds interrupted in-place runs in the worktrees of the repository it is run from.
3. **A held source.** A fork refuses to seed from a source that carries an in-place marker, before and after the clone, and runs today's full setup instead.
4. **The cache.** A baseline entry keeps the base's check outcomes and the bytes of their logs and JUnit reports as sha-named blobs, up to 16 MiB of logs per entry and 256 MiB for the directory, oldest first. A hit writes the logs into the new run, verifies each hash, and records them as that run's evidence; any piece missing or changed is a miss. Over the limit an entry keeps its failures alone and says so.
5. **Rejected.** Rewriting a worker's Bash through a plugin hook so it carries a private `GIT_INDEX_FILE`: it touches every worker command and the contract for rewriting a tool call cannot be checked from this repository.

## Consequences

- Supersedes ADR-0008's "detection and recovery evidence, not byte-identity" for the index, and its decision that a cache hit keeps failures only.
- The baseline cache key moves (version 4): old entries miss, never answer wrongly.
- `doctor` names the workspace mode and why, whether copy-on-write works between the repository and the state directory, and any interrupted in-place run.
- Concurrent runs in one worktree remain unsupported.
