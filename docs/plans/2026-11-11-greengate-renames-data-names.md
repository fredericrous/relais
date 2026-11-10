---
canonical: greengate:docs/plans/2026-11-11-greengate-renames-data-names.md
phases: [1, 5, 6]
status: active
---
Part of [greengate 4.0 renames its data names](https://github.com/greenharness/greengate/blob/main/docs/plans/2026-11-11-greengate-renames-data-names.md): this repository takes Phase 1, relais reading `greengate.conf` and the `greengate-list-v1` envelope beside the old names; Phase 5 (the move to `greenharness-slots` in the install window) and Phase 6 (the census) come on later branches.

## Code review
- relais: backend, lang:rust

Phase 1 here touched `crates/relais/src/repo/detect.rs`, `crates/relais/src/verify/mod.rs` (the one `LIST_FORMATS` both readers use) and `crates/relais/src/workspace/mod.rs` (`greengate.conf` protected like `amont.conf`). `crates/relais/src/verify/heavy.rs` reads no config or marker name, only the slots, so it has nothing to change before Phase 5.

## Verification

- A repository with both `greengate.conf` and `amont.conf`, no gate on PATH → the gates are read from `greengate.conf` and labelled with it → `a_greengate_conf_wins_over_amont_conf_and_names_its_gates` passes.
- A `greengate-list-v1` answer for `amont.conf`, then for a swept repository with only `greengate.conf` → the same gates, no note, labelled with the manifest they came from → `a_greengate_list_envelope_is_read` passes.
- The verify inventory parser on a `greengate-list-v1` envelope → equal to the `amont-list-v1` one → `greengate_names_the_same_list_envelope` passes.
- The old names → the existing `amont.conf` / `amont-list-v1` tests in `repo::detect` and `verify` pass unchanged; an `amont-list-v2` envelope is refused with a message naming both formats → passes.
- `greengate.conf` and `amont.conf`, at the root → protected → `protected_configuration_is_protected_at_any_depth` passes.
- Real-input evidence against a greengate 4.0 `list --json` is carried to Phase 5, which installs 4.0 (backend review).

## Implementation review

- implementation-review: approve-with-changes, then approve on the delta; fixed: verification rows observed, files named, both formats in messages (78k+23k).
- backend: approve-with-changes, then approve; fixed: messages, protected-path test; carried: a real greengate 4.0 `list --json` check at Phase 5 (60k+22k).
- lang:rust: approve-with-changes twice; fixed: one `LIST_FORMATS`, docs, swept-repository test; deliberate: `amont_conf_gates` keeps its name beside `amont_gates`/`amont_list` until the census renames the module's `amont_*` together; no added line passes 100 columns (45k+27k).
