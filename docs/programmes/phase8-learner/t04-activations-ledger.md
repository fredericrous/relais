+++
type = "research"
mode = "afk"
status = "open"
blocked_by = ["t01-gate-inputs"]
repos = ["relais"]
+++
# What `router_activations` records, and how rollback reads it

## Question

Which columns does `router_activations` need so that `relais router
evaluate` can show why a stratum passed, `relais router rollback` can undo
exactly one activation, the early trigger can roll back on evidence, and
the same data never activates the same stratum twice (ADR-0003's rule for
the artifact learner)? Does the stop-row `label_policy_version` column
(owed per the `holds-until:` on `ROUTER_LABEL_POLICY_VERSION`) ride the
same ledger step (48 → 49)?

## Look at

- `crates/relais/src/ledger/schema/migrations.rs` (`router_exploration_stops`,
  the v45–v48 steps), `crates/relais/src/ledger/learning.rs` and
  `learn/registry.rs`, `learn/promote.rs` and `learn/typed_rollback.rs`
  (how artifact activations and rollbacks are recorded and undone),
  `.adr/records/0003-*.md`; the map's open point on how `router rollback`
  names what it undoes.

## Answer looks like

The table definition, the accessor signatures, the dedup rule, and whether
the stop-row column rides along, each with the reason.
