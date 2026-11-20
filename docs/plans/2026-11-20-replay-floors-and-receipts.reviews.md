# Replay floors and receipts — full reviews

Panel by the real `plan-review-*` agent types, 2026-11-20, every seat on opus. An earlier stand-in round, with general-purpose agents playing the roles, produced the body this panel first read (sha `aa88b89a6e61`). That round's record is in `/tmp/t18/plan-r2b.md` and the git history of the landed plan.

- **backend**: round 1 approve-with-changes (50k, 121 s); round 2 approve-with-changes (58k, 26 s); on the final body after rust's high, approve (60k, 16 s); after Codex, approve-with-changes (64k, 28 s).
  - Accepted: a new reader beside `settled_trials_for_recipes` (`learn/comparison.rs:632` keeps the old one); `waiting on trial` on stderr; `settled` defined; n in integers; Phase 2 waits on the 🧑 decisions; observable Verification rows; name `cli/recipe.rs`; Phase 3 merged into Phase 2.
- **lang:rust** (slice: Phases 1–2 and shared sections): round 1 approve-with-changes (36k, 25 s); round 2 approve-with-changes (44k, 26 s).
  - Accepted: an integer floor, `5q ≥ 4s`, with 11/19 → 21 and 16/20, 20/25 pinned; the new reader keeps the guarantee that a corrupt row is an error; the cutoff constant set in the release commit; a serde round-trip for `Findings`. Round 2 high: `s` = q + in-arm errored trials, and a valid repeat enters neither.
  - Rejected: a failed snapshot save (already best effort at `verification.rs:415-441`; withdrawn in round 2); forward-compatibility for an older binary (one binary per machine).
- **tui**: round 1 approve-with-changes (41k, 48 s); round 2 approve-with-changes (47k, 17 s).
  - Accepted: held on stderr with `Blocked`; the receipt-less dry-run line was unreachable; a real `next:` command; honest `no receipt written` wording.
- **unix**: round 1 approve-with-changes (40k, 55 s); round 2 approve-with-changes (48k, 29 s).
  - Accepted: held through `TypedResearchGate` (`cli/recipe.rs:356-358`); `decide --json` keeps one stdout object (`receipt: null`, `receipt_skipped`); the receipt-less replay stays a stderr refusal at `replay.rs:149`.
- **codex** (gpt, high effort): approve-with-changes.
  - High, narrowed: an execution error in a CLI replay (`replay.rs:478-484`) recorded no trial. Fixed by recording one errored trial. Codex's own fix, a pending row inserted before `execute`, was not taken: a killed replay would leave a barrier that never settles.

**Carried into implementation** (medium or low after round 2, no body edit):
1. The Verification row "Piloted gate" is wrong (backend r2#9, rust r2#2, unix r2#5). `change-feature@2` has 11 qualifying trials, so the floor is never read. Expect today's count refusal under both options: `TypedResearchGate` on stderr, exit `Blocked`, no `held:` line.
2. The "No invented receipt" row must state stderr, exit 0, and `receipt_skipped: "review_findings"` under `--json` (backend r2#10).
3. Codex #2: "keeps today's behaviour: it resolves" is wrong for `review_over_budget` with no snapshot, which today is refused (`decide.rs:461`, `approval.rs:142`). Keep that refusal: do not "fix" the code to match the sentence. `no receipt written` prints only on the receipt-less paths that resolve.
4. Record an errored trial only when the arm run failed. An operator interrupt, `storage_full` and ledger errors stay unrecorded (backend, after Codex, #1). If writing the errored trial fails, fall back to today's `unrecorded` line.
5. Each line is at most 80 columns, a user path excepted. End the `candidate refused:` prefix's line before `held:` (unix r2#4). Pin the full stderr string, prefix included (tui r2#6).
6. Reword "It never reports a failure" as "a held read is a refusal that clears as valid trials accrue; it is never recorded as a trial failure" (backend r2#12, tui r2#6).
7. `<p>` is floored, so Context's "58%" reads 57% in the held line (tui r2#5).
8. The receipt-less replay refusal ends with a next command (tui r2, note on #2).
9. The repeat test puts the valid repeat between the 1st and the 15th qualifying trial (backend final #1). Recount whether `@2`'s 11 valid trials are on distinct tasks.
10. Concurrent CLI replays: a running one is invisible as a barrier until it settles.
11. The `--json` field `receipt_skipped` names the stop reason.

### Handoff (2026-11-20)

- The panel ran on the real agent types; this body is reviewed. Next: land it in relais through worktree-task. Use branch `feat/replay-floors` and the file `docs/plans/2026-11-20-replay-floors-and-receipts.md`, with the reviews as a sidecar.
- Then close blue-factory t18 on `prog/blue-factory/t18-replay-floors`. The Resolution:
  - Two of the four ship: receipts (review_findings only) and the Gate-f validity floor.
  - Gate-b is dropped, because an empty candidate already fails as `empty_candidate`.
  - Gate-f's task count is already enforced: TYPED_GATE_TRIALS=15, with dedup by task.
  - The incumbent arm is deferred.
  - The pool is 9 today (all pass the dry run on profile 7030f36d) and at most 10 after receipts ship. A backfill would add 0.
- Follow-up tickets:
  - a dated recount of the pool and the snapshot coverage 7 days after the release;
  - `budget_exhausted` counted as errored, not as an arm failure (5 of 22 errored trials).

