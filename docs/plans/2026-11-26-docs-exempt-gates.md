---
canonical: greensummit:docs/plans/2026-11-26-docs-exempt-gates.md
phases: [4]
status: active
---

Slice: Phase 4, f73 part only (`relais programme check` refuses duplicate
follow-up ids, plus `--with-ref <ref>`). The `@docs-exempt` adoption in relais
comes in a second PR after greensummit 2.1.0.

## Code review

- relais: backend, lang:rust, unix (tui, architect left out: one new check message, no new module)

## Decision log

- The check sits beside `analysis::check` as `analysis::duplicate_ids`, reported
  by `relais programme check` only (frontier and other readers need no git),
  not inside `analysis::check` as Phase 4 worded it.
- Each `renumber to` hint takes the next free number (f126, f127, f128 on
  decisions), so following every hint makes no new duplicate.

## Verification

- `cargo test -p relais --lib programme::` → all pass → 56 passed.
- `cargo test -p relais --test it programme_cli` → all pass → 48 passed (
  incl. orphan ref and usage-before-read).
- `cargo clippy -p relais --all-targets -D warnings` → clean → clean.
- `relais programme check --with-ref origin/main decisions/docs/programmes/follow-ups`
  (before the renumber) → exit 2, f11/f12/f70 on stderr as `path:1:` problems →
  exit 2, three lines, hints f126/f127/f128, stdout empty.
- same with `--with-ref nosuchref` → exit 1, one `relais:` line → exit 1, one
  line ending "is nosuchref a ref sharing history with HEAD?".
- Every `*/docs/programmes/*` in the fleet → only decisions follow-ups has
  duplicates; no `greengate.conf` runs `programme check`, so no block gate
  starts refusing on release.

## Implementation review

- implementation-review (opus): approve-with-changes; usage test now asserts the message with no map.md; plan wording recorded above. 56k, 57 s.
- backend (opus): approve-with-changes; orphan-ref test and SPEC sentences (working-tree listing; ref-only duplicates unreported, deliberate: a branch based on the ref reports them). 60k, 72 s.
- lang:rust (opus): approve-with-changes; two vectors replace the index sentinel, `Listing::Ref(rev)`; deliberate: no `GitRev` newtype (one boundary, tested), hint string kept, lossy names match `Programme::load`. 47k, 37 s.
- unix (opus): approve, no findings. 61k, 49 s.
- Round 2 (tree 8401457ff004, opus): all four approve; every round-1 finding resolved or accepted; deliberate: shadowed `found` closure parameter (low, harmless). 214k in all, 18–40 s.
