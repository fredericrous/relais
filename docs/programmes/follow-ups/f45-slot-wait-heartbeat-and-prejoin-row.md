+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# A queued verification shows a failed heartbeat and a pre-join lock failure

## Question

Filed from relais-modularise t05 (code review, backend #2 and #3, kept as today there). While a verification waits for a host slot, `slot::take` drops the lease heartbeat's `Err` with `let _ =`, so a lease the coordinator already revoked gives no signal. A lock that fails before the line is joined turns the host slots off and returns `Ok(None)` with no journal row, so the day-14 wait percentiles miss that wait. `origin/main` did both the same before t05. Should a failed beat emit one `EnvironmentWarning` per wait, and should the pre-join path write `record(waited_ms, "deferred", Some(WaitEnd::IoError))`?

## Look at

- `crates/relais/src/verify/heavy.rs`, `mod slots`, `take`: the `pause` closure's `gate.heartbeat(lease)` and the `Halt::Unusable` arm.
- `crates/relais/src/verify/heavy/tests.rs`: `a_cancel_while_queued_for_a_host_slot_ends_the_wait_and_leaves_the_line` and `a_lock_that_fails_before_joining_turns_the_host_slots_off_once`, which a fix extends.

## Answer looks like

Either both changes land with a test each (a gate whose heartbeat fails emits one warning per wait; the pre-join failure writes one `deferred` row ending `io_error`), or a Resolution that says why one stays as it is.
