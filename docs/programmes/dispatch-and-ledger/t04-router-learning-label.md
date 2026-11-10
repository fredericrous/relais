+++
type = "grilling"
mode = "hitl"
status = "out-of-scope"
blocked_by = []
repos = ["relais"]
+++
# What the router learns from once the verdict comes from amont

## Question

ADR-0006 promotes and rolls back routing recipes on "accepted without
escalation". Which amont-side event becomes that label: the stamp on the
candidate tree, the push gate passing, or the pull request merging?

## Resolution

Out of scope, 2026-11-10: the person chose "Delegate, stay standalone", so
relais keeps its own verdict, from either adapter, and the label does not
change.
