---
status: active
branch: feat/inspect-judged-by-report
repos: [relais]
adrs: []
---
# relais: judge an inspection by its report, and let a worker recover from a shape refusal (#105 follow-up)

## Review panel

👉 **Decide:** approve if your three changes now fit the code. A blockage claim after shape-only refusals still passes the scope and protected-input checks, then goes to repair. The inspection sign-off path is `NeedsDecision` with only sign-off gaps, then a re-seal by `kind`. The settlement source is recorded as `settled_via`, falling back to today's rule on old receipts. One optional low item: a claimed attempt's row is written `verifying` before it is routed to repair; by default B finishes it as `repairing`, with a test assertion.
📍 relais · plan reviewed · next: open the issue, land this plan as the first commit, then run A1. Panel: backend.
**Changed by review:**
- the reviewer checks the report against the tree with read-only tools, and fails closed;
- `CriteriaUnmet` never escalates;
- `review: off` stays loadable;
- A is split into A1 and A2, with adapter scope for A2;
- B's scope includes `backend.rs`;
- on inspect, a red `Check` criterion fails without buying a repair.

📄 Full reviews: [2026-10-05-inspect-judged-by-report.reviews.md](2026-10-05-inspect-judged-by-report.reviews.md)
**Verdicts:** backend: rounds 1 and 2 approve-with-changes, then three fresh binding passes, all applied (approve, approve-with-changes, then the final one in Full reviews). The hook binds only freshly launched reviews.

## Context

On 2026-10-04 another session ran 11 sandboxed inspect tasks (`kind = inspect`, "Report the repository's top-level layout.", one per repo, Claude Code 2.1.289). Five were accepted. **Four failed, and every one had already delivered its report on attempt 1** (`attempt-1-result.txt` holds the full layout). They all followed the same chain:

1. **The base was already red.** For example, application-landscape has 1 failing vitest. An inspect candidate is the base tree, so it inherits the red check: `candidate_identical_to_base`, then `behavioral_failure {"failures":["npm@…"]}` (`runner/mod.rs:2559-2569`, `machine.rs:287-299`).
2. **relais sends it to repair anyway.** The repair addendum (`runner/mod.rs:4109-4116`) tells the worker to "fix only the failures": failures in a tree it may not touch.
3. **The haiku worker, with nothing to fix, misbehaves** (also reported by the session that ran them):
   - it writes report files (`LAYOUT.md`, `TOP_LEVEL_DIRECTORIES.txt`) → `scope_exceeded`;
   - it runs `pnpm install` and the store lands in the worktree (`.pnpm-store/v10/…`) → `scope_exceeded`;
   - it tries a heredoc into `$TMPDIR`, which the harness refuses ("A file redirect in this command can't be checked before it runs"). The worker "produced nothing", so the run ends `blocked (permission_denied)` (`runner/mod.rs:2461-2488`).

What the code does today (explored 2026-10-05):
- **Nothing ever judges an inspect report.** It is stored as `EvidenceKind::WorkerResult` (`runner/mod.rs:2262-2272`, "for an inspect task it is the whole deliverable (SPEC §4)") and never read again. The reviewer's prompt (`review_prompt`, `:3514`) is written for a patch and never sees the report.
- **Criteria are "met" only when every check is green** (`verify/mod.rs:570`, `settle_acceptance`; `accepted()` at `:155`). So an inspect run on a red base can never pass, whatever it reports.
- **The refusal reason never reaches the runner.** The adapter keeps only `Bash(<120 chars>)` per denial and drops `tool_use_id` (`adapter/claude.rs:533-559`). The reason text is in the transcript, which `record_sandbox_denials` already reads and then discards (`runner/mod.rs:1557-1581`).
- **Every refusal after "produced nothing" is terminal:** `Observation::PermissionDenied` → `Blocked` (`machine.rs:424-440`), and no repair is ever dispatched after a denial.

The refusal is only the last link: the cause is judging a read-only task by checks it cannot affect. Intended outcome:
- an inspection is accepted when its report meets its criteria, whatever the base's checks say, and never repaired because of them;
- a refusal of a command's *shape* (as opposed to a missing permission) costs one same-tier repair that names the rewrite, never the run;
- a Claude Code upgrade that starts refusing a shape the rules recommend fails verification instead of silently making the rules wrong.

## Design

