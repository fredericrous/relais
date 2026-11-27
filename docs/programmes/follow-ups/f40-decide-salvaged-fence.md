+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# decide --answer salvaged takes the decision grant

## Question

Filed from the relais inbox (2026-11-24, session f138271b, an-t21 follow-up). `relais decide --answer salvaged` closes an open decision row through `Ledger::record_salvage_contained` without the decision grant, so a shared settle does not fence it. an-t21 left this out of scope.

## Look at

- `crates/relais/src/cli/decide.rs` (or its module) and `Ledger::record_salvage_contained`.
- an-t21's decision-grant fence.

## Answer looks like

`salvaged` takes the same grant as the other answers, with a test that a concurrent settle is fenced.
