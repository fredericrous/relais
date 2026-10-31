# Relais runs on the homelab: full reviews


- **architect**: approve-with-changes, 39k tokens, 52 s. Findings:
  - [high] the sandbox loop needs its own ADR-0003-style record;
  - [high] no ticket covers remote heavy checks;
  - [medium] the baseline must be measured before anything moves;
  - [medium] a `repos` value on every ticket;
  - [low] t08 blocked by t01.
- **unix**: approve-with-changes, 27k, 36 s. Findings:
  - [medium] the frontier command needs a directory and an id comparison;
  - [medium] the remote process model: cancel, orphan reaping, exit mapping, lease TTL;
  - [low] check exit codes are fine (rejected: no edit).
- **tui**: approve-with-changes, 26k, 44 s. Findings:
  - [high] a visibility ticket;
  - [high] a failure/fallback ticket whose messages say what to do next;
  - [medium] status shows the placement;
  - [medium] the review and decide steps fold into t02.
- **po**: approve-with-changes, 26k, 52 s. Findings:
  - [high] the destination needs a target and a baseline;
  - [high] a first-slice ticket;
  - [medium] a fallback ticket;
  - [medium] list gVisor/Kata as out of scope (rejected: gVisor may answer t10);
  - [low] the review and decide steps fold into t02.
- **rust**: approve-with-changes, 35k, 72 s. Findings:
  - [medium] `blocked_by` uses full stems;
  - [high] a baseline ticket;
  - [high] the heavy-check lease model, and t02 blocked by t04 and t12;
  - [low] the exact frontier set;
  - [low] the model choice under ADR-0001 goes into t01.
- **backend**: three reviews.
  - Round 1, approve-with-changes, 30k, 72 s. Findings: [high] baseline; [high] failure modes; [medium] dependencies; [medium] ADR-0003 envelope; [medium] failed-run handling; [medium] exact frontier; [low] full ids.
  - Round 2, approve-with-changes, 31k, 37 s. Findings: [high] the frontier at merge includes t13; [high] the t13 sampler spec; [low] each run in its repo.
  - Final, approve, 28k, 25 s. Two notes carried into implementation: copy the holdout line into t14's ticket file; whichever of t04 and t14 resolves second aligns with the first.
- **codex**: approve-with-changes. Findings:
  - [high] amont rejects `prog/` branches (verified at `vocabulary.rs:81`);
  - [high] remote verdicts are bound to the tree and its provenance;
  - [high] the ADR-0045 holdout boundary.