### A. An inspection is judged by its report (fixes the issue opened in step 0)
Applies only to `kind = inspect`. Change tasks keep SPEC §10 exactly: a red base must be made green.

1. **Checks and gaps are information, not failures.**
   - The baseline still runs, and its failures are recorded as evidence, as today.
   - An inspect candidate's failures exclude every label already in `ctx.baseline.failures`. This reuses `VerificationReport::new_failures()` (`verify/mod.rs`), which exists and is unused.
   - Of the gaps at `runner/mod.rs:2530-2557`, only those about the profile's checks become information for inspect: a missing or undefined check command, and an amont gap. They are recorded on the receipt and no longer stop an inspection before its report review. Acceptance-evidence gaps keep their own handling (see 3, human sign-off).
   - An inspect worker that changed the tree is already stopped by the write-scope rule (`scope_exceeded`), so a new failure on an inspect candidate cannot come from its own work.
2. **The report is the deliverable, and a reviewer judges it against the tree.**
   - For `kind = inspect`, a report review is the inspection's verification. It is one research-tier dispatch, on the same tier as the worker (haiku).
   - It runs in the candidate worktree with read-only tools (`Read`, `Grep`, `Glob`; no Bash). The tool set becomes a parameter of the launch (`LaunchSpec`), not the constant `SANDBOX_TOOLS` at `adapter/claude.rs:401`, which `sandbox_args` passes as `--tools` (`:407-411`). The report review asks for exactly `Read,Grep,Glob` in **both** modes: `--tools` restricts the harness's tool set with or without `--restricted`, so in allowlist mode the reviewer gets no Bash and no write tools either (backend finding 10).
   - Its input is the contract's objective, its acceptance criteria and the report text, fenced as data like other worker-authored text.
   - It must check each claim it relies on against the tree, and answer per criterion: met / not met, with a one-line reason naming what it checked. A report that is well formed but made up does not pass (backend finding 1).
   - It is verification, not the patch review, so the contract's `review` field (which governs the patch review; an inspection has no patch) neither enables nor disables it. The receipt notes when a contract says `review: off`. No new contract error is introduced, so stored and queued inspect contracts keep loading (backend finding 7).
