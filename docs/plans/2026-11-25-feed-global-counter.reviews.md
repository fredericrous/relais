# Reviews: feed the global counter from every settle

Seats: backend, lang:rust, tui, unix (round 1 and 2), architect and po (joined at the final body). All on opus.

## Verdicts

- backend: round 1 approve-with-changes (8 findings, 7 accepted, 1 rejected: `RunConfig` already carries `shared_state`); round 2 approve (blocker: `adds=contract`, applied).
- lang:rust: approve-with-changes, then approve (double booking of unreserved retries; injected clock; own seat only through `settle:<dispatch>`).
- tui: approve-with-changes twice (report lines).
- unix: approve-with-changes, then approve (0600 file with directory fsync; unreadable file kept aside).
- architect: approve-with-changes. High finding to keep `max(global, local)` REJECTED: the ticket's Answer is "daily_spent without its ledger floor"; `EXTERNAL_CLIENT` is `None` and `LocalState::daily_spent` is `None`, so the drop changes nothing in production until the external client ships; the floor is one line to restore under the measured rollback criterion (counter more than 5% below the ledger sum on a UTC day). Not a rework.
- po: approve-with-changes (Gap 5 test and rollback criterion applied).

## Carried into implementation (departures from the body are these)

1. Settles without a reservation are booked only on the reconcile tick; a request sync flushes only entries that have a reservation (architect).
2. A test: a settle without a reservation while the coordinator is at its seat cap is kept (architect).
3. On a failed `gate.settle` the runner emits a `gate_settle_failed` event and one stderr line (tui).
4. The rollback comparison counts known-cost dispatches only, skips the cutover day and names where the ledger sum comes from (backend, architect).
5. The runner's holder reuses `seat_holder`; with `external` and no host name the runner reports and books nothing (an-t17: never a constant). Under `Selection::Local` no file is written and `book_unmanaged_spend` is skipped, so a single node is unchanged (advisor, an-t17 rule). A test: a `LocalState` coordinator writes no file.
6. The `carry_on` hook gap: checked, `book_turn_usage` answers `NoSuchDispatch` for a dispatch not on the ledger, so a hook-admitted agent with no daemon is not metered at all (neither ledger nor counter); the docs say so, no hook test (nothing is booked to observe).
7. Persist only when the list changed; a cap drop counts as a change.
