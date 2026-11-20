---
status: active
branch: feat/replay-floors
repos: [relais]
adrs: [ADR-0006]
programme: blue-factory t18-replay-floors
plan-id: p-01M7WKTCVXJJPJT6YHM1Z75WBW
---
# Replay floors and receipts for decision-accepted runs

## Review panel

👉 **Decided 2026-11-20 (the person):** the 80% floor applies only to revisions first trialled after the release (a); a held revision passes at its earliest 80% prefix (a).
📍 relais · 2 phases · 1 release · 2 decisions · next: Phase 1, receipts. Panel: backend, lang:rust, tui, unix (opus-5-5), codex (gpt, high).
**Changed by review:** held is a `Blocked` refusal on stderr, never an exit-0 stdout line; floor in integers (n = 4s − 5q), with `s` = qualifying + errored; a CLI replay's execution error now records an errored trial.
**Verdicts:** backend approve-with-changes, then approve on the final body; lang:rust, tui, unix approve-with-changes; codex approve-with-changes (1 high, narrowed); 16 carried; words 1973 → 2230 (×1.13); remove 0, merge 1.
📄 Full reviews: [2026-11-20-replay-floors-and-receipts.reviews.md](2026-11-20-replay-floors-and-receipts.reviews.md)

## Picture

```
 run stops needs_review ──────────────┐
   review_over_budget   (snapshot: today)
   review_unavailable   (snapshot: today)
   review_findings      (snapshot: NEW, Phase 1)
   inputs_changed, scope_exceeded, crash  (no snapshot, stays so)
                                      │
 relais decide <run> approve ─────────┤ seals receipt from snapshot
                                      ▼
 receipt(base_sha, contract_hash, verification_profile_hash)
                                      │
 relais dataset replay ── profile hash current? ── no → refused (the 33 old)
                                      │ yes
                                      ▼
 arm run (an empty candidate already fails: empty_candidate)
                                      │
                                      ▼
 typed gate: first 15 qualifying, valid/settled >= 80%? (Gate-f, Phase 2)
       no → held (authorises nothing)      yes → streak bar 14/15 as today
```

Receipts reach decision approvals after a review-findings stop; the
validity floor guards what the gate may read.

## Context

The blue-factory programme's paired-replay research (t14) proposed four
additions to relais's replay and typed research gate. Its premise was that
26 of 35 relais feature tasks could not be replayed for lack of a receipt.
This plan measured that again on 2026-11-20, against the live ledger
(read-only), before deciding what ships:

- **Today's pool is 9 replayable tasks, not 0.** Every one of the 9 passes
  `relais dataset replay --dry-run` against a candidate rebuilt from
  today's `relais.toml` (profile hash `7030f36d…`, first sealed
  2026-11-12).
- **The 33 receipt-less tasks are all decision-approved, and all dated
  2026-09-23 to 2026-11-09**, before the current profile hash existed. A
  backfilled receipt would carry an older profile hash and be refused at
  `cli/replay.rs:202` whatever else it held. **Receipts add 0 to the pool
  from the past.**
- **Decision approvals fell from about 25 a day (11-07 to 11-09) to about
  1 a day since 11-10.** Only 1 relais feature run has been
  decision-approved since 11-12. **After receipts ship, the pool is 9 + at
  most 1 = 10 today**, and it grows by roughly one decision-approved task a
  fortnight on top of clean acceptances.
- **Why a decision-approved run has no receipt:** 149 accepted runs went
  through `decision_approved`. 108 stopped for `review_findings`, 28 for
  `review_unavailable` (before snapshots existed), 14 for
  `verification_inputs_changed`, 1 each for `scope_exceeded` and
  `process_crash`. Today only `review_over_budget` and `review_unavailable`
  save a verification snapshot (`runner/verification.rs:408`), so only they
  can seal a receipt at `relais decide … approve` (`runner/approval.rs:21`).
- **Trial validity:** 22 of 46 trials are `errored` (52% valid). For the
  live Haiku candidate (`change-feature@2`, recipe `b8c0…`), 11 of 19
  are valid (58%). Its errors are 3 `blocked_preflight`, 3 worktree
  retirements, 1 `budget_exhausted`, 1 `decision_abandoned`: mostly
  infrastructure.