3. **Settling, failing closed.**
   - In `settle_acceptance` (`verify/mod.rs:550-574`), for inspect only, the `None` (bare), `LlmReview` and unnamed `Test` arms take the report review's verdict for that criterion.
   - `Check`, named `Test`, `HumanSignOff` and `AmontGate` keep their arms unchanged.
   - Each of these counts as **not met**: a verdict that is missing, cannot be parsed, or omits a criterion. A verdict that names an unknown criterion is ignored. A review dispatch that fails is `ReviewUnavailable` → `NeedsReview`, as for the patch review today.
   - The run is accepted when every mandatory criterion is met.
   - Pre-existing check failures stay visible on the receipt ("base failures, not judged: …").
   - **The settlement source is recorded.** Each settled criterion on the receipt carries a new field `settled_via` (`profile_checks`, `report_review`, `declared_evidence` or `human_sign_off`), with `#[serde(default)]`. It sits beside the contract's declared evidence, which is never overwritten. The name `settled_by` is not reused: it already exists as `Option<TestSettlement>` on `CriterionOutcome` (`verify/mod.rs:508`, `:556`) and is in stored receipts.
   - `independence_summary` (`verify/mod.rs:598`) derives independence from `settled_via`: `report_review` is model judgment, so not independent; `profile_checks` is independent; declared evidence is as today. When `settled_via` is missing (a receipt written before this change, pending ones included), it falls back to today's evidence-based rule.
   - The receipt re-seal recomputes from the stored `settled_via`, so an inspection's bare criteria never read `AllIndependent` (person's finding 3).
   - **The receipt knows its kind.** `Receipt` (`verify/mod.rs:1748-1778`) gains `kind` (`inspect` / `change`) with `#[serde(default)]` = `change`, so old receipts parse and are treated exactly as today.
   - **Human sign-off on an inspection** (person's finding 2). The outcome order, first match wins:
     1. a red `Check` or named-`Test` criterion → `Failed` (as in 5);
     2. a report criterion not met → the `CriteriaUnmet` repair (as in 5);
     3. every criterion met except sign-offs → `NeedsDecision`.
   - In case 3, the report review has run, and its per-criterion verdict and `settled_via` are persisted in the pending receipt (`store_pending_receipt`). The `VerificationGap` detail of that transition (`runner/mod.rs:2556`) carries **only** the `SignOffUnrecorded` gaps. The profile gaps that A1 turns into information go to the receipt's information field only, never into that detail, because `decide --answer approve` refuses while any gap there is open (`main.rs:4522-4548`).
   - When the sign-off is recorded, `reseal_receipt_with_signoff` (`main.rs:4867-4901`) sets the criterion's `evidence = HumanSignOff` as today (`:4886`), and also its `settled_via = human_sign_off`, so the stored source stays accurate. It then reads the receipt's `kind`:
     - for `inspect`, acceptance means every mandatory criterion met by its recorded settlement (report verdict, sign-off or declared evidence);
     - for `change`, and for any receipt without `kind`, the existing `receipt.verification.accepted()` rule is unchanged, because it also requires every check green.
4. **"Produced nothing" means an empty report.** At `runner/mod.rs:2472-2475`, `produced_nothing` for inspect is an empty (or `relais-blocked`) result, not an unchanged tree. A refusal during an inspection that still delivered a report becomes evidence, not a block.
5. **Repair only for the report, through its own observation.**
   - `Observation::CriteriaUnmet(Vec<criterion>)` gets its own row in `machine::decide`: `Repairing` + `Next::Attempt{Repair, rung.next()}` (same tier) while `repairs_used < max_repairs`, then `Failed` with `Reason::CriteriaUnmet`. It never escalates and is never `BaselineFailureNotWaived`.
   - It is not `VerificationFailed`, whose row escalates (`machine.rs:302-318`) and treats an unchanged candidate as recurrence. An inspect candidate is always unchanged (backend finding 3).
   - `Reason::CriteriaUnmet` is added to `lifecycle.rs`: `ALL` goes 50 → 51, plus its parse spelling, which the round-trip test at `lifecycle.rs:634` covers.
   - The repair addendum names the unmet criteria and the reviewer's one-line reasons (fenced), and says: answer in your final message; do not create files, install dependencies or change the tree.
   - Only criteria settled by the report review can trigger this repair. An unmet `Check` or named-`Test` criterion on an inspect contract (its arm is unchanged, and false while the named check is red) goes straight to `Failed` with `Reason::CriteriaUnmet`, the detail naming the criterion and the check. `Reason::CriteriaUnmet` ends both this path and a report criterion still unmet after the repair; the detail tells them apart (agreed with the person). It never goes to the `CriteriaUnmet` repair row, because a repair cannot turn a check red on the base green. When a check criterion and a report criterion are both unmet, the check failure wins: `Failed`, with no repair dispatched (bind findings 2 and 2b).
   - Base check failures never trigger an inspect repair. This is the issue's exact ask.
6. **Cost.** On 2026-10-04 an accepted inspect run cost $0.02 (one haiku dispatch), and each of the four failed ones cost $0.08–0.11 (two dispatches, no result). The report review adds about one haiku dispatch: about $0.04 per accepted inspection. Those four failures would then have been accepted at about $0.16 together, instead of failing at about $0.39.
7. **The prompt says so up front.** `build_prompt` (`runner/mod.rs:4060`) gains one kind-specific line for inspect: "your final message is the deliverable: do not create files, install dependencies or change the tree".

### B. A shape refusal costs a repair, not the run
Applies only in sandbox mode. In allowlist mode an unmatched rule *is* a missing permission, so it stays as today.

1. **Classify each refusal from the transcript.**
   - `sandbox/denials.rs`'s lenient reader also keeps each Bash call's `command` and each result's `is_error`.
   - A new `classify_refusals(jsonl) -> Vec<Refusal { tool_use_id, command, reason, class }>` covers the `is_error` results of refused calls.
   - **Shape:** a new measured `SHAPE_REFUSALS` set, kept beside `probe::PERMISSION_REFUSALS`: "can't be checked before it runs", "contains multiple operations", "requires approval", "Contains case_statement", "Blocked: sleep".
   - **Capability:** the `PERMISSION_REFUSALS` wordings ("has been denied", "--restricted confines the file tools", "denied by your permission settings").
   - The sets match the full measured sentences, not fragments, because a bare "requires approval" could also be a real missing permission.
   - **Fail closed: each of these is capability, which is today's terminal behaviour:**
     - text in neither set;
     - text in both sets (capability wins);
     - a denial with no `tool_use_id`, or an id that is not in the transcript;
     - a denial of any tool other than Bash;
     - every refusal, when the transcript coverage is not `Complete`.

     Only a run whose every refusal is positively shape-classified takes the new path (backend finding 5).
2. **Join on the id.** The adapter keeps `tool_use_id` per denial (`adapter/claude.rs:533-559`, carried on `DispatchResult` in `backend.rs:573`), so each denial maps to its transcript result exactly rather than by truncated command. `record_sandbox_denials` returns the classification instead of discarding the transcript, and it rides on `Candidate` next to `permission_denials`.
3. **A new observation and reason.**
   - `Observation::ShapeRefused(Vec<Refusal>)` gets a row in `machine::decide`: `Repairing` + `Next::Attempt{Repair, rung.next()}` when `repairs_used < max_repairs` (the repair rung is the same tier and model, `route/ladder.rs:299`); otherwise `Blocked / PermissionDenied` as today. It never escalates.
   - `Reason::ShapeRefused` is added to `lifecycle.rs`: `ALL` becomes 52, after A2's `CriteriaUnmet`, plus its parse spelling.
   - At `runner/mod.rs:2476-2488`: when the worker produced nothing and every refusal is a shape refusal, the runner uses `self.decide` → `Next::Attempt` (the `:2578-2603` pattern), not `self.stop`.
4. **A blockage claim after shape refusals is not the end.**
   - `snapshot_attempt` (`runner/mod.rs:2279`) ends the run on `worker_claims_blockage` before any judging, and the worker prompt asks a blocked worker to say `relais-blocked:`. So a shape refusal followed by that marker would still end the run.
   - The refusals are classified before that point (`record_sandbox_denials`, `:2130`). `Dispatched`, the value returned there, carries the classification and a `claimed_blockage` flag. There is no `Candidate` yet at `:2279`, and `result` is consumed at `:2227-2233`. Both are copied into `Candidate` where it is built (`:2362-2370`); B2's "rides on `Candidate`" means this copy.
   - When every refusal of the attempt is a confirmed shape refusal (B1, failing closed), the claim is recorded as evidence and the attempt does not stop at `:2279`. It falls through to the snapshot. The checks in between still end the run exactly as today:
     - a straggling writer (`:2291`);
     - a scope violation → `NeedsDecision` (`:2347-2353`);
     - a protected verification input changed → `NeedsDecision` (`:2401-2425`).
   - Otherwise it reaches the `:2476` refusal path and goes to the shape-refusal repair, even with an in-scope change to unprotected files. A worker that said it was blocked is never verified for acceptance on that attempt.
   - Any other blockage stays terminal exactly as today: a claim with no refusal, with any capability refusal, or with one that cannot be classified (person's finding 1).
5. **The repair is told the rewrite.**
   - `Progress.last_refusals` is a new field. It is not mixed into `last_failures`, which feeds `same_failures` and `Limit::Attempts`.
   - `build_prompt` gains a parameter, and under `AttemptKind::Repair` a `[refusal addendum]` lists each refused command and the harness's reason, fenced through `data_list_block`.
   - Each shape class gets its measured rewrite, worded as in the worker rules (`runner/mod.rs:4030-4049`). For example, a heredoc redirected into `$TMPDIR` → "write from python via os.environ['TMPDIR'] or a plain redirect".

### C. Verification checks the shapes the rules recommend
- `probe_plan` (`sandbox/probe.rs:136-223`) gains two positive controls, single-line so they survive the prompt character for character:
  - `log-redirect`: `echo x > $TMPDIR/l.log 2>&1; tail -1 $TMPDIR/l.log` → line `x`;
  - `python-write`: `python3 -c "import os;p=os.environ['TMPDIR']+'/p.txt';open(p,'w').write('y\n');print(open(p).read().strip())"` → line `y`.
- Each is measured as allowed. A `;` is fine when every part is analysable. "Contains multiple operations" fires only when one part needs approval. On 2.1.289, `echo a > $TMPDIR/s1.log 2>&1; tail -1 $TMPDIR/s1.log` ran with no approval (hand measurement of 2026-10-04), as did `make check > $TMPDIR/check.log 2>&1; tail -25 $TMPDIR/check.log` in runs 6 and 7 (backend finding 4). Before the digest is pinned, both exact step inputs are measured once more on the current harness.
- A refusal arrives as an `is_error` result, so the existing `OutputLine` judging fails on it with the reason in the snippet. No new `Expect` variant is needed.
- `PROBE_VERSION` goes 5 → 6 with a new pinned digest; the id-list test and `measured()` are updated.
- Every machine re-runs `relais doctor --verify-sandbox` once, and #163 tells it why.

### Docs
- SPEC §4 (L99): what settles an inspect criterion.
- SPEC §10 (L351-353): for inspect, base failures and check gaps are recorded and do not decide. The report review is the inspection's verification, independent of the `review` field.
- SPEC §8 (L277, L289) and the §9 table (L338): a shape refusal is not a missing permission. It costs a same-tier repair, never a stronger model.
- CHANGELOG `## Unreleased`.

## Packages
One branch and one PR (`work.*`: one PR per repo per plan), with reviewable commits.

0. **Issue and plan.**
   - Open the GitHub issue "inspect runs are repaired for base check failures and the repair worker misbehaves", citing the four runs and the other session's report.
   - Land this plan as `docs/plans/2026-10-05-inspect-judged-by-report.md`, the first commit, via the `worktree-task` skill.
A is split in two runs because `runner/mod.rs` is about 11k lines (backend finding 8). Each scope is derived from its own criteria, as the contract-scope feedback memory requires.

1. **A1**, as a relais run: checks and gaps as information for inspect, `produced_nothing` for inspect, and the inspect prompt line. Scope: `runner/**`, `verify/**`, `docs/SPEC.md`, `CHANGELOG.md`.
2. **A2**, as a relais run: the read-only report review dispatch with the tool set as a launch parameter, fail-closed settling, `Observation::CriteriaUnmet` with its reason, and the inspect repair addendum. Scope: `runner/**`, `verify/**`, `route/**`, `adapter/**`, `backend.rs`, `lifecycle.rs`, `main.rs`, `docs/SPEC.md`, `CHANGELOG.md`. `main.rs` holds the sign-off re-seal.
   - A1 on its own is not releasable: between A1 and A2 an inspection can be neither repaired nor accepted. Both ship in the one PR, and nothing is tagged between them (backend finding 11).
3. **B**, as a relais run. Scope: `sandbox/**`, `adapter/**`, `backend.rs`, `runner/**`, `lifecycle.rs`, `docs/SPEC.md`, `CHANGELOG.md`. `backend.rs` declares `DispatchResult.permission_denials` (`:573`), which every adapter builds (bind finding 1).
4. **C**, by hand (small and exact). Then re-run `relais doctor --verify-sandbox` on this Mac **before** installing, so dispatch never blocks (the lesson from #161).

Each package gets `make check`, falsification of its key test with a forced rebuild, and the implementation-review agent before the push.

## Verification
- **A:** a runner test with an inspect contract and `main_gone_check()` (red base) and a mock worker that returns a report.
  - With the mock report review saying met: `Accepted` with exactly 2 dispatches (the worker and the report review), no repair, and the receipt listing the base failure as not judged.
  - Saying not met: one repair whose addendum names the criterion and contains no check failure.
  - An empty report: blocked as today.
  - Fail-closed settling: each of a review verdict that is missing, unparseable, or omits a criterion settles that criterion not met; a failed review dispatch → `NeedsReview`.
  - `CriteriaUnmet`: a not-met verdict on the last allowed attempt ends `Failed` with no escalation dispatch.
  - An inspect contract with a `Check` criterion on a red base ends `Failed` with `Reason::CriteriaUnmet`, naming the criterion and the check, and no repair attempt row. The same holds when a report criterion is also unmet.
  - Sign-off: an inspection with a passing report, a red base, a profile gap and a missing human sign-off ends `NeedsDecision`. The report verdict is in the pending receipt, and the transition detail lists only the `SignOffUnrecorded` gap. After `decide --answer approve --criterion`, the run is `accepted` in both the ledger and `receipt.json`.
  - Old receipts: a pending receipt written before this change (its `settled_by` a `TestSettlement`, no `settled_via`, no `kind`) parses, re-seals under the new binary as today, and its independence is unchanged.
  - A change task's re-seal is unchanged: it still requires `verification.accepted()`.
  - Independence: an inspection's bare criteria settled by the report review read `NoneIndependent`; mixed report and check criteria read `PartlyIndependent`; a re-sealed receipt recomputes the same.
  - A stored inspect contract with `review: off` still loads, and its report review still runs.
  - Change tasks: the existing `baseline_failure_is_visible_and_not_waived` (`runner/mod.rs:6597`) still passes untouched.
- **A, tool set:** a runner test that the report review's launch asks for exactly `Read,Grep,Glob` in sandbox and allowlist mode alike. Against reality, the `system/init` record of a real report-review dispatch, in sandbox **and** allowlist mode, lists exactly those three tools. Allowlist mode is the new `--tools` path.
- **A against reality:** a real report review on a fabricated report must come back not met, and the reason must name the path it checked. The fabricated report lists directories that do not exist in a fixture worktree.
- **B:**
  - Classifier unit tests on the five measured shape texts and the three capability texts. Each of these is capability: an unknown text, text matching both sets, a denial with no or an unknown `tool_use_id`, a non-Bash denial, and anything under incomplete coverage.
  - Blockage marker:
    - a shape refusal plus `relais-blocked:` → one same-tier repair (one repair attempt row, same model). The same holds with an in-scope edit to an unprotected path; the test names that path;
    - a shape refusal plus `relais-blocked:` plus an out-of-scope write → `needs_decision` (`scope_exceeded`), and no repair;
    - the marker with no refusal → `blocked`;
    - the marker with a capability refusal → `blocked`.
  - Runner tests: a shape-only refusal with nothing produced → one same-tier repair whose prompt contains the `[refusal addendum]` and the rewrite. A capability refusal → `blocked` as today (the existing `refused_tools_block_the_run_and_buy_no_stronger_model`, `:6673`). An allowlist-mode refusal → unchanged.
- **C:** the probe plan test and pinned digest; then a real `relais doctor --verify-sandbox` on 2.1.289 passes all steps, including the two new ones.
- **End to end:** re-run the four failed inspect tasks of 2026-10-04 against their red bases. All four must be accepted, each with 2 dispatches (worker and report review) and 0 repairs, and the base failure listed as not judged. Then the next ordinary sandboxed run is #105's run 8.

## Phases
- [x] 0 — issue #167 opened; plan landed as the first commit (5689380).
- [x] A1 — an inspection on a red base behaves as on a green base (43f4162, e3043bd).
- [x] A2 — the report review judges an inspection (561b3bc, 8fbc761).
- [x] B — a sandbox shape refusal costs a same-tier repair (8a376ab, e11a9ac).
- [x] C — the probe checks the shapes the rules recommend; PROBE_VERSION 6 (743fa7a).

## Decision log
- 2026-10-05: **A1 makes a red base behave as a green one,** not a half-state. On a green base an inspection was already accepted with nobody judging its report, so A1 gives red bases that same guarantee, and A1 alone is releasable. A2 then tightens both with the report review. This supersedes the plan's "A1 on its own is not releasable" note.
- 2026-10-05: **A2's scope gained `sandbox/verify.rs`.** Its `LaunchSpec` literal breaks once the tool set is a launch field. The plan's scope missed it; the contract pre-flight grep caught it before the run.
- 2026-10-05: **Two shapes added to B, newly measured in A1's own sandboxed run** (Claude Code 2.1.289):
  - "Contains brace with quote character (expansion obfuscation)", from a python heredoc with a `{'` dict literal;
  - a leading `cd …;`, refused as needing approval.

  Both came from a worker editing Rust through python scripts. So B's rewrite for them is "use the Edit tool", and the sandbox rules regain "edit files with your Edit/Write tools" and "never start a command with `cd`".
- 2026-10-05: **A contract fault (mine).** A2's follow-up stopped on `scope_exceeded`: a one-line `notes: Vec::new()` in a `main.rs` test literal. The pre-flight grep had listed `main.rs`, but I launched in the same command without reading its output. I verified the candidate by hand: `make check`, plus the key tests falsified.
- 2026-10-05: **A hand fix in B's review round.** A shape-refused attempt that touched the profile's tests still entered `verifying` through the `VerificationInputsChanged` transition. That transition now comes after the shape decision, with a regression test, falsified.
- 2026-10-05: **The person's default for the low item.** A claimed shape-refused attempt's row is finished `repairing`.

<!-- panel: repos=relais adds= reviewers=backend body-sha=0207cc81f05b -->
