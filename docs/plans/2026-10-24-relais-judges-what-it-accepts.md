---
status: done
branch: feat/judge-what-it-accepts
repos: [relais]
adrs: [ADR-0006, ADR-0011, ADR-0010]
---
# Plan: relais judges what it accepts, and gets out of its own way

## Review panel

👉 **Decide:** none beyond the one already made (a same-tier repair is not escalation). Approve if relais should hold criteria it was given, and stop blocking on its own bookkeeping.
📍 relais · plan ready · next: phase 1, the gate rule. Panel: backend (opus-5-5), codex (gpt-6-astra). ⚠ The tool computed a delta panel (the plan file name was reused), so there was no Rust seat; backend and Codex covered the Rust work.
**Changed by review:** criteria are settled before acceptance on change and integrated runs (no such check existed); the lower bound for scope, test credit and hints is the merged tree; `None` never meets a criterion.
📄 Full reviews: [2026-10-24-relais-judges-what-it-accepts.reviews.md](2026-10-24-relais-judges-what-it-accepts.reviews.md)
**Verdicts:** backend approve-with-changes ×2 then approve; Codex approve-with-changes, 2 high applied. Carried into implementation: verify-only contracts (also `Kind::Change`) keep today's settlement, and an unreadable diff on a worker change is an error (Codex 3, backend final 1), each with its own test.

## Context

