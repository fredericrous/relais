---
canonical: greengate:docs/plans/2026-11-11-greengate-renames-data-names.md
phases: [1, 5, 6]
status: active
---
Part of [greengate 4.0 renames its data names](https://github.com/greenharness/greengate/blob/main/docs/plans/2026-11-11-greengate-renames-data-names.md): this repository takes Phase 1, relais reading `greengate.conf` and the `greengate-list-v1` envelope beside the old names; Phase 5 (the move to `greenharness-slots` in the install window) and Phase 6 (the census) come on later branches.

## Code review
- relais: backend, lang:rust

## Verification

- Phase 1: relais tests read each new name and its old one; released; installed here.
