+++
type = "grilling"
mode = "hitl"
status = "closed"
blocked_by = ["t01-repeat-baseline"]
+++
# When a lesson is a promotion candidate

## Question

How many distinct sessions, over what minimum age, make a still-valid
lesson a candidate for an aval or greenbelay rule, and how does relais hand
the candidate over (an inbox line, a follow-up ticket, a report section)?

## Resolution

The person's choice, 2026-11-19: a still-valid, unretired lesson given to
or hit by **3 distinct sessions** and at least **7 days** old (past the
revert window) is a promotion candidate. relais writes one line to the
repository's greenharness inbox, so the next session boundary files it as a
ticket; the rule itself is written by hand in greenbelay or an ADR (t07).
