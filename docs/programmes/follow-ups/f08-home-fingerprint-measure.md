+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais", "greenbelay"]
due = "2026-11-27"
+++
# Measure the home fingerprint warnings

## Question

Seven days after each phase is installed, how many home fingerprint warnings were caused by a tool, and how many by the person's own edits; and is the hook fast enough?

## Look at

The "Measure after release" section of `docs/plans/2026-11-20-home-fingerprint-warns.md`. relais: `home_drift` evidence rows since the v0.73.0 install (due 2026-11-27). greenbelay: `home-fingerprint-drift` journal lines from 7 days after the first greenbelay release carrying #306: greenbelay v6.6.0, released 2026-11-21 (tag eb07f717, #307), so its 7 days end 2026-11-28, a day after the relais due date.

## Answer looks like

Per tool, a count of warnings sorted into tool-caused and person edits, and greenbelay's hook p95. The rollback holds if more than 1 in 3 are the person's edits or the p95 exceeds 50 ms: relais's `enabled` default goes to false, greenbelay's assertion is removed.
