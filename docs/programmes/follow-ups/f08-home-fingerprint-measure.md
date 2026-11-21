+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais", "greenbelay"]
due = "2026-11-28"
+++
# Measure the home fingerprint warnings

## Question

Seven days after each phase is installed, how many home fingerprint warnings were caused by a tool, and how many by the person's own edits; and is the hook fast enough?

## Look at

The "Measure after release" section of `docs/plans/2026-11-20-home-fingerprint-warns.md`. relais: `home_drift` evidence rows since the v0.73.0 install (due 2026-11-27). greenbelay: `home-fingerprint-drift` journal lines from 7 days after the first greenbelay release carrying #306 is installed: that release is greenbelay v6.6.0 (tag eb07f717 pushed 2026-11-21, #307). The Mac still ran 6.5.0 at 02:50 that day, so the window counts from `brew info greenbelay`'s install time of 6.6.0 and ends 2026-11-28 at the earliest.

## Answer looks like

Per tool, a count of warnings sorted into tool-caused and person edits, and greenbelay's hook p95. The rollback holds if more than 1 in 3 are the person's edits or the p95 exceeds 50 ms: relais's `enabled` default goes to false, greenbelay's assertion is removed.
