---
status: active
branch: feat/verify-through-amont
repos: [relais]
canonical: decisions:docs/plans/2026-11-01-one-verdict-build.md
phases: [S2]
---
# One verdict per tree, build: S2 relais verifies through amont (pointer)

The canonical plan is decisions `docs/plans/2026-11-01-one-verdict-build.md`. This branch builds phase **S2**:

- relais's own profile runs each gate as `amont run --stamp <gate>`.
- A missing stamp or a deferred gate is a verification gap, never a pass.
- A base that amont already proved green skips its baseline run.

## Decisions made while building

- **Seam:** a new `verify::stamps` module holds the types, the parser, the gap and proven rules, and the probe. The existing `HookInventory` port gains a `stamps` method that asks nothing by default; only `AmontCli` spawns `amont stamps --json`. The runner files (`verification.rs`, `baseline.rs`) get one small call site each, so T1's rebase stays small.
- **Gap rule:** the answer's gates are keyed by check id (`pre-commit-cargo-test`), while the profile names short gates (`cargo-test`), which amont resolves.
  - Every gate amont reports must be `stamped` or `attested`.
  - An answer naming no gate, a refusal (amont older than 1.66, or an unknown gate) and a probe that did not run are each a gap naming why.
  - Integration off is no gap.
  - A candidate identical to its base, which has no worktree of its own, is not asked.
- **Toolchain in the proven skip:** a stamp carries no toolchain (amont ADR-0015 keys it by tree and gate identity), so relais cannot prove the stamp was earned on the toolchain it would use now. It requires what the baseline cache requires: a toolchain it can identify. relais pins its toolchain in `rust-toolchain.toml`, so here the toolchain is a function of the tree. An unpinned repository takes the residual risk, recorded as an open question.
- **child-cycles:** `scripts/check-child-cycles.py` takes one directory, so `amont.conf` gains two gates: `child-cycles-ledger` and `child-cycles-runner`.
- **No runner-level test:** whether amont is on is decided by the real binary on the machine (`integration_available("amont")`), so a runner test would pass on the laptop and behave differently on CI. The rules are pure and fully unit-tested, the probe is tested against fake `amont` scripts, and the wiring is one call site each.
