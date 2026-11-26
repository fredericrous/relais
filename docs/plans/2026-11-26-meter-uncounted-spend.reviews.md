# Full reviews: Meter the spend that still misses the counter

## Round 1 (body e89e24cd9684)

- backend (opus, 42k, 55 s): approve-with-changes. 1 medium retry only
  `Unavailable`, state the worst case (accepted); 2 medium a fixed
  `dropped_micros=<n>` token, else the "5% check sums it" claim has no
  reader (accepted); 3 medium each gap names its ceiling,
  `change.a-deferral-names-its-ceiling` (accepted); 4 low `Unknown` counts as
  taken, as today (accepted); 5 low the "green" Verification row (rejected:
  implementation review reads the docs).
- unix (opus, 41k, 41 s): approve-with-changes. 1 medium retry only
  `Unavailable` (accepted, merged); 2 medium worst case 2 x 30 s (accepted;
  skip-on-cancel rejected: a cancelled run's spend still happened); 3 low a
  parseable amount (accepted as backend's token).
- tui (opus, 36k, 50 s): approve-with-changes. 1 high a stderr line before
  the second try, or the run's end hangs 60 s in silence (accepted); 2 medium
  the docs daemon stderr list and the cap-line quote (accepted); 3 low the
  exact final text (accepted).
- lang:rust (opus, 48k, 65 s): approve-with-changes. 1 medium runner tests on
  `FaultyGate` over a real `LocalGate`, read back through `inner.status()`
  (accepted); 2 low count from `SETTLE_TRIES` (accepted); 3 low the extra
  timeout (accepted, stated).

## Round 2 (body b30818e85eae)

- backend (opus, 48k, 20 s): approve. All round-1 findings resolved. Lows:
  a duplicated docs clause; one long Goal line (carried into
  implementation).
- unix (opus, 45k, 15 s): approve. Low: Goal says "tries twice" loosely; `<n>`
  should be the tries made (carried: the code reports the tries made).
- tui (opus, 41k, 18 s): approve-with-changes. Assert the `trying once more`
  line (carried: stderr is not captured by the runner fixture; the try count
  is asserted instead); low "after 1 tries" (carried: the code says
  `after 1 try`).
- lang:rust (opus, 57k, 29 s): approve-with-changes. Medium: assert the
  `dropped_micros=` token in the refused and cap tests (carried, implemented).

Codex: not run (programme ticket agent).
