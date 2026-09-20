# Changelog

What an upgrader gets, in sentences. Each [GitHub
release](https://github.com/fredericrous/relais/releases) carries the
mechanical pull-request list too, generated; this file is the part a human
wrote, and the release workflow refuses to tag a version whose section is
missing here.

## v0.1.2

### Fixed

- **Unknown usage is unknown, not zero.** A dispatch whose harness reported
  no cost was stored as `0` and summed into a figure that read as money,
  with only a completeness label to say otherwise. The ledger's
  `cost_micros` is nullable since schema v3 (the table is rebuilt on first
  open, rows preserved, and every row whose completeness already said
  `unknown` gets the NULL its zero stood for); unknown usage is left out of
  every sum; and every cost line — `run`, `status`, `explain`, `report` —
  reads `unknown (no usage was reported)` or `at least $x (unknown: …)`
  instead of a number. A session with subagents is recorded `inclusive`,
  and the ledger's dedup of inclusive parents is documented as ready for a
  managed nested dispatch that does not exist yet.

## v0.1.1

### Added

- **Windows.** The coordinator's endpoint on Windows is a loopback TCP
  port and a 32-byte nonce written to the same file a Unix socket would
  occupy; a connection that does not present the nonce is dropped before
  a byte of its request is read, and the file's ACL under the user's
  profile is the permission restriction SPEC §23 asks for. Process
  liveness, termination and parentage go through process handles; a
  worker tree is killed with `taskkill /T`. The release ships an
  `x86_64-pc-windows-msvc` zip again and `install/install.ps1` is back.
  Election, the stale-endpoint probe and cleanup are unchanged: they
  see one path and one listener on every platform.

## v0.1.0

The first release: the companion described in `docs/SPEC.md`, usable on a
repository today, with the audit that made it so.

### Added

- **The runner, the machine and the coordinator.** A task contract is
  frozen and hashed, routed by risk floors and a conservative baseline
  (the learned router abstains until it has evidence), executed in an
  owned worktree by a `claude -p` worker, verified against an immutable
  candidate commit, and receipted. The §9 lifecycle is one pure function
  with a test per row. Dispatch goes through a per-user coordinator that
  reserves seats and budget before any process exists.
- **`relais install --claude`**, preview-first, for the `/relais` skill and
  the namespaced agent definitions.
- **The learning loop**: dataset build, train, evaluate, promote — owned,
  local, and pinned per run.
- **Prebuilt binaries** for Linux (glibc and musl, x86_64 and aarch64)
  and macOS (Intel and Apple silicon), with checksums, and
  `install/install.sh` to fetch them verified. (No Windows in this
  release; 0.1.1 adds it.)

### Fixed, before anyone upgraded

The audit in `docs/AUDIT-2026-09-20.md`, run against Claude Code 2.1.278,
found and this release closes: the adapter probing flags the CLI does not
have (`--budget`, `--max-turns`), permission denials read as a worker's
choice, a candidate identity that changed every second, scope judged on the
live tree instead of the snapshot, one protected prefix unlocking all of
them, and a coordinator that idle-exited under a run that was merely
verifying. What remains open is issues #5–#10.

### Not in this release

No crates.io or npm package: `relais` is a working name whose availability
has not been checked. No homebrew formula until the tap is seeded.
