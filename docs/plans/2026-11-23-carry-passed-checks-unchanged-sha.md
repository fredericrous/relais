---
status: active
branch: feat/carry-passes-unchanged-sha
repos: [relais]
adrs: [ADR-0013, ADR-0009]
plan-id: p-01M82WKB2VH4EHN6GQHQEKQGKM
---
# Carry passed checks to an unchanged sha

## Review panel

none — approve if carrying the passes after the last failure saves about half of t27's 58.6 run-min with no false verdict.
📍 relais · 4 phases · 1 release · next: Phase 1. Panel: backend, lang:rust, lang:typescript, tui, unix (opus).
**Changed by review:** only passes after the last failure are carried (a command may read an earlier one's output); a typed `check_carried` event and pane case; `(carried)` report row.
**Verdicts:** 5 approve on the final body; 1 blocker fixed (tui); 13 carried into implementation; words 1454 → 2089 (×1.44, blocker and wrong-statement fixes); remove 0, merge 0.
📄 Full reviews: [2026-11-23-carry-passed-checks-unchanged-sha.reviews.md](2026-11-23-carry-passed-checks-unchanged-sha.reviews.md)

## Picture

A repair that hands back the same sha re-runs only what failed there.

```
attempt 1, sha S      lint: fail   make: pass   test: pass
        |
   repair returns S (unchanged, not in place, same inputs)
        |
        v
attempt 2, sha S      setup, then lint only
                      make, test carried (carried_from = attempt1)
        |
   sha differs, or in place, or inputs differ
        v
                      every check runs, as today
```

Programme ticket: blue-factory
[Reuse the passed checks of an unchanged candidate](https://git.daddyshome.fr/fredericrous/blue-factory/src/branch/main/docs/programmes/blue-factory/t32-reuse-passes-unchanged-candidate.md),
built on t27's verdict (54 of 327 repairs returned the same sha; re-running
the checks that had passed on it cost 99.6 min over 16 runs and found
nothing; re-running the failed ones turned 5 runs around).

## Goal

When a repair or an escalation returns the sha the previous attempt
verified, relais runs again the checks that failed there and every check before the last of them, with the
profile's setup, and carries the passes after it. A carried pass is marked as
such in the receipt and in `check_timings` (`cached = 1`, naming the
attempt that ran it), so `decide`, `relais report` and the learner never
read it as a fresh run. A changed sha re-runs every check, as today.

## Non-goals

- The `same_failure_recurrence` stop (`runner/machine.rs:535`) and the
  `same_failures` comparison are unchanged.
- No carry across runs, and none to a different sha, even one whose tree
  differs only outside the checks' inputs: that is the baseline cache's
  job (SPEC §18), read-only since ADR-0013.
- The candidate-identical-to-base reuse (`CandidateIdenticalToBase`) is
  unchanged and keeps precedence.
- Holdouts and the semantic review are not carried (see Behaviour).
- No retry policy change for the re-run failed checks: they run under
  their declared `retry` as any check does.

## Code review

- relais: backend, lang:rust, lang:typescript, unix, tui (the report's `(carried)` row, the `check_carried` event and the pane's carried checks change)

## Behaviour

**When a pass is carried.** At verification of attempt N (N ≥ 2), relais
carries the passes of the last verification that ran checks, when all of
these hold:

1. the candidate's sha equals the sha that verification ran on
   (`Progress` keeps it keyed by sha, set only when the profile's commands
   actually ran: a setup gap, a lease stop or a cancel leaves nothing to
   carry);
2. the candidate is not identical to the base (that path keeps its own
   reuse);
3. the run is not in place (`Workspace::InPlace`, `self.inplace`). An
   in-place run seeds each verification tree from the person's checkout
   (`workspace_ops.rs` `task_tree`), which someone other than the run can
   be writing while it runs (`setup_gate.rs` `SeedWait::WritersEnded`
   says so), and no event marks such a write. The two same-sha flips t27
   could not explain (amont-agent `make@c1c4713f`) were in place. So an
   in-place run re-runs every check, as today, and says why in a
   `checks_not_carried` decision event;
4. the verification inputs are the same. Within one run the profile, the
   authority and the resolved holdouts are fixed at preflight, and
   `verification_inputs_changed` is the candidate's diff against the base,
   so for the same sha it is the same list by construction. The guard is
   still explicit: the carry stores the profile's fingerprint (its
   commands and setup, hashed) and `verification_inputs_changed`, and a
   difference in either re-runs everything with `checks_not_carried`
   naming it.

