# Full reviews: a public relais seed command


All reviewers ran on opus-5-5, except codex (gpt-6-astra). plan-panel computed `panel=delta` (backend only) because this plan reuses the previous plan's file. Unix, tui and lang:rust were added because the plan adds a new command line.

**Round 1**

- **backend:** approve-with-changes (36k, 46 s), 6 findings, all accepted:
  - (blocking) atomicity: the existing writers each commit their own transaction, so a temporary ledger is published instead;
  - `created_at` through a fixed clock;
  - `needs_review` and `interrupted` also open decisions;
  - the accepted transition chain and its reasons;
  - the failpoint test;
  - the guard runs before any open.
- **lang:rust:** rework (32k, 40 s), 6 findings:
  - (blocking) atomicity: `_in` variants of the writers proposed; backend's route chosen instead;
  - `created_at`;
  - the guard without `Ledger::open`;
  - canonicalised paths;
  - duplicate run ids refused at validation;
  - decisions.
- **unix:** approve-with-changes (27k, 30 s), 6 findings, all accepted:
  - canonicalised paths;
  - a refusal creates no ledger;
  - exit 1 for I/O;
  - the check-then-act race;
  - failpoint reach;
  - output streams.
- **tui:** approve-with-changes (24k, 31 s), 4 findings, all accepted:
  - the human line on stderr with the command prefix;
  - every bad line, then a summary;
  - the `<stdin>` label;
  - refusals name the next command, and `--json` stdout stays empty on a refusal.

**Round 2**

- **backend:** approve-with-changes (45k, 31 s).
  - Accepted: WAL sidecars left on rename (high); the failpoint is `cfg(test)`, so the test is a unit test with a Drop guard (blocking).
  - Carried: stale wording (medium); SchemaMismatch as exit 2 (low).
- **lang:rust:** approve-with-changes (45k, 43 s).
  - Accepted: `Ledger::close` with `wal_checkpoint(TRUNCATE)` (high); `hard_link` placement (high); a stale `.seeding` file and a Drop guard (medium).

**Backend binding passes**

- **Pass 1** (24k, 46 s), high: a read-only WAL open leaves a `-shm` behind. Resolved by refusing any existing ledger file. Approved on that body (29k, 82 s), with a stale Decision-log wording carried.
- **Codex** (gpt-6-astra, high effort): approve-with-changes.
  - Accepted (high): an invocation-specific staging file, never another invocation's; the `hard_link` as commit point, with a cleanup failure after it only a warning.
  - Carried (medium): duplicate holdout names within a run.
- **Pass 2** (23k, 25 s), high: split the publication and concurrency checks into a writer unit test and a CLI exit-code test. Applied.
- **Pass 3** (21k, 15 s): approve. One low finding carried: name the test-only cleanup failpoint and pause hook.

**Carried into implementation**

- Codex 3: duplicate holdout names within a run, and a synthetic attempt identity.
- Stale wording: "renamed into place" in the Decision log, "one transaction" in Phase 1, and "exits non-zero".
- A `SchemaMismatch` is a refusal (exit 2).
- Name the test-only hooks.
