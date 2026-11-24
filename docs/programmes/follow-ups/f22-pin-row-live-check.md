+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
due = "2026-12-01"
+++
# A pin row's model matches the model its subagent ran

## Question

Since f19, a pinned spawn's `router_decisions` row records the model the
router infers the call ran, not the pin. Does that hold in live sessions,
once the relais plugin is reinstalled and relais v0.82.0 or later serves
`pin_sources`?

## Look at

- `router_decisions` rows with reason `pin` written after the reinstall,
  joined to `router_usage` step rows of the same session and `agent_id`
  (source `step`).
- The Agent `tool_use` calls behind them in the session transcripts: their
  `subagent_type` and `model`, and whether the parent was itself a subagent.
- Plan `docs/plans/2026-11-24-pin-row-records-call-model.md`, the Verification
  live rows, and its carried findings.

## Answer looks like

The share of new-plugin pin rows whose `model` equals the subagent's step
model, split by pin source (`own`, `preferred`, `machine`). The count of rows
that kept the raw pin string while the session model was known (expected 0).
One `relais-review` launch with no `model`, in a session on a non-Opus model,
whose row names the session model. Nested spawns (a subagent's own spawn)
compared apart. A mismatch on an explicit call `model` is a defect, filed.
