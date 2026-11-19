+++
type = "grilling"
mode = "hitl"
status = "closed"
blocked_by = ["t02-finding-shape"]
+++
# Where a lesson appears in a sibling run

## Question

Does a lesson go to the worker only, or also to the reviewer (which then
checks that the sibling did not repeat it), under which byte budget, and
how is it worded beside aval's guidance, which outranks it?

## Resolution

The person's choice, 2026-11-19: **worker and reviewer.** The worker gets a
fenced "lessons from sibling runs" block under its own budget (about
8 KiB), ranked by file overlap with the write scope and read hints, with
the sentence that an applicable aval decision outranks a lesson. The
reviewer gets the same lines and reports a repeat as a finding.
