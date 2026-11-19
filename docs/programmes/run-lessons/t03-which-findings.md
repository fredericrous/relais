+++
type = "grilling"
mode = "hitl"
status = "closed"
blocked_by = ["t01-repeat-baseline", "t02-finding-shape"]
+++
# Which review findings become lessons for siblings

## Question

Which findings are passed to sibling runs: every finding, only those the
repair fixed (the finding was right), only blocking or rule-citing ones, or
only those that already repeated once?

## Resolution

The person's choice, 2026-11-19: **rule-cited and confirmed.** A finding
travels to siblings when it cites a code rule or a contract criterion and
the repair fixed it (its id is gone in the re-review). It needs t02's fixed
line grammar and findings table first. Grounded in t01: code-rule
violations repeat in a sibling 89 of 162 times, reviewer prose 1 of 162.
