+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# Name a declared bin in the aval launch-failure hint

## Question

relais v0.85.0 (t14 of decisions `topo-spotter-rename`) ends the aval
launch-failure detail with "check that `{program}` on PATH runs, or set `bin`
under [integrations] in relais.toml". When the repository already declares
`bin`, PATH plays no part and `bin` is already set, so the hint points the
operator the wrong way. Which wording does each source of the program get
(declared `bin`, `greentopo` found on PATH, the `aval` fallback)?

## Look at

- `crates/relais/src/context/mod.rs`: the `RunError::Spawn` arm of
  `aval_resolve_in` (the `could not be launched` detail).
- `crates/relais/src/tooling.rs`: `missing_integration_detail`, which already
  branches on `declared_bin`, and its test `the_aval_blocker_names_what_to_install`.
- The t14 reviews in `docs/plans/2026-11-25-relais-spawns-greentopo.md`
  (backend, tui, unix, rust: low, kept deliberate).

## Answer looks like

The hint branches on where the program came from, with a test pinning each
wording, in one relais release.
