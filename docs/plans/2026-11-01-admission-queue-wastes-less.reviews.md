# Reviews: the admission queue wastes less

- **Round 1, backend: rework** (46k, 308 s).
  - [blocking] Requeue contradicts ADR-0010.
  - [blocking] Taking the slot first bypasses fairness (a new dispatch id per try).
  - [high] The plugin launcher is session-scoped.
  - [medium] The successor's own expiry is unstated.
  - [medium] The successor's review is unnamed.
  - [low] Verification is not written as input → expected.
- **Round 1, lang:rust: rework** (38k, 210 s).
  - [blocking] Release-and-retry churns amont's FIFO.
  - [blocking] The plan needs an ADR-0010/0011 amendment.
  - [high] No cancel test while the slot is held.
  - [medium] Timing tests depend on load.
  - [medium] The cap's clock is unstated.
  - [low] Time-of-check race on the free-token count.
- **Round 2, backend: approve-with-changes** (47k, 166 s).
  - [high] A verify successor's baseline strands its candidate.
  - [high] The expiry site has no candidate.
  - [medium] Integration's head cannot be resolved.
  - [medium] Detail plumbing.
  - [low] Refusal text, and `make check`.
- **Round 2, lang:rust: approve-with-changes** (46k, 184 s).
  - [high] Pass the candidate explicitly, at `attempt.rs:904` and `:933` only.
  - [high] Integration cannot be answered this way.
  - [medium] Detail plumbing.
  - [medium] Existing asserts.
- **Codex: rework**, on the `needs_decision` body.
  - [high] A verify run's baseline has no attempt or AttemptContext.
  - [high] Setup and reviewer-admission paths are uncovered.
  - [medium] Reviewer dispatches are not worker dispatches.
  - [low] SPEC §9 and §23.

  All of these were superseded: a read of `candidate::resolve` and the ledger showed the recovery already exists, so the state change was dropped.
- **Final bind 1, backend: approve-with-changes** (46k, 85 s).
  - [high] Reviewer admission at `launch.rs:174`: applied.
- **Final bind 2, backend: approve** (24k, 48 s).
  - Wording slip: "two cases" lists three. Carried into implementation.
