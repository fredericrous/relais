+++
type = "research"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# What each activation gate needs that the ledger does not yet hold

## Question

For each of the four activation gates in
`docs/plans/2026-10-07-session-router.md` §5 (≥ 20 drawn with strong
outcomes and unknown rate ≤ 0.5; a success lower bound; savings in total
cost per completed task with a 90% interval excluding zero; label quality
with a missed-failure upper bound ≤ 0.1 and no flag or revert miss), which
inputs can be computed from today's ledger tables and `router::stats`, and
which are missing: a per-stratum cost per completed task on drawn versus
undrawn tasks (the arm figure exists per session, not per stratum), a
two-arm bootstrap of the difference, the missed-failure measure, the
`/relais-flag` and revert signals. Which gates can the two ready review
strata pass today?

## Look at

- `docs/plans/2026-10-07-session-router.md` §5 (lines 168–212) and §6.
- `crates/relais/src/router/explore.rs`, `stats.rs`, `report.rs` (the
  per-session split in `session_routing`), `crates/relais/src/ledger/router.rs`
  (`router_exploration_tasks`, `router_window`, `router_task_token_totals`).
- The ledger schema for `router_tasks`, `router_decisions`, `router_usage`
  (`crates/relais/src/ledger/schema/migrations.rs`).
- `crates/relais/src/learn/typed_gate.rs`: the artifact learner's gate
  evaluation, the likely precedent for `router evaluate`.
- A read-only query on a `.backup` copy of `~/.local/state/relais/ledger.sqlite`
  for the two ready strata: drawn and undrawn tasks, their sessions, and
  whether their usage rows are priced.

## Answer looks like

A table, one row per gate: inputs it needs, which exist (file and
function), which are missing, and the smallest addition that supplies each.
Then, for `sub review d3 module` and `sub review d4-5 cross-cutting`: the
gate-1 and gate-2 figures computed from the copy, and whether gate 3 is
conclusive given the unpriced sessions in their windows.
