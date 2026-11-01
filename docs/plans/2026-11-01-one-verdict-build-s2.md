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

- **Departure from the plan (toolchain):** the plan says the proven skip also requires "the toolchain matches the baseline key". A stamp carries no toolchain and amont's stamp key has none, so relais cannot check that. The skip requires an identified toolchain (`toolchain_identified`, computed as the baseline cache's key is), which is enough while the stamped profiles run only in repositories that pin their toolchain in the tree. relais pins `rust-toolchain.toml`. The `holds-until:` comment at `base_is_proven` (`verify/stamps.rs`) names the end condition: the first unpinned repository to adopt a stamped profile must carry the toolchain in the stamp or the key first.
- **Partial answers:** `amont stamps` keys its answer by check id (`pre-commit-cargo-test`, `pre-push-cargo-test`), while the profile names short gates (`cargo-test`).
  - Each asked name is resolved against the ids by exact match after the `pre-commit-` or `pre-push-` prefix.
  - A name the answer says nothing about is a gap, and it blocks the proof, so a partial answer neither passes nor proves.
  - An unknown gate name makes amont 1.66.1 exit 2 with `amont: unknown gate "…"` on stderr (measured), which the probe already reports as a refusal gap.

## Verification record (S2)

| Input | Expected | Actual |
|---|---|---|
| `amont stamps --json` in the worktree (amont 1.66.1, installed) | an answer with the tree, platform and gates | `{"tree":"a16d3c24…","platform":"x86_64-macos","gates":{"pre-push-cargo-test":"absent"}}` |
| `amont stamps --json` with the seven profile gate names | every name resolves, all `absent` on an unstamped tree | the eight ids `pre-commit-{cargo-fmt,clippy,module-cycles,child-cycles-ledger,child-cycles-runner,cargo-test,plugin-test}` and `pre-push-cargo-test`, all `absent` |
| `amont stamps --json zzz-not-a-gate` | usage error | `amont: unknown gate "zzz-not-a-gate" — try `amont list``, exit 2 |
| `make lint` + `make test` on the implementation tree | green | 2128 + 43 + 286 + 1 passed, 0 failed |
| the new unit tests (`verify::stamps`, `a_receipt_without_baseline_proven_reads_false`) | the gap and proven rules hold, old receipts read `baseline_proven: false` | passed; see the review-fix commit |

**Post-release steps (the live checks).** They wait for the relais release, because a branch binary must not touch the real ledger. The parent runs them with the released binary:

1. A real relais run on relais.
   - Expected: its log shows one `amont run --stamp <gate>` command per gate (seven), and the receipt has no gap.
2. The person's commit and push of that exact tree.
   - Expected: both say "not repeating it here" for `cargo-test`.
3. A second run off the same merged base.
   - Expected: the log carries a `baseline_proven` decision, the receipt reads `baseline_proven: true`, and `baseline-cache/` gains no entry.
