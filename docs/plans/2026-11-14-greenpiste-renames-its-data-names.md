---
canonical: greenharness/greenpiste:docs/plans/2026-11-14-greenpiste-renames-its-data-names.md
phases: [1]
status: done
branch: feat/greenpiste-data-reads
---
# greenpiste renames its data names (pointer)

This repository's slice of greenpiste's 3b-2 plan (Phase 1, readers read
both): `review_outcome.rs` reads the greenpiste ledger path, both row and
merge schemas, and `GREENPISTE_PLAN_ROOT` before the old ones. The new name
is read first, the old one only when the new is absent (`NotFound`); the
old reads go at the programme census. `quiet.rs` needed no code: its
dotfile rule already skips both writers' temp files, and its comment now
names both.

## Code review
- relais: backend, lang:rust (unix, tui, architect left out: two readers gain a fallback, no output or boundary change; lang:typescript left out: no TypeScript file)

## Verification

- `cargo test -p relais --lib -- review_outcome:: quiet::` → 29 passed, among
  them: the new ledger path wins and the old is the fallback; both row and
  both merge schemas are read (by agent id); the new plan-root variable wins
  over the old (new-only, old-only, both, none).
- The release and its install here are tracked in the canonical plan's
  Phase 1, not in this slice.

## Implementation review

- implementation-review: approve-with-changes then approve on the delta; help/README defaults, the quiet test, the schema assertions fixed (49k/59 s, 30k/14 s).
- backend: approve-with-changes then approve; plan-root precedence now a tested pure function; NotFound-only fallback (60k/71 s, 29k/18 s).
- lang:rust: approve-with-changes then approve; `matches!` guards, `cfg(unix)`, private old const; deliberate: serde alias enum not taken, the String keeps the invalid count (50k/50 s, 26k/9 s).
- Candidate types.exhaustive-matching at review_outcome.rs:443: fine, `io::ErrorKind` is non-exhaustive, the catch-all is required.