**What runs.** The profile's setup runs in a fresh verification worktree,
as for any candidate, then the commands whose last outcome on that sha
failed, and every passed command before the last of them: a profile's
commands run in order in one tree (`verify/mod.rs` `run_indexed_within`),
so `test` may read what `make` built. Only the passes after the last
failure are carried. The re-run set is a prefix of the profile, run with
`verify::commands_labelled` + `run_indexed_within` and logged under each
command's own profile index, `attempt<N>-cmd<i>`. The carried outcomes
and the fresh ones are merged in profile order, so the receipt's check
list and the log stems keep the order and indices of a full run. When
every check passed last time (the previous attempt failed on its review
or on a holdout), no command runs, but the worktree is still made: amont's
inventory, stamp answers and gate coverage, and the holdouts, read the
candidate's tree as they do now; the setup still runs there, because
amont's stamp and gate answers read the set-up tree. The holder match in
`verify_candidate` gets an explicit `Carry` arm that always makes the
worktree.

**What is never carried.**

- Holdouts run again in full: they are the independent evidence
  (ADR-0045), the holdout stop reads this attempt's outcomes, and t27's
  99.6 min counted checks only.
- The semantic review runs again when the routed level asks for one: a
  review that sent the run back is exactly what the repair answers.
- A failed, timed-out or unrecorded check is never carried; a flaky pass
  (failed, then passed on its retry) is a pass and is carried, keeping its
  `flaky` mark, and is never red.

**How a carried pass is named.**

