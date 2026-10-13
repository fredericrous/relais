# Reviews: trust a repository once


- **backend r1** (38k, 48 s) approve-with-changes: high, no rollback (`MachineSettings` is `deny_unknown_fields`, a downgrade cannot read `[trust_repos]`); high, `declaration()` includes models, so hashing it warns on every routing candidate; medium, the "once" consumed by `plan`/replay, no starting record, concurrent runs; medium, ADR path is `.adr/records/`; low, legacy-grant count for the contract step; low, origin keying covers same-origin clones. All applied.
- **backend r2** (44k, 18 s) approve-with-changes: findings 1–4 and 6 resolved. **Carried into implementation:** (7) the ledger step writes a copy of the ledger beside it before applying, and Rollback states that runs recorded after the upgrade are lost; (8) each run records its `TrustStatus` (`repository`/`legacy_exact`) in the same ledger step, which `doctor`'s 30-day legacy count reads; (9) preflight takes a declaration store from its caller (`run`) or one that records nothing (replay, `plan`, `doctor`), never a bool (`functions.no-flag-arguments`).

- **backend final** (33k, 23 s) approve-with-changes, no high or blocking: carries 7–9 belong in Changes and Rollback (the implementation does them there); also carried: (10) Verification adds a downgrade rehearsal (remove `[trust_repos]`, restore the ledger copy, v0.18.0 `relais plan` starts, blocked until `trust grant --key`, then trusted); a legacy-only repository admits its exact policy and refuses a candidate.

