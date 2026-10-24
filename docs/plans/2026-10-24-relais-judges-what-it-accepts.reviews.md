# relais judges what it accepts: full reviews

- **backend R1** (opus-5-5, 47k, 104 s): approve-with-changes.
  - High: change runs have no repair path for an unmet criterion; test detection must use the follow-up's lower bound.
  - Medium: `settle_acceptance` needs a patch input; trim whitespace and add more test forms; a cost estimate before the replays.
  - Low: the learn labels already live in `dataset.rs`.
  - All applied.
- **backend R2** (opus-5-5, 32k, 60 s): approve-with-changes.
  - High: nothing on the change path checks criteria before acceptance.
  - Medium: define what `None` means.
  - Low: a stop rule for the cost estimate.
  - All applied.
- **codex** (gpt-6-astra, high): approve-with-changes.
  - High, applied: use `PreparedCandidate.tree`, not `candidate_commit`, as the lower bound; enforce the test rule before integrated acceptance in `scheduler.rs`.
  - Medium, carried: the verify-only boundary.
- **backend final** (opus-5-5, 26k, 29 s): approve. Carried: verify-only runs keep today's settlement, with a test.
