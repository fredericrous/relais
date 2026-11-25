# Full reviews: fence decide answers through the grant

Round 1 (opus): backend approve-with-changes (3 findings), lang:rust approve-with-changes (3), tui approve-with-changes (3), unix approve-with-changes (3).
Round 2: backend approve, lang:rust approve-with-changes (grep row; fixed), tui approve-with-changes ("nothing was written" false after --criterion; fixed), unix approve.
Final body: backend, lang:rust, tui, unix approve.

Rejected: backend round-1 #1 (a repeat on a closed decision never reaches the fence: the run is terminal and decide.rs:502 refuses it first); recorded in the Decision log.

Carried into implementation: backend #3 (a failed re-read keeps the grant, so the node's own retry reads "elsewhere" for up to 10 minutes; bounded by the TTL); tui #3 (the sign-off note as its own Behaviour bullet; it stays in Non-goals and Verification); rust (add the failed re-read case to the lib test row); unix (a positive control for the grep row: the closure's one `approve_from_snapshot(` hit).