- Receipt: `CheckOutcome` gains `carried_from: Option<String>` (the log
  prefix of the attempt that ran it, `attempt1`), skipped when absent, so
  every receipt written before reads and writes byte-identically. Its
  timing (and each retry's) is `cached: true`, as `CheckOutcome::reused`
  already does; its `log_path` and `log_sha256` stay the original run's.
  A chain (attempt 3 on attempt 2's sha, itself carried from attempt 1)
  names `attempt1`, the attempt that ran the command, never a carry.
- Ledger: `check_timings` gains a nullable `carried_from TEXT` column
  (schema v49, expand-only); a carried row is written under the carrying
  attempt's phase with `cached = 1` and `carried_from` set. The check-log
  evidence row is written again for the carrying attempt (the same rule
  as the base reuse); the JUnit report row is not, since it was recorded
  when the check ran.
- Events: one `checks_carried` decision per verification that carried,
  naming the labels carried, the labels run, and the source attempt.
  Reason text, at most 80 columns, label lists cut to the first three
  then `+N more`: `carried 2 passes from attempt1 (make, test); re-ran
  lint`. `checks_not_carried` reads `re-ran every check: <cause>`, where
  cause is `in-place run`, `profile changed` or `inputs changed`.
  The reason is for people to read and is never parsed. Each carried pass
  also sends a `CheckCarried { label, from }` event (additive in
  `protocol.rs`, in profile order, after the last re-run
  `check_ended`, so the pane keeps the order of a full run), and the pane builds a verifying step's carried checks
  from those events, never from the decision's reason.
- `relais report` splits timings by `cached` and prints `(reused)`; it
  also groups on `carried_from IS NOT NULL` and prints `(carried)` for
  those rows, so a carried pass is never counted as a base reuse, and its
  copied retries are not counted as new ones (`ledger/check_timings.rs`,
  `report/mod.rs`, with a test of the rendered line). `decide` and the learner read receipts
  and labels, not timings; a carried check reads as reused through
  `carried_from` and `cached`.

**Failure accounting after a carry** is unchanged: `red` is the failed
labels among the merged outcomes (carried passes are never red), so the
repair failures, `same_failures` and the on-demand base run see exactly
the failures they would see after a full run whose passes held.

## Phases

- [ ] Phase 1 — receipt and ledger shape (all 25 `CheckOutcome` literals in `verify/mod.rs`, `verify/stamps.rs`, `verify/holdout_run.rs` updated; serde default + skip): `crates/relais/src/verify/mod.rs`
  (`carried_from`, `CheckOutcome::carried(from)`, a carried receipt
  fixture beside the pinned `PRE_FIELD_RECEIPT_*`/`TIMED_RECEIPT_6`),
  `crates/relais/src/ledger/check_timings.rs` (`CheckTimingRow.carried_from`),
  `crates/relais/src/ledger/schema/migrations.rs` (v49),
  `crates/relais/src/runner/evidence.rs` (write the column; the
  `INSERT OR REPLACE` names it, so a replaced row keeps it),
  `crates/relais/src/report/mod.rs` (a `(carried)` row apart from
  `(reused)`), `crates/relais/src/protocol.rs` (`CheckCarried`),
  `claude-plugin/hooks/timeline.ts` (a `check_carried` case beside
  `check_started`, building a non-running `Check` with `exit: 0` and a `carried` field, leaving `detail` and `wait` alone; an older pane skips the kind in its `default:` arm), `claude-plugin/hooks/pane.ts` (renders `carried from <attempt>`) and the `Check` type.
- [ ] Phase 2 — the carry: `crates/relais/src/runner/spend.rs`
  (`Progress.last_verified`: sha, phase, profile fingerprint, inputs list,
  outcomes), `crates/relais/src/runner/verification.rs` (the `reuse`
  argument becomes an enum `CheckReuse { None, Base(&[..]), Carry(..) }`;
  the carry makes a worktree, runs setup, runs the prefix up to the last failed command, merges by
  profile index; junit recorded per outcome: fresh ones yes, carried ones no, replacing the call-level `reuse.is_none()` gate at line 906),
  `crates/relais/src/runner/attempt.rs` (decide the carry next to
  `unchanged`, store `last_verified` after the commands ran, emit
  `checks_carried` / `checks_not_carried`).
- [ ] Phase 3 — tests: the timeline/pane test file under `claude-plugin/` (carried check not running, order, no-step no-op, unknown kind skipped), `crates/relais/src/runner/tests/mod.rs`
  (scenarios below), `crates/relais/src/verify/mod.rs` (receipt round
  trips), `crates/relais/src/ledger/check_timings.rs` (column).
- [ ] Phase 4 — docs and release: `docs/SPEC.md` (§10 verification, the
  reuse paragraph by the baseline cache text), `CHANGELOG.md` (unreleased),
  then a release through `tag-release`.

## Decision log

- 2026-11-23 — In-place runs carry nothing, because their seed source is a
  tree the person can write mid-run and no event marks it; the ticket's
  "re-seeded in place" has no code event to key on (searched
  `workspace_ops.rs`, `setup_gate.rs`, `home_drift.rs`).
- 2026-11-23 — Holdouts and review run again, because they are the
  independent evidence and t27's minutes are in checks.
- 2026-11-23 — The inputs guard is a stored fingerprint compared at the
  carry, tested as a predicate: no end-to-end case can change inputs on
  the same sha within one run today.
- 2026-11-23 — Only the passes after the last failed command are carried
  (backend, lang:rust), because a later command may read what an earlier
  one built. Measured on the ledger's same-sha pairs with timings: most
  profiles are one `make check`, where nothing can be carried either way;
  17b36's case (lint failed, then `make` waited 31 min) is still carried.
  Counted from `runs/*/events.jsonl` (`verification_started` blocks, same
  candidate on both sides): t27's 16 pairs re-ran passes for 58.6 min of
  run time (99.6 with queue) in 10 pairs; passes after the last failure
  are 30.1 min in 5 pairs, about half. A per-command independence opt-in
  could widen it if the recount shows less.

## Verification

- Unit/scenario tests (`cargo test -p relais`), each red before the
  change where it asserts new behaviour:
  - repair returns the same sha after the first of two checks failed (the second passed): only the
    failed command runs (a counting fake command), the passed one is in
    the receipt with `carried_from = "attempt1"` and `cached` timing, and
    `check_timings` holds the carried row with `cached = 1`,
    `carried_from = 'attempt1'`;
  - repair returns a different sha: every command runs, nothing carried;
  - same sha, `make` passed then `test` failed: both run again, nothing
    carried;
  - a carried flaky pass keeps `flaky`, and is not in `red` or
    `same_failures`;
  - in a carry, the re-run command's JUnit row is written and the carried
    one's is not;
  - re-writing a carried `check_timings` row keeps `carried_from`;
  - `relais report` prints the carried label as `(carried)`, one run, and
    the `(reused)` line of a base reuse is unchanged;
  - five carried passes: five `CheckCarried` events in profile order, the
    pane step lists all five, and the reason line is 80 columns or fewer
    with `+2 more`; the new event round-trips through `protocol.rs`;
  - in-place run, same sha: every command runs, `checks_not_carried`
    names in-place;
  - inputs predicate: a different profile fingerprint or inputs list
    refuses the carry;
  - chain of three attempts on one sha: `carried_from` names `attempt1`;
  - same failure on the unchanged sha still ends `same_failure_recurrence`
    (regression);
  - all checks passed, review sent it back, same sha: no command runs, the
    worktree is made, holdouts run;
  - old receipts (`PRE_FIELD_RECEIPT_*`, `TIMED_RECEIPT_6`) round-trip
    byte-identically; a carried receipt round-trips;
  - migration v49 applies on a v48 ledger, existing rows read
    `carried_from` NULL.
- Piloted run: a scratch repository with a two-command profile whose
  first command fails on its first invocation only (a marker file outside
  the tree; first, because a failure last re-runs both under the prefix
  rule), and a contract whose repair returns the same sha; `relais run`
  then `relais report` prints the failing label once (run) and the other
  with `(carried)`, and the run's events hold one `checks_carried`
  decision naming `attempt1`.
- `make check` on the branch head: exit 0, output linked in the PR.
- Follow-up measure, filed in the blue-factory map: two weeks after the
  release, the minutes re-verification spends on unchanged candidates,
  counting only `cached = 0` rows, against t27's 99.6 min over 16 runs
  (6.2 min a run). It works if run minutes spent re-running passes on
  unchanged candidates fall to about half per pair (t27: 58.6 run-min,
  30.1 of them after the last failure).

<!-- panel: repos=relais reviewers=backend,lang:rust,lang:typescript,tui,unix body-sha=dfb5bd60c8ae -->
