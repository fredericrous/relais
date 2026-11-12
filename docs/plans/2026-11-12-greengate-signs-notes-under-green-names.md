---
canonical: fredericrous/decisions:docs/plans/2026-11-12-greengate-signs-notes-under-green-names.md
phases: [2, 4, 5]
status: active
---
Part of [greengate signs its notes under green names](https://github.com/fredericrous/decisions/blob/main/docs/plans/2026-11-12-greengate-signs-notes-under-green-names.md) (ADR-0068): this repository takes Phase 2, `relais native gate-stamp` reading `refs/notes/greengate-gate` (header `greengate-gate-v1`) first and `refs/notes/amont-gate` (header `amont-gate-v1`) only when the green ref has no note for the tree; Phase 4 (the plugin's check name `greengate-gate@<tree7>`) and Phase 5 (the census removing the old read) come on later branches.

## Code review
- relais: backend, lang:rust, lang:typescript (unix, tui, architect left out: two readers, no command-line change)
