+++
type = "research"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# How the relais crate is coupled inside

## Question

What is the module-to-module dependency graph of `crates/relais`? Who owns
each ledger table, and what state is shared between modules? Which of the
5-7k-line `mod.rs` files (`verify`, `admission`, `coordinator`, `report`) hide
several jobs, counted in non-test lines (inline `#[cfg(test)]` modules
excluded, and the counting method stated)? Which ties block a crate split:
the `include_str!` and `include_bytes!` paths (13 sites in 9 files), the
`conformance` feature, the tests that pin the skill text and the policy hash,
and the `pub(crate)` surface?

## Look at

- `crates/relais/Cargo.toml` (features, the `conformance` fake) and
  `crates/relais/src/lib.rs` (the module list and what each exports).
- `crates/relais/src/{verify,admission,coordinator,report,policy,route}/mod.rs`
  and `crates/relais/src/ledger/` with its schema.
- `command grep -rn 'include_str!\|include_bytes!' crates/relais/src` for the
  path-relative loads that break when a file moves to another crate.
- `crates/relais/src/install/` and the tests named in
  `docs/SPEC.md` (the skill text and the policy hash are pinned by tests).
- `use crate::` edges between the module groups named in the map's Notes.

## Answer looks like

A table of the module groups with the edges between them (who imports whom,
counted), the ledger tables each group owns, and the shared state. A second
table of the large files with non-test lines per job inside each. A list of the
ties that block a split, each with the file and the cost to cut it. Last, the
baseline for the map's Destination: the build time after a one-line edit and
the largest file's line count, with the command that measured each.