- **Empty diffs:** no settled trial whose arm run recorded a candidate has
  `candidate_sha == base_sha`. That's 0 of 27.

Outcome: receipts ship for correctness, not for pool size, and one gate
floor ships as a guard. The incumbent arm does not ship.

## Goal

A run accepted by a decision after the review found something carries a
receipt, as truthful as a clean run's. The typed research gate refuses to
read a verdict from a trial set that is mostly infrastructure noise.

## Non-goals

- **No backfill of receipts into the live ledger.** It would add 0
  replayable tasks (see Context) and invent evidence the runs never saved.
- **No receipt for a decision approval whose verification never judged
  the candidate:** `verification_inputs_changed`, `scope_exceeded`,
  `process_crash`, `interrupted`. These stay receipt-less.
- **No incumbent arm re-run in the same session.**
  - It would double the replay cost (~$0.6–2.2 per task) on a pool of 10.
  - `judge()` has no rule that would read the incumbent's result.
  - Revisit it when a paired significance test is adopted or the pool
    exceeds 30.
- **No paired significance test.** The streak bar stays (t14).
- **No new "≥ 15 paired tasks" constant.** `TYPED_GATE_TRIALS = 15`
  (`route/candidate.rs:160`) with dedup by task (`typed_gate.rs:134`)
  already is that floor.
- **No reclassification of `budget_exhausted` from errored to failure.**
  It is filed as a follow-up ticket in the blue-factory programme.

## Code review

- relais: backend, lang:rust, unix, tui

## Behaviour

1. **A receipt for a review-findings approval.**
   - When a change run stops `needs_review` for `review_findings`, it saves
     a verification snapshot first.
     - In `runner/verification.rs` the findings go through `self.decide(…)`,
       which may send the run to a repair attempt instead.
     - The snapshot half of `Engine::stop_for_review` is split out. It is
       called only when `decide` stops the run, and before the
       `needs_review` transition, so the machine decides once and a signal
       between the two never leaves a stop without its snapshot.
     - The decomposed path (`runner/scheduler.rs:1126`) moves from
       `engine.stop` to the same snapshot-then-stop, with
       `integrated_receipt`.
     - Sealing at approve reads a new predicate, which adds
       `ReviewFindings`. The approve path in `cli/decide.rs:457` and `:468`
       switches to it. `seals_from_snapshot` stays only as the
       `review-parts` gate (`runner/successor.rs:489`).
     - `Engine::decide` records the transition itself
       (`runner/engine.rs:202`). So the order becomes: the pure
       `machine::decide`, then the snapshot when it is a stop, then
       `record_decision`.
   - `relais decide <run> approve` then seals that run's receipt from the
     snapshot. The checks are unchanged: bound hashes, no gaps, checks
     accept the candidate.
   - The review findings stay in the receipt as they were recorded, in a
     new `ReviewRecord::Findings { detail, review: Box<ReviewRecord> }`
     variant. The nested record comes from `review_record_of`, so it keeps
     independence, successor-only coverage and `cleared_by`, or each
     part's hashes and verdict. `reviewed()` delegates to it. Older
     receipts still deserialise. There are tests for same-model,
     successor-only and parts reviews.
   - An approval for a run that has no snapshot keeps today's behaviour:
     it resolves and writes no receipt. In text mode `decide` now prints one
     line on stderr: `no receipt written: no verification snapshot for this
     <stop reason> stop`; the exit stays 0. With `--json` it prints no extra
     line: the one stdout object (`cli/decide.rs:52`) gains
     `"receipt": null` and `"receipt_skipped": "<stop reason>"`.
     Older runs are unchanged.
   - `relais dataset replay` therefore finds a receipt on such a run, and
     its dry run prints one stable line on stdout, in the dry-run block:
     `source settled by: decision (review_findings)` or
     `source settled by: runner`. A run with no receipt still stops at
     `cli/replay.rs:149` with `OperationalFailure`; that stderr line gains
     `(source settled by: none)`.
