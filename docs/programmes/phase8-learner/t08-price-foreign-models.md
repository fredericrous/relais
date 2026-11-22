+++
type = "task"
mode = "hitl"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# Price the two foreign models so their sessions rejoin the headline

## Question

Four sessions since 2026-11-01 used `gpt-6.1-sol` (989 usage rows) or
`qwen3.6:35b-a3b` (4 rows) and are left out of the arm figures; the
savings gate is inconclusive for any arm that holds one. The person adds
their `[[pricing.models]]` entries to `~/.config/relais/machine.toml`
(shape in `docs/INTEGRATIONS.md:286`; the local qwen can be 0). Only the
person edits machine.toml.

## Look at

- `docs/INTEGRATIONS.md:278-300`; in `relais report`'s session-routing
  section the line reads `excluded N session(s) from cost, no
  [pricing.models] entry for:` followed by the ids, while the TOML entry
  is written `[[pricing.models]]` (an array of tables); follow-up f11 asks
  the message to match.

## Answer looks like

The two entries present, and `relais report` showing no excluded sessions
on each arm (or the remaining unpriced ids).
