---
canonical: greenharness/greenpiste:docs/plans/2026-11-14-greenpiste-renames-its-data-names.md
phases: [1]
status: active
branch: feat/greenpiste-data-reads
---
# greenpiste renames its data names (pointer)

This repository's slice of greenpiste's 3b-2 plan (Phase 1, readers read
both):  backend, lang:rust (unix, tui, architect left out: two readers gain a fallback, no output or boundary change; lang:typescript left out: no TypeScript file):relais review_outcome.rs reads the greenpiste ledger path, both schemas and GREENPISTE_PLAN_ROOT before the old ones; quiet.rs skips both temp prefixes. The new name is read first, the old one only when the new is
absent; the old reads go at the programme census.

## Code review
- relais
