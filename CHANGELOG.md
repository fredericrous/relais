# Changelog

What an upgrader gets, in sentences. Each [GitHub
release](https://github.com/fredericrous/relais/releases) carries the
mechanical pull-request list too, generated; this file is the part a human
wrote, and the release workflow refuses to tag a version whose section is
missing here.

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
- **Prebuilt binaries** for Linux (glibc and musl, x86_64 and aarch64),
  macOS (Intel and Apple silicon) and Windows, with checksums, and
  `install/install.sh` / `install.ps1` to fetch them verified.

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
