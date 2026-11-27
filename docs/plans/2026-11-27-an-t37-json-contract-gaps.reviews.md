# Full reviews: an-t37 json contract gaps

## Round 1 (all approve-with-changes, opus)

- tui: busy exits 20; `next_due_at` (rejected: `Due` holds words only, follow-up); `--log` empty stdout named; not-due printed at two sites.
- backend: exit codes wrong (20, 1, 1, 1); reuse the in-process `Daemon`; name the homelab ticket; observable Verification rows; no accessor on `Due`; drop the tui profile; removed-cwd spawn.
- po: exit codes; not-due at two sites; SPEC §17 and `outcome.rs` docs; stamp wording.
- unix: short temp root for the socket; exit 20; removed-cwd spawn via `sh -c`; `write_object`, not `println!`; `--log` leaves stderr empty.
- lang:rust: Pong skew fake bound before spawn (else a real daemon leaks); `Scratch` on `short_temp_dir` and a cwd variant.
- architect: reuse `Daemon`, fake only where unreachable, replies from `Response`; README and SPEC docs; stamp and log wording.

All accepted except `next_due_at`. Recorded by hand (the ledger could not find the agent transcripts).

## Round 2

- po: approve. architect: approve.
- backend, rust, tui, unix: approve-with-changes; every round-1 finding resolved.

## Carried into implementation

1. `write_object` is private: make it `pub(crate)` (`decision_record.rs`) and say any other write error in one stderr line (backend, tui, unix, architect, rust).
2. One `CycleOut` method writes the object, bare to the `--log` file, else `write_object`, from both deferred arms (rust).
3. Test 4's `end_training` count is asserted on the fake coordinator, since the real `Daemon` frees the lease on pid liveness too; a second policy case runs on the real `Daemon` (rust).
4. Verification gets rows for the coordinator arms, `doctor` and `trust` (backend).
5. Not-due with `--json --log f`: one bare object in `f`, stdout empty (tui).
