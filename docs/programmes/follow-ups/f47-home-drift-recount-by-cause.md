+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
due = "2026-12-05"
+++
# Recount home drifts by cause after a week of v0.93.0

## Question

f08 found 0.5 home-drift rows per run, nearly all of them `.git/config`
and the plugin JSONs, but only one row carried a cause. Over the week of
`home_drift` rows from 2026-11-28 01:36 UTC (v0.93.0 installed), how many
are `unexplained`, `call` and `writer`, and how many runs show at least
one unexplained drift?

## Look at

`evidence` rows of kind `home_drift` in the ledger with `at` from
2026-11-28T01:36; each row's JSON `cause`; `runs.created_at` over the same
window for the denominator. f08's Resolution has the earlier counts.

## Answer looks like

Per cause, a count, and the runs with an unexplained drift out of all runs.
It holds if unexplained drifts show on few enough runs that one stands out.
Otherwise name the writer behind the most frequent unexplained path for a
known-writer entry.
