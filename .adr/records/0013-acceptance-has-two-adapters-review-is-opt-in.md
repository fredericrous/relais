---
id: ADR-0013
status: accepted
decisions:
  - key: acceptance.adapters
    choice: relais keeps acceptance as one interface with two adapters - where amont (greengate) is installed, each profile gate is a command `amont run --stamp <gate>` and its verdict is read back through `amont stamps --json`, so the person's push reuses it; where amont is absent, the profile's commands run plainly and relais judges their exit statuses; relais never requires amont, and a repository's profile states one command per gate it wants stamped (a single `make check` earns no stamp)
    first: true
    reason: one verdict per tree (decisions programme, 2026-11-01) built relais's side of the stamp path, but on 2026-11-10 only relais's own profile used it, so over 11-03 to 11-10 a proven base skipped the baseline once in 33 receipts and every push re-ran the suite (decisions one-verdict-per-tree t11; relais checks-once t03); the person chose "delegate, stay standalone" on 2026-11-10, because a hard dependency would make greengate a requirement for trying relais
  - key: acceptance.semantic-review
    choice: relais runs its semantic reviewer only when a contract asks for it with `review: required`; a run without it is accepted on its checks and criteria, and review happens at implementation-review when the change ships through worktree-task; under `review: required` the reviewer comes with its two guards - the inspect report review judges an `inspect` run's report, and a candidate that edits test or verification inputs is reviewed
    first: true
    reason: since 2026-10-10 review was 38% of relais's spend ($441 of $1,151, checks-once t02), and implementation-review (F4b) then reads the same diff again; the person chose "opt-in reviewer" on 2026-11-10 over keeping two narrow guards and over removing all review (checks-once t05)
  - key: acceptance.baseline
    choice: relais runs no baseline when amont proves the base (every profile gate stamped or attested on the base tree); otherwise no baseline runs before the worker, and a base run happens only after a candidate goes red, on the red checks alone, to tell a failure the worker introduced from one already on the base; the baseline cache still serves that run
    first: true
    reason: outside the relais repository the baseline cache almost never hits, so each run paid a full check before its worker started - on amont 9.1 h of baselines against 3.6 h for every attempt's verification since 2026-10-10, and 0.74 uncached baselines per verified run fleet-wide in the week to 11-10 (checks-once t02, one-verdict-per-tree t11); the person chose "proof, else lazy" on 2026-11-10 (checks-once t08)
---
# 0013 — acceptance has two adapters, review is opt-in, a baseline runs only when needed

## Context

For one change, relais checked the base, checked the candidate, and reviewed
the patch. Then the person's commit and push hooks ran the same checks again,
and implementation-review read the same diff again. One verdict per tree
(decisions, 2026-11-01) built the stamp path: relais runs an amont gate with
`amont run --stamp`, and the push skips it. But only relais's own profile used
that path. Every other profile runs one opaque command such as `make check`,
which earns no stamp. Measured over 11-03 to 11-10, uncached baselines held at
0.74 per verified run, and a proven base was skipped once in 33 receipts.

The relais programme "relais checks once and reviews nowhere twice"
(`docs/programmes/dispatch-and-ledger`) asked what relais should keep. The
person answered three questions on 2026-11-10.

## Decision

1. **Two adapters, one acceptance.** Acceptance (criteria, gaps, receipts,
   holdouts) stays relais's own.
   - **With amont,** a profile gate is `amont run --stamp <gate>`. relais
     reads the verdict back through `amont stamps --json`, as SPEC §10
     already describes. An absent or deferred stamp is a gap.
   - **Without amont,** the same profile commands run plainly. relais has no
     dependency on amont, and there is no adapter interface for a third party
     to implement.
   - **Profiles name one command per gate** they want stamped. Commands that
     are not amont gates stay plain beside them.
2. **The semantic reviewer is opt-in.** It runs only under `review:
   required`. Its two guards come with it: the report review that is an
   `inspect` run's verification, and the review forced when a candidate edits
   test or verification inputs. `runner/review/*` stays as the opt-in path.
   Deleting it is a later choice, once the opt-in rate is known.
3. **Proof, else lazy.** No baseline when amont proves the base. Otherwise
   nothing runs before the worker. A base run of only the red checks follows
   a red candidate, so a failure the worker introduced can still be told from
   one already on the base.

## Consequences

- **What is given up:**
  - An `inspect` run that does not opt in is verified only by its report
    existing.
  - A worker that weakens a test in a run that does not opt in is caught
    only by implementation-review and the push gate.
  - A red base is no longer caught before a worker is paid. It is caught at
    the first red candidate, which ends `needs_decision
    (baseline_failure_not_waived)` as before.
  - SPEC §10's "pre-existing failures are visible from the start" no longer
    holds.
- **Built as ordinary plans, not here:**
  - split the Rust repositories' profiles per gate (checks-once t09);
  - add the default-off review switch;
  - add a base run on demand, which the new-failure filter,
    `guarded_preexisting`, `baseline_resource_exhausted` and the
    identical-to-base reuse are rewired to (checks-once t01).
- **SPEC and ADRs amended by the build, each in the PR that changes the
  behaviour** (checks-once t01):
  - SPEC §4, §5, §6, §9, §10, §11, §17, §18 and §23;
  - ADR-0004 (`needs_review` stops being emitted);
  - ADR-0006 (check);
  - ADR-0010 (the budgets no longer cover a default review);
  - ADR-0011 (its review repair applies only under `review: required`).
  - decisions ADR-0045, whose holdouts cite `llm_review`.
- **Legacy values stay readable:** `State::NeedsReview`, the `Review*`
  reasons, `ReviewRecord` in receipts, and the `review_parts` and
  `verification_snapshots` tables. They are no longer emitted by default.
  `LABEL_POLICY_VERSION` stays 4: the learner reads no review verdict.
- **Measure after the build:** rerun the measurement script of the decisions
  repository's one-verdict-per-tree t11, and checks-once t02's queries. Expected after the build: uncached baselines per verified run well
  below 0.74, and review spend falling to the opt-in share.
