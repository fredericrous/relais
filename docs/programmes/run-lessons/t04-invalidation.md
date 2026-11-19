+++
type = "research"
mode = "afk"
status = "closed"
blocked_by = []
+++
# How a lesson knows the files it is about have changed

## Question

What does relais already compute that could pin a lesson to the files it is
about and drop it when they change (blob ids at the lesson's paths, the
context manifest's source fingerprints, symbol extraction), and what does
each option cost per run when hundreds of lessons exist?

## Look at

- `crates/relais/src/context/mod.rs` (manifest fingerprints), `fsclone.rs`,
  `contract/scope.rs` (`scope_could_touch`, overlap of globs), SPEC §7 and
  §18.
- How many distinct paths recent contracts' write scopes and read hints
  expand to (ledger `contract_revisions`).

## Answer looks like

Two or three invalidation designs (for example: blob ids of the named
files at the base; a path glob plus the tree of its directory; a time to
live), each with what it misses, what it drops too eagerly and the measured
or estimated cost at preflight; and a recommendation.

## Resolution

Researched 2026-11-19 (Explore agent, read-only, ledger `contract_revisions`,
latest 300 revisions).

- **What exists:** `fingerprint_hints` (`context/mod.rs:717`) already records
  `(path, blob)` per read hint at the base, capped at 400 files
  (`mod.rs:640`), into `ContextManifest.fingerprints`. `scope_could_touch`
  (`contract/scope.rs:29`) tests glob overlap. No symbol extraction exists.
- **Measured:** write scope averages 9.7 globs per contract (max 30; 818 of
  2,661 entries hold `**`); read hints average 6.8 (max 25), no globs.
- **Designs:** A, blob ids of the named files (misses unnamed callers and
  renames; drops on any edit of a named file); B, tree id of the directory
  (coarsest, any sibling edit drops it); C, time to live (blind).
- **Recommendation:** A, with a prefilter: one `git diff --name-only
  <oldest lesson base>..<base>` per run, `scope_could_touch` against each
  lesson's globs, blob comparison only for the lessons that overlap; drop
  on a changed or missing blob; C as a 30-day backstop. A few git processes
  per run regardless of lesson count (estimated, not timed). A lesson whose
  hints hit the 400 cap is advisory.
