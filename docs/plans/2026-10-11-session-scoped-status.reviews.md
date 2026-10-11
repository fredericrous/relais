# Full reviews: session-scoped status

**Round 1:**
- **backend** approve-with-changes (22k, 18 s):
  - [high] Minimum age and the pid write being atomic.
  - [high] Compare-and-set reap.
  - [med] EPERM and pid namespaces.
  - [med] SIGTERM to a reused pid.
  - [med] Usage booking race.
  - [med] Volume and rollback numbers.
  - [low] Verification as input → output.
  - [low] A daemon log line per reap.
- **lang:rust** approve-with-changes (48k, 131 s):
  - [high] Events from another process collide on seq and can interleave.
  - [high] A bare signal to a recorded pid.
  - [high] A reused pid is never reaped.
  - [high] The daemon's reconcile has no ledger and the daemon exits when idle.
  - [med] Usage races and out-of-order cumulative totals.
  - [med] Every driver entry point, child runs included.
  - [low] An old daemon on a v26 ledger.
- **tui** approve-with-changes (20k, 14 s):
  - [high] Cancel output per case.
  - [high] `--session` required, and an empty result.
  - [med] Plain-text labels and 80 columns.
  - [med] Sanitise the model name.
  - [med] Reap reason as a sentence.
  - [low] Stalled shown as a hint.
- **unix** approve-with-changes (33k, 46 s):
  - [high] Pid start time.
  - [high] There is no SIGTERM handler.
  - [high] The daemon's stdout and seq.
  - [med] Concurrent reapers.
  - [med] Exit codes and the README table.
  - [low] Empty or TTY stdin for `native usage`.

All round-1 findings were accepted and applied.

**Round 2, backend** (36k, 43 s, then 20k, 23 s):
- These were resolved in the body: the re-entry age (`driver_since`), the `transition_if` compare-and-set, and silence never reaping a live pid with a matching start time.

**Carried into implementation:**
1. Back up the ledger before the v26 migration (or add a down-migration), and measure the `native usage` spawn time at p50 and p95.
2. Verification step 3 also accepts "already ended: interrupted".
3. Add a cancel row for a foreign run whose driver is gone: reaped, exit 0.
4. Every driver transition after the start goes through `transition_if`, with a test.
5. The silence rule also requires that no dispatch pid is alive.
6. The entry-point test also asserts `driver_since`.
7. The reported run, `run-65d7dca899789-2d73`, is cleaned up after the release.
