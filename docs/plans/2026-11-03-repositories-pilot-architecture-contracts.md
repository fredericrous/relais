---
canonical: github.com/fredericrous/decisions/docs/plans/2026-11-03-repositories-pilot-architecture-contracts.md
phases: [2]
status: active
---
# Pointer: four repositories pilot their architecture contracts

This repository carries Phase 2: the `cli` cycle fix (`cli/decision_record.rs`),
`cli` added to the child-cycle script runs, the refreshed packs and the
`amont trust` grant, and CI's adr-lint pin moved to 0.15.0. relais's
`[architecture]` contract waits on the ports-rewrite plan (Phase 2b). The plan,
its reviews and its decision log live in the canonical file above.

## Verification (Phase 2, observed 2026-11-03)

| input | expected | actual |
|---|---|---|
| `python3 scripts/check-child-cycles.py crates/relais/src/cli` on origin/main | the two known cycles | `CYCLES: ['decide -> feedback -> decide', 'decide -> salvage -> decide']`, exit 1 |
| the same on this branch | no cycle | `CYCLES: none`, exit 0 |
| `python3 scripts/check-module-cycles.py` | pass | exit 0 |
| `amont run --stamp pre-commit-clippy pre-commit-cargo-test` on tree ce3adbe4 | both pass | both stamped (clippy `-D warnings`, the full test suite) |
| `adr-lint sync --check` with 0.15.0 (CI's new pin) | pass | exit 0 |
| `make lint` runs `check-child-cycles.py crates/relais/src/cli` | listed | Makefile `lint:` and ci.yaml carry the `cli` line beside `ledger` and `runner` |
| refreshed packs, `amont trust` | the 0.15.0 adr-lint row, trusted | amont.conf row pinned to e4d1119, `✓ trusted (d0708a6b…)` |
| `make lint` on the final tree (4485a0e2) | pass, incl. the `cli` child-cycle line | exit 0; `CYCLES: none` for `cli` |
| `make check` on the final tree | pass | first run: 2174 passed, 1 failed — `policy::tests::the_authority_hash_survives_effort_becoming_data`, the own-policy hash moved by the new `child-cycles-cli` command; the hash and its history were updated (seventh deliberate move), then the commit's `cargo-test` gate passed (32 checks) |
| a real commit staging only a comment in `adr-lint.toml` | `adr-lint-check` runs | `amont list --stage pre-commit` with only `adr-lint.toml` staged: `● adr-lint-check (declared)`; the commit passed its 28 checks (probe commit then dropped) |
| `child-cycles-cli` gate | blocks at commit, runs in relais verification | amont.conf `pre-commit child-cycles-cli … block`; relais.toml `amont run --stamp child-cycles-cli`; `amont trust` re-granted (a1c73977…) |

Note: the own-policy authority hash moved, so the person's trust grant for relais runs in this repository must be renewed after merge.
