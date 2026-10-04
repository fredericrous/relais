# Full reviews: 2026-10-05-inspect-judged-by-report

**backend, round 1** (approve-with-changes, 49k tokens, 76 s), nine findings, all applied:
1. [high] The reviewer only saw the report, so a fabricated report would pass → it now gets read-only access to the tree, plus a fabricated-report test.
2. [high] Settling did not fail closed → only the bare, `LlmReview` and unnamed `Test` arms change; a missing or bad verdict counts as not met.
3. [high] Reusing `VerificationFailed` would escalate → `Observation::CriteriaUnmet`.
4. [high] The `;` probe step was suspected to be a shape refusal → the plan cites the 2.1.289 measurement and re-measures before pinning.
5. [medium] The fail-closed cases were incomplete → no or unknown id, non-Bash denials and matches in both sets all count as capability.
6. [medium] Check gaps stopped an inspection → gaps are information for inspect.
7. [medium] `ReviewOffOnInspect` would break stored contracts → dropped; the report review is verification.
8. [medium] A was too big for one run, and its scope too narrow → split into A1 and A2, with lifecycle and route in scope.
9. [low] Cost was missing → per-run costs from the ledger added.

**backend, round 2** (approve-with-changes, 62k tokens, 28 s): 1-7 and 9 resolved; 8 not resolved.
- 10 [high] The read-only tool set needs the adapter → `LaunchSpec` parameter, `adapter/**` and `backend.rs` in A2's scope, and both modes.
- 11 [low] A1 alone is not releasable → a note added.

**backend, bind** (approve, 65k tokens, 66 s): 10 and 11 resolved. Two low items, both applied later: measure `--tools` in allowlist mode in the real init check, and the placement of the list note.

**backend, fresh bind 1** (approve-with-changes, 43k tokens, 52 s), four findings, all applied:
1. [high] B's scope missed `backend.rs` (`DispatchResult.permission_denials`, `:573`) → `adapter/**` and `backend.rs` added.
2. [medium] On inspect, an unmet `Check` criterion on a red base would buy the forbidden repair → it goes straight to `Failed`.
3. [low] The allowlist-mode init check → added.
4. [low] The A1 note's placement → fixed.

**backend, fresh bind 2** (approve-with-changes, 44k tokens, 34 s): 1-4 resolved. Two new findings, both applied:
- [medium] `Reason::ALL` counts: 51 after A2, 52 after B.
- [low] The red-`Check` `Failed` path had no named reason → it uses `Reason::CriteriaUnmet`, and the check failure wins over a repair.

**backend, fresh bind 3** (approve, 31k tokens, 21 s): both resolved, no blockers. Two low items went to the person (see Decide).

**After the person's three changes:**
- **backend delta** (approve-with-changes, 68k tokens, 60 s): five findings, all applied: the `settled_via` name collision, `Receipt.kind`, only sign-off gaps in the `NeedsDecision` detail, the outcome order, and B4 plumbing through `Dispatched`.
- **bind** (approve-with-changes, 57k tokens, 54 s): all five resolved. One medium (B4 still passes the scope and protected-input checks; the flag and the classification copy into `Candidate`) and one low (the re-seal writes `settled_via`), both applied.
- **final bind** (approve, 34k tokens, 24 s): both resolved, no blockers. One low item goes to the person: the state a claimed attempt's row finishes in.

Noted for B's run: dropping the per-name deduplication (`adapter/claude.rs:490`, test `:875`) changes runner tests at `:6684`, `:6728` and `:9504`, all inside B's scope.
