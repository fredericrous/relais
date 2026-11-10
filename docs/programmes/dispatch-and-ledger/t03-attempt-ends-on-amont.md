+++
type = "research"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais", "amont"]
+++
# What stops amont's push gate from reusing a relais candidate's verdict

## Question

One verdict per tree decided that relais verifies through `amont run
--stamp`, and that amont's push gate then reuses the stamp. On amont, relais
still runs plain `make check` and the push gate reruns it. What stands in the
way: the repositories' `relais.toml` profiles, the empty `core.hooksPath`
relais commits with, the tree a stamp is bound to versus the tree that is
pushed, or something else?

## Look at

- `crates/relais/src/verify/stamps.rs` and the `amont run --stamp` prefix
  handling.
- `amont/relais.toml` and `relais/relais.toml`: `[verification.profiles.*]`
  commands.
- `crates/relais/src/repo.rs` (the commit with an empty `core.hooksPath`).
- decisions `docs/programmes/one-verdict-per-tree/t01-*.md`, `t03-*.md`.
- amont: how `run --stamp` records a stamp, and what the pre-push gate reads.

## Answer looks like

For each blocker: where it is (file:line or config), and the smallest change
that removes it. Then, for one repository, the profile line that makes the
push gate reuse a relais candidate's verdict, and the tree identity both
sides must agree on.

## Resolution

2026-11-10, Explore agent, key line spot-checked.

**The blocker is the profiles.** `amont/relais.toml:43-44` runs `["make",
"check"]`, so no `amont run --stamp` ever runs on amont and no stamp exists
for the push gate to reuse. Only relais's own `relais.toml` (`:86-110`) uses
`amont run --stamp <gate>`; no other repository's profile does. One verdict
per tree t03 foresaw this ("one opaque command … earns no stamp until its
profile is split per gate").

**Not blockers:**
- **The empty `core.hooksPath`:** it is in test fixtures only
  (`repo.rs:431`). Candidates are snapshotted with `git commit-tree`
  (`workspace/mod.rs:384-420`), and `amont run --stamp` writes the git note
  itself (amont ADR-0015).
- **relais's side:** it already recognises `STAMP_PREFIX`
  (`verify/stamps.rs:37`), reads `amont stamps --json` (`verify/mod.rs:2344`)
  and treats an absent or deferred stamp as a gap.
- **A plan commit on top:** amont keys a stamp by declared-input fingerprint
  (`gate_stamp.rs:413-470`), and amont's `attest-inputs` excludes
  `docs/plans`, so a plan-only commit keeps the stamp. A rebase that changes
  declared inputs reruns the gate, by design.

**Gaps the build must handle:**
- **What the stamp covers:** on amont, `cargo-test` (`pre-commit-cargo-test`
  / `pre-push-cargo-test`). `make check` also runs `scripts/test_bump_tap.py`
  and `attest-inputs`, which are not gates, so they stay plain commands.
- **The gate token** binds the command and the resolved program's path,
  size and mtime (`gate_identity.rs:185-196`). If relais's verify
  environment resolves another `cargo` than the push shell, the push reruns.
  Unverified.
- **A clean HEAD:** `run --stamp` stamps HEAD's tree and refuses a dirty
  tree. relais's own profile works this way, so it holds there; unverified
  for an amont run.

**The profile line for amont:** `argv = ["amont","run","--stamp","cargo-test"]`,
`timeout_seconds = 3600`, `env = { AMONT_ON_BUSY = "wait" }`, plus
plain commands for the non-gate parts. Both sides must agree on the
candidate commit's tree (or its declared-input fingerprint), the same
`cargo` binary, and the same machine's notes ref.
