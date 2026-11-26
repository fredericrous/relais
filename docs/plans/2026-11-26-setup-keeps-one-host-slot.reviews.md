# Full reviews: A setup keeps one host slot

## Round 1 (body be9707fbb4da)

- **backend** (opus, 49k, 72 s): approve-with-changes.
  1. [high] A kept slot writes no `admitted` row between steps, so other waiters' 300 s stall clock is not reset. Accepted: setups measured (p50 0.8 s, p95 419 s, max 425 s over 24), recorded in the Decision log.
  2. [medium] The unreachable-coordinator retry in `ask()` would hold the kept slot. Accepted.
  3. [medium] Where the kept slot lives. Accepted through lang:rust 1.
  4. [medium] No signal that a step reused the slot. Accepted: `host_slot_kept` decision.
  5. [low] The verification row was not observable. Accepted.
  6. [low] The picture did not show the give-back. Accepted (applied by hand: the patch was HTML-escaped).
- **lang:rust** (opus, 52k, 71 s): approve-with-changes.
  1. [high] Interior mutability: `Mutex<Option<Lease>>` on `HeavyAdmission`, a `KeptSlot` guard, and `Admitted` stays `Sync`. Accepted.
  2. [medium] Unreachable retry. Accepted (backend 2's patch).
  3. [medium] No tests for disk short, weight change, or error/cancel exit. Accepted.
  4. [low] `"held"` is written in two places, not one. Accepted.
- **tui** (opus, 27k, 30 s): approve-with-changes.
  1. [medium] A failed rename is silent. Accepted.
  2. [medium] The give-back is silent. Accepted: `kept_slot_given_back`.
  3. [low] The reason given for leaving out the tui seat was inaccurate. Accepted.
  4. [low] Add an assertion of no `waiting_for_slot` after step 1. Accepted.
- **unix** (opus, 35k, 38 s): approve-with-changes.
  1. [medium] A signal skips the drop: no `released` row. Accepted: stated in the plan, with a kill row.
  2. [low] A failed rename is silent. Accepted (duplicate of tui 1).

## Round 2 (body 84280d3ae89b)

- **backend** (opus, 54k, 16 s): approve-with-changes. All 6 resolved. New: [low] 7. The give-back reasons and tests do not cover the unreachable coordinator. Carried into implementation.
- **lang:rust** (opus, 57k, 16 s): approve-with-changes. All 4 resolved. New: [medium] 5. Restore the broad `verify::`, clippy and fmt row. Carried into implementation.
- **tui** (opus, 31k, 14 s): approve. All 4 resolved. New: [low] 5. The same reason gap as backend 7. Carried into implementation.
- **unix** (opus, 40k, 20 s): approve-with-changes. Both resolved. New: [low] 1. The same as lang:rust 5. Carried into implementation.