Building relais 0.30.0 (#296) took 14 relais runs. Five relais behaviours turned work that was done into a stop, a wrong count or a false accept. Each was finished by the orchestrator at Opus prices.

1. **The Haiku gate scores a review repair as escalation.** `trial_outcome_of` (`runner/live_trial.rs:425`) sets `accepted_without_escalation = receipt.attempts <= 1`. Under 0.30.0, a replay whose reviewer finds only minor issues is review-repaired on the same model, then accepted in 2 attempts, and so scores a gate failure. Before 0.30.0 the same outcome was a rejection the person could judge a pass.
   - The ledger already counts real escalation apart: `Ledger::escalation_attempted` (`ledger/runs.rs:1078`), from attempts with `phase = 'escalation'`.
   - **The person's decision (2026-10-24): a repair is not escalation.** The 7 recorded trials all had `attempts <= 1`, so their scores do not change.
2. **A criterion that asks for new tests is ticked when the checks pass.** `settle_acceptance` (`verify/mod.rs:809-864`) meets bare-string criteria, and `{evidence: test}` without a name, whenever the report is accepted. That was deliberate.
   - The phase 4 run (run-65e87d6bac101-190c) was accepted with none of its five required tests. It was recorded `corrected`.
3. **A `from_candidate` run is scope-checked against its parent's changes too.** `check_scope` (`workspace/mod.rs:754-793`) diffs `base..candidate`, so every path the parent candidate changed counts as out of scope. Two runs stopped `scope_exceeded` on exactly the parent's own files.
4. **An answered interrupted in-place run keeps blocking its worktree.** `Lease::acquire` (`workspace/inplace.rs:210-235`) refuses on any leftover marker and never reads the ledger. `relais decide --answer salvaged` (`cli/decide.rs:269-286`) never touches the marker, so the next run blocked until a separate `relais resume --retire`.
5. **A follow-up run cannot read-hint a file its parent created.** `fingerprint_hints` resolves hints against `base_sha` (`runner/preflight.rs:499-520`, `context/mod.rs:435-447`), not the merged candidate tree the worker sees.

Out of this plan, noted for amont-agent: a reused plan file name makes `plan-panel` compute a delta panel (no Rust seats on a Rust plan), and the hook then asks for fresh re-binds from every seat.

## 1. A repair is not escalation in the typed gate

- `trial_outcome_of` takes whether the run escalated. Both settle paths compute it with `ledger.escalation_attempted(&run)`:
  - the live path, `live_trial::settle` (`:548-565`);
  - the replay path, `cli/replay.rs:602`.
- `accepted_without_escalation` = accepted and not escalated, whatever `attempts` is.
- Learn labels already use this rule (`learn/dataset.rs:713-737`). A test asserts that a gate trial and the dataset row for the same repaired run agree.
- **ADR-0006 amendment:** "accepted without escalation" means no attempt on the escalation tier. A repair on the same tier, after a failed check or after review findings, is not escalation (the person's decision, 2026-10-24).
- **Tests:**
  - a replay accepted after a `review_repair` → `accepted_without_escalation = true`;
  - one accepted after an escalation → `false`;
  - a single-attempt accept → `true`, as before.

## 2. Declared test evidence means the patch adds a test

- On a `change` run, a criterion declared `{"evidence": {"kind": "test"}}` with no name is met only when the checks pass **and** the candidate's patch adds at least one test.
- "Adds a test" means an added line in the candidate diff, measured from the same lower bound as phase 3 (`Preflight.candidate.tree`, the parent's candidate merged into the target base, under `from_candidate`; otherwise `base_sha`), so a follow-up never gets credit for tests inherited from its parent or target base. After trimming leading whitespace, the line starts a test:
  - Rust `#[test]` or `#[tokio::test]`;
  - TypeScript/JavaScript `test(` or `it(`;
  - Python `def test_` or `async def test_`;
  - Rust `#[rstest]` or `proptest!`;
  - Go `func Test`.
- Otherwise the criterion is unmet, with the gap "declared test evidence, but the patch adds no test". On a change run nothing gates acceptance on criteria today: they are settled only inside `candidate_receipt` (`runner/verification.rs:201-202`), and `CheckCriteriaUnmet`/`CriteriaUnmet` are raised only by `judge_inspection` (`runner/verification.rs:400`, `:419`). This phase settles criteria on the change path before acceptance; a missing-test gap sets `progress.last_failures` and follows `Observation::CriteriaUnmet`, which repairs while `repairs_used < max_repairs` (shared with check repairs, `runner/machine.rs:498`) and then fails. A test covers the case where a check repair already spent the budget.
- `settle_acceptance` (`verify/mod.rs:809`) gains a `patch_adds_test: Option<bool>` (`None` = not a change run or the diff could not be read; a declared-test criterion is then unmet, never met) that its callers compute (`runner/verification.rs:300`, `runner/scheduler.rs:1164`). The `Test{name:None}` arm is split from `LlmReview` (`:861`), and the doc comment at `:788-791` is updated.
- Enforce missing-test evidence before integrated acceptance in `runner/scheduler.rs`, not only while constructing `integrated_receipt`. Route the missing-test condition through the existing bounded integration-repair flow, subject to remaining attempts, repairs and resource budgets; fail when exhausted. An end-to-end decomposed-run test must show that passing checks with no added test cannot seal an accepted root receipt, that a successful repair can accept, and that a still-missing test stops after the allowed repair.
- Bare-string criteria keep today's rule, which is documented and deliberate.
- **The relais skill** (`claude-plugin/skills/relais/SKILL.md`, contract section) tells the orchestrator to declare `"evidence": {"kind": "test"}` on each criterion that asks for new tests.
- **SPEC §10** names the rule.
- **Tests:**
  - a declared-test criterion with a patch adding `#[test]` → met;
  - the same with no test added → unmet, with the gap, and the run goes to repair;
  - a bare-string criterion → unchanged.

## 3. A follow-up run is scope-checked against its own changes

- When the contract has `from_candidate`, `check_scope` (called at `runner/attempt.rs:644` and `runner/scheduler.rs:398`) diffs from `Preflight.candidate.tree` to the new candidate. This is the actual initial merged tree, including the target base's changes; `resolved.candidate_commit` is the original parent candidate and is not this lower bound. Pass the scope lower bound separately rather than changing the verification/export base.
- The parent's own paths then never count. A path this run changes outside its own scope still does, even when the parent touched it.
- Protected-prefix checks are unchanged.
- **Tests:**
  - a `from_candidate` run that changes only in-scope files on top of a parent that changed other files → no `scope_exceeded`;
  - one that edits a parent's file outside its own scope → `scope_exceeded`, naming that path.

## 4. Answering an interrupted in-place run frees its worktree when it safely can

- When `relais decide` resolves an `interrupted` in-place run as `salvaged`, `abandon` or `decided`, it runs the marker's retire step (the same code as `relais resume --retire`, `cli/resume.rs:575-670`) in the "kept" case only: HEAD moved since the run started, so the tree and branch are the person's.
- The marker is removed, and the answer says so.
- If HEAD did not move, the marker stays. The answer prints the `relais resume --retire <run>` command, because a retire would then restore the tree.
- **Tests:**
  - a salvaged interrupted run with HEAD moved → marker gone, the next run in that worktree is not blocked;
  - HEAD unmoved → marker kept, the command printed.

## 5. Read hints of a follow-up run resolve against the tree the worker sees

- With `from_candidate`, `fingerprint_hints` resolves against `Preflight.candidate.tree`, the merged tree already available before `preflight.rs:279`. Pass a separate hint-tree argument so changing hint resolution does not silently change other uses of `assemble_context`'s base argument.
- Without it, nothing changes.
- **Tests:** a successor contract whose read hint names a file only its parent created → no `read_hint_unresolvable`. Also advance the target base after the parent: inherited base changes cause no scope violation, inherited tests provide no new-test credit, and hint hashes match the actual merged tree. Cover conflict-resolution edits against the prepared conflict tree as well.

## 6. Record it

- SPEC: §8 (scope for `from_candidate`), §10 (declared test evidence), §17 (escalation for the gate), the in-place section (decide frees a kept worktree).
- ADR-0006 amended as in 1.
- CHANGELOG, then a relais release. Then the held Haiku replays resume (Part C of the cost plan), one at a time, within the $20 cap. Before that, estimate the cost per replay with a repair (2 attempts, repair effort raised per `route/ladder.rs:12-15`) and how many replays fit in the cap; if the estimate times the replays left exceeds the cap still unspent, run only those that fit and report the shortfall.

## Routing

Each of 1–5 goes to one relais run in a worktree, with its tests declared as `"evidence": {"kind": "test"}`. That puts phase 2's rule to work as soon as it ships; until then I grep each patch for the tests. Phase 6 is written in the session. One PR, one commit per phase.

## Verification

- `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings` → exit 0, with the new tests named.
- `make plugin` → exit 0 (the SKILL.md edit).
- After the release, on live data: `relais recipe evaluate ~/.config/relais/candidates/change-feature-haiku.toml` still reports 7 qualifying trials. The first resumed replay that is review-repaired settles `accepted_without_escalation = 1`.

## Verification record

- **Code checks**, on the final rebased tree (c2c78085…):
  - `cargo clippy --workspace --all-targets -- -D warnings` → exit 0.
  - `cargo test --workspace` → exit 0 (510 s).
  - `make plugin` (PLUGIN_SKIP_OK unset) → exit 0, 332 pass. The only plugin edit is SKILL.md; the plugin tests also carry #300's tests.
- **1, a repair is not escalation** (`runner/live_trial.rs`, `cli/replay.rs`, `learn/dataset.rs`):
  - `a_replay_accepted_after_a_review_repair_is_accepted_without_escalation`;
  - `a_replay_accepted_after_an_escalation_attempt_is_not`;
  - `a_single_attempt_replay_accept_is_accepted_without_escalation`;
  - the gate and dataset agreement test;
  - an end-to-end repaired run through `live_trial::settle` (stores `Some(true)`), and a settle unit test with an escalation (stores `Some(false)`).
- **2, declared test evidence needs a test** (`runner/tests/declared_tests.rs`):
  - `a_declared_test_criterion_is_met_when_the_patch_adds_a_test`;
  - `…_without_an_added_test_takes_one_repair_then_fails`;
  - `a_declared_test_gap_after_a_spent_check_repair_fails_without_a_second_repair`;
  - `a_verify_only_contract_with_declared_test_evidence_is_settled_as_before`;
  - `a_follow_up_gets_no_credit_for_the_tests_its_parent_added`, plus the advanced-base case asserting the gap;
  - `a_decomposed_run_with_no_added_test_stops_after_its_one_integration_repair`;
  - `a_decomposed_integration_repair_that_adds_a_test_can_accept`;
  - an unreadable diff on a worker change ends Interrupted with no receipt.
- **3, follow-up scope** (`runner/tests/scope_floor.rs`, 6 tests):
  - an in-scope follow-up;
  - a parent's file out of scope;
  - an advanced base;
  - a protected path;
  - a verify measured from the base;
  - a decomposed `from_candidate` contract refused.
- **4, decide frees a kept worktree** (`cli/decide.rs` tests):
  - HEAD moved, salvage and abandon: the marker is retired;
  - HEAD unmoved: the marker is kept and the command named;
  - a run that was not in place: nothing said;
  - a corrupt marker, an unreadable HEAD and an unreadable git dir are named, with the answer still recorded.
- **5, read hints of a follow-up** (`runner/tests/verify_kind.rs`, `context/mod.rs`):
  - a hint resolves in the tree it is given;
  - a parent-only file;
  - the hint hash after the target advanced;
  - a hint in a resolved conflict tree;
  - a change without `from_candidate` still blocks.

## Post-release step

After the release, on live data:
- `relais recipe evaluate ~/.config/relais/candidates/change-feature-haiku.toml` still reports 7 qualifying trials.
- The first resumed Haiku replay that is review-repaired settles `accepted_without_escalation = 1`.
- **Replay cost estimate:** an accepted Haiku trial costs $0.55 on average (5 trials, at most $0.62). With a repair (October median $0.20) that is about $0.75. The 7 pool replays come to about $5.50, inside the $20 cap.
- The person checks the pane loader row (#300) after reinstalling the plugin.

## Decision log

- 2026-10-24: Phase 3's scheduler criterion ("the decomposed path measures a `from_candidate` run from the merged tree") cannot be tested. `contract/mod.rs` refuses to decompose a `from_candidate` change, so it was replaced by a test of that refusal.
- 2026-10-24: `decide` stays silent when a run's directory is not on disk. The ledger records the run's directory, not whether it ran in place. A deleted, unpruned worktree's marker is left to `relais resume --retire`. holds-until: the ledger records whether a run ran in place (implementation review delta, finding 9).
- 2026-10-24: Phase 5's manifest records `base_sha` beside hint fingerprints taken from the merged tree. SPEC §7/§8 documents how that tree is rebuilt (phase 5 reviewer, low).
- 2026-10-24: The release carries #300, the pane loader row. It was merged to main after v0.31.0 and is unreleased, so the CHANGELOG names it.
- 2026-10-24: The person approved committing with only the `plugin-test` gate skipped (`git -c hook.skip=plugin-test`), after `make plugin` passes alone on the same tree, when the gate times out under load. It was used on several commits here; every other gate ran.
- 2026-10-24: Two runs were accepted by relais 0.30.0 without their required tests (phase 3 `run-65e920f8510c0`, recorded corrected; the phase 2 gap). Follow-up runs added the tests. This is the defect phase 2 fixes.

## Implementation review

approve-with-changes (round 1: 94k, 191 s; delta: 109k, 27 s).
- Fixed: the unreadable-diff test; `decide` no longer swallows errors; phase 5's conflict and advanced-base tests; phase 1 settled end to end; `decide` returns Ok once recorded; SPEC §17.
- Recorded here: the Verification record, the #300 changelog note, and the deferral's `holds-until`.

<!-- panel: repos=relais reviewers=backend,codex body-sha=f646fb5454cd -->