2. **Gate-f: a validity floor.**
   - `settled` is `qualifying` plus every in-arm errored trial, errored
     retries on the same task included (as Context's 11 of 19 does);
     `qualifying` stays one per task, and a valid repeat on a task already
     counted enters neither (tested: 15 qualifying plus one repeat is 15/15). In-arm means the same recipe, type and
     design, with no first-model condition, whatever the outcome.
   - It counts up to and including the 15th qualifying trial.
   - A passing read is final: later trials never move it.
   - The floor and the first-15 streak are both read only over a closed
     prefix. A closed prefix is a run of trials, in `(created_at,
     trial_id)` order, that ends before the arm's earliest unsettled trial.
     A pending trial is a barrier, never a denominator entry. This needs
     a new reader beside `settled_trials_for_recipes` (`ledger/trials.rs:692`)
     that returns unsettled rows too and keeps its guarantee that a corrupt
     row is an error (tested); the existing reader keeps its contract for
     `learn/comparison.rs:632`. Evaluation stays read-only. While a pending
     trial bars the read, the refusal on stderr names it:
     `waiting on trial <id> (pending since <date>)`.
   - A CLI replay whose execution errors (`cli/replay.rs:478-484`) now
     records one errored trial instead of none, so infrastructure noise
     reaches the denominator. Tested through `relais dataset replay`.
   - A held read authorises nothing. The floor is then read at the
     earliest trial prefix, from the 15th qualifying trial onward, where
     `q/s ≥ 0.80`. It is computed from the ledger alone, so a later trial
     never moves a pass, with or without a `record`ed promotion. The streak bar is still read from
     the first 15 qualifying trials.
   - At read time (15 qualifying), the gate passes only if
     `qualifying / settled ≥ 0.80`. Below that, it authorises nothing: it is refused through
     `CandidateRejection::TypedResearchGate`, like a 13/15 read today, and
     prints on stderr after `relais recipe evaluate: candidate refused:`
     (`cli/recipe.rs:356-358`):
     `held: <q>/<s> valid (<p>%), below the 80% floor; needs <n> more valid trials`.
     `<n>` is the fewest further valid trials that reach 80% if they all
     pass: n = 4s − 5q. Every floor test is integer arithmetic, pass iff
     5q ≥ 4s (an f64 `0.8 × s` gives 22 for 11/19, where 21 is right), and
     `<p>` is floored. Tests pin 11/19 → 21 and the exact-80% boundary
     (16/20, 20/25). A second line names a command that exists:
     `next: re-run relais recipe evaluate <path> after <n> more trials settle`.
   - The output is 80 columns or fewer and has no colour. The exact string
     is pinned by a test. The exit code is `CliOutcome::Blocked`, as for
     every gate refusal `recipe evaluate` prints today
     (`cli/recipe.rs:358`); a test asserts the stderr line and that code.
   - It never reports a failure: the revision becomes readable once
     enough valid trials accrue.
   - Revisions whose first trial predates the release follow the 🧑
     decision below.

## Phases

- [x] Phase 1 — receipts for review-findings approvals:
  `crates/relais/src/runner/scheduler.rs`, `crates/relais/src/cli/decide.rs`,
  `crates/relais/src/runner/successor.rs`, `crates/relais/src/verify/mod.rs`,
  `crates/relais/src/runner/engine.rs`,
  `crates/relais/src/runner/verification.rs`,
  `crates/relais/src/runner/approval.rs`, `crates/relais/src/cli/replay.rs`,
  `docs/SPEC.md`. Tests:
  - a review-findings stop saves a snapshot, before its `needs_review`
    transition;
  - review findings sent to a repair attempt save none;
  - `review-parts` on a review-findings stop is still refused;
  - approve, driven through the `relais decide` entry point, seals an
    `accepted` receipt with `ReviewRecord::Findings`;
  - an older receipt still deserialises, and a `Findings` receipt
    round-trips through serde;
  - a `review_findings` approval with no snapshot resolves, writes none,
    and prints the `no receipt written` line on stderr, or
    `receipt_skipped` under `--json`;
  - the dry run prints each of the two stdout source lines, and the
    receipt-less refusal prints its stderr line with `OperationalFailure`.
- [x] Phase 2 — Gate-f validity floor, started once both 🧑 decisions
  below are answered. It amends ADR-0006
  (`routing.task-type-research`, "at least 14 of the first 15 qualifying"):
  the floor is added to that record's decision, in its own commit, with
  `aval heads --write`. Files:
  `.adr/records/0006-a-change-runs-below-implementation-only-on-evidence.md`,
  `crates/relais/src/learn/typed_gate.rs`,
  `crates/relais/src/ledger/trials.rs`,
  `crates/relais/src/cli/replay.rs`,
  `crates/relais/src/route/candidate.rs`, and the `recipe evaluate` output
  in `crates/relais/src/cli/recipe.rs`, and the release notes in
  `CHANGELOG.md` for the one release. Tests for these cases:
  - 15 of 18: passes;
  - 15 of 19: held (78.9%); 16 of 20 clears the floor;
  - 15 of 20: held, with the pinned line and exit code;
  - 15 of 18, then 6 errored trials after the read: still passes;
  - the oldest trial still pending, then 15 qualifying and 3 errored: no
    pass yet; that oldest trial settling errored gives 15 of 19, held;
  - held at 15 of 20, reaching 80% at 20 of 25, then 5 errored trials:
    still passes;
  - an in-flight revision under the chosen grandfathering.
- [x] 🧑 decision (a, 2026-11-20): does the 80% floor apply to a revision already mid-gate
  (`change-feature@2`, 11 of 19 valid, 58%)?
  - **(a) Recommended: only revisions whose first trial is after the
    release.** `@2` keeps today's bar, and a stricter rule never moves
    the goalposts mid-count. The cutoff is a named constant in
    `route/candidate.rs`, compared with the revision's first trial's
    `created_at`. It is set in the release commit, and a test pins the
    boundary.
  - (b) Every revision. With 8 errored trials already, `@2` stays held
    until it has 32 valid trials, which is 40 settled even with no further
    error.
  - (c) Every revision, with errored trials before the release left out
    of the denominator.
- [x] 🧑 decision (a, 2026-11-20): once a held revision reaches the floor, does it pass?
  - **(a) Recommended: yes.** It passes at the earliest prefix that
    reaches 80%, and that pass is final.
  - (b) No. A held read is final, and the revision needs a new revision
    number to be read again.

## Decision log

- 2026-11-20 — Two of the four ship: receipts (narrowed to
  `review_findings`) and Gate-f's validity part. Not shipped:
  - the incumbent arm;
  - Gate-f's task count, which is already enforced;
  - Gate-b, because a change's empty candidate already fails as
    `empty_candidate` (`runner/attempt.rs:1340`), and 0 of 27 trials had
    one.

  The reason is the pool measurement in Context.

## Verification

| Check | Input | Expected |
|---|---|---|
| Receipt sealed | `cargo test -p relais approval::` plus a new runner test driving a `review_findings` stop then `decide approve` | the receipt row exists; its outcome is `accepted`; its findings are kept |
| No invented receipt | the same test with a `review_findings` stop that saved no snapshot | the decision resolves, writes no receipt row, and prints the `no receipt written` line |
| Gate-f | `cargo test -p relais typed_gate::` | 15/18 passes; 15/19 and 15/20 are held; 16/20 clears the floor; a pending trial is a barrier; nothing is authorised when held |
| Piloted replay | `relais dataset replay --dry-run --task task-0fb5e5ecf89e059c --recipe <rebuilt candidate>` from the worktree build | prints `source settled by: runner` and the profile hash `7030f36d…` |
| Piloted gate | `relais recipe evaluate <rebuilt candidate>` from the worktree build | under (a): `change-feature@2`'s count and no `held:` line; under (b): `held: 11/19 valid (57%), below the 80% floor; needs 21 more valid trials` |
| Live snapshots | 7 days after release: compare the ledger's review-findings stops with the snapshot rows where `stop_reason = review_findings`, and count `verification_snapshot_failed` steps | one snapshot per stopping run; 0 failed steps |
| Full suite | `greengate run` at commit | exit 0, and the new tests named in Phases 1–2 appear in its run list |

<!-- panel: repos=relais reviewers=backend,lang:rust,tui,unix,codex body-sha=6acb9e15baf2 -->
