# Full reviews: Relais host-slot jobs belong to the session

All seats ran on opus. Codex: not run (ticket agent in a programme drive).
`greenbelay plan-review record` and `plan-apply` could not find this agent's
transcript, so the round-1 patches were applied by hand.

## Round 1

- backend — approve-with-changes (34k, 65 s). 1 [high] no release, install
  or live check of the session equality: accepted. 2 [low] keep the
  `run-w2` assertion as the empty-session pin: rejected, the test helper sets
  `session_id` "tab"; the fallback has its own test.
- lang:rust — approve-with-changes (44k, 60 s). 1 [high] name the pinned
  `run-w2` assertion: accepted. 2 [medium] test the empty fallback: accepted.
  3 [medium] sleeper order and stale tickets: accepted. 4 [low] print the
  refusal limit: accepted.
- tui — approve-with-changes (26k, 31 s). 1 [medium] `unattributed-*`
  sessions read `unattrib`: accepted, they keep the run id. 2 [low] the scope
  line hides the `greengate slots` change: accepted, tui added. 3 [low] say
  how an agent finds the job id: accepted.
- unix — approve-with-changes (27k, 36 s). 1 [low] keep the run id in the
  command label: rejected, the label is what `slots` fits in 80 columns and
  the setup lease relabels it per step. 2 [low] scope line: accepted (with
  tui 2).

## Round 2

- backend — approve (40k, 28 s). New lows, carried into implementation: the
  Decision log's fallback line should name `unattributed-*` too; the live
  check should say how the slots are filled first.
- lang:rust — approve (49k, 21 s). New low, carried: fill the slot with a
  `greengate slot run -- sleep` from another session before the live check.
- tui — approve (30k, 12 s). No new finding.
- unix — approve (31k, 13 s). Rejection of 1 accepted; no new finding.
