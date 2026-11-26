# Full reviews: one `--json` error contract

Round 1 (opus), all approve-with-changes.

- backend: learn cycle and trust `Ok(failure)` returns print no object (accepted); text lines on success (Non-goal); verification lacked input -> expected (accepted); homelab file unnamed (accepted); drop tui (rejected: a stream change in a CLI is tui's remit).
- lang:rust: five learn cycle returns not two (accepted); `--log` sends the line, not the object, to the file (stated); no test for trust/doctor (trust test added); doctor partial document (accepted edge).
- tui: seven trust returns and three more learn returns (accepted); text lines on exit 0 (Non-goal); tests for trust and doctor (trust added).
- unix: Busy and Refused returns (accepted); NotIdle text (Non-goal); docs "nothing on stdout" qualified (accepted); clap usage errors and `--log` exceptions documented (accepted); `print_document` BrokenPipe (rejected, pre-existing).

Round 2 (backend, delta): all five round-1 findings resolved. New, medium, carried into implementation and done: a test for the stale-key object shape (`a_stale_grant_key_is_one_error_object_with_its_code`).
