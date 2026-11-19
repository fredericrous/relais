+++
type = "research"
mode = "afk"
status = "open"
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
