+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
due = "2026-11-23"
+++
# The stop-row count a day after label policy 2 went live

## Question

Does `router_exploration_stops` still hold 0 rows a day after v0.77.0 is
installed, or did a row appear, and under which policy was it counted? A
row means the `holds-until:` on `ROUTER_LABEL_POLICY_VERSION` has fired and
the `label_policy_version` column is owed now (see the ticket on
`router_activations`).

## Look at

- `sqlite3 -readonly ~/.local/state/relais/ledger.sqlite "select * from router_exploration_stops"`
  (the installed binary writes it at session start through `router-state`).

## Answer looks like

The row count and, for any row, its counts and the policy in force when it
was written; `0` closes the ticket with no further action.
