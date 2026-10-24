---
status: done
branch: feat/settle-and-show
repos: [relais]
adrs: [decisions:ADR-0042, ADR-0010, ADR-0004]
---
# Plan: relais shows its own work and settles what it can

## Review panel

👉 **Decide:** none beyond the two below. Approve if the pane fix plus one automatic repair, widen and retry per run is the right first step.
📍 relais · plan ready · next: the artboard example, then the pane snapshot test. Panel: backend, typescript, react, ui-design, ux-research, game-ux (opus-5-5), codex (gpt-6-astra). ⚠ The tool computed a delta panel (the plan file name was reused), so there was no Rust, unix or tui seat; backend covered the Rust work.
**Changed by review:** review repair gets a real continue path, at most one per run, and its own evidence file; the retry is checked against cancellation and budgets; the pane bound includes quiet rows and keeps the live run, with `↑ N rows above`.
📄 Full reviews: [2026-10-23-relais-shows-and-settles.reviews.md](2026-10-23-relais-shows-and-settles.reviews.md)
**Verdicts:** round 1 backend rework, 5 approve-with-changes; round 2 backend approve-with-changes; Codex approve-with-changes (2 high applied); all 6 seats re-bound on the final body: approve. Carried into implementation: one trim order (output to 8, folded rows, ledger/session/hint, output to 1, tail dim rows); `review_repairs_used` on `Budget` plus verify-kind and `max_repairs=2` tests; no parent decision prompt after an automatic widen; a focus assertion after a fold; success bar < 29 salvages (the three causes) by 10-30.

## Context

The person sees relais's own phases (baseline, verification, review) as only a timer in the right pane, the pane does not reliably follow the live run, and many runs end needing a hand salvage.

**Pane, measured.** relais streams check output live (run-65e729a702231-10bbc: 344 `output` events, the first at second 0 of each check). The plugin drops them at layout:
- `runsOf` (`claude-plugin/hooks/ui.ts:27`) passes every run of the session, oldest first, finished ones included.
- `relaisRows` (`hooks/pane.ts:225`) gives each run its fixed rows and splits only what is left into output: `cap = (rows − fixed) / withOutput`.

A red loop over this session's real runs (20 finished plus one live run cut mid-check, a 40-row pane) gives 27 output lines with 0 older runs, 4 with 1, and 0 from 3 older runs on. The pane then draws 65 to 362 rows into 40, with the live run at the bottom, below the visible rows. That is the missing auto-scroll.

**Salvage, measured** (decisions resolved `decision_salvaged` since 10-19: 56):
- review_findings 15;
- scope_exceeded 10;
- review_unavailable 8;
- process_crash 5;
- other 18.

The causes behind the first three:
- **Review findings:** any finding stops the run (`runner/machine.rs:573`, `Decision::stop`, no repair). Of the parsed salvaged reviews, 8 had only `low` findings. The runner does not parse severities, and the reviewer prompt (`install/mod.rs:285`) never asks for one.
- **Scope:** the worker edits tests that read the changed code (`workspace/mod.rs:763-793`). Today the orchestrator answers `revise --widen-scope` by hand (`runner/successor.rs:95-155`).
- **Reviewer unavailable:** 13 of 14 recent cases are "the reviewer ended without a terminal result" (`runner/review/engine.rs:543-550`), and nothing retries (`engine.rs:158`). The cause is not recorded beyond that sentence.

Each salvage also spends the main (Opus) session's tokens finishing the work by hand.

## 1. The pane draws the live run first (claude-plugin)

- **Two kinds of run.**
  - A run is *focus* when it is live (`isLive`), or when it is the newest run awaiting a person. `AWAITS_A_PERSON` moves from `runs.ts:51` into `timeline.ts` as an exported `awaitsAPerson(m)`, so `pane.ts` never imports `runs.ts`.
  - Every other run is *done*. Older awaiting runs fold with `· awaits you`.
- **Focus runs** keep today's full frame (direction A, the 2026-10-06 artboard): head, steps, the active step's output, then the tail.
- **Done runs** fold to one row each: `run <id> · <state> · <cost> · <mm:ss>`.
  - The row is dim when the run was accepted. Otherwise it starts with `FAIL` and takes the `error` colour, with the state text as its label (artboard line 5).
  - Only the 3 most recent are listed; older ones become one `+N earlier runs` row.
  - With no focus run, the most recent done run stays expanded, so its outcome stays readable. It is not one of the 3 folded rows.
  - Folded runs draw no output and take no output share.
- **Order** stays a top-to-bottom timeline: folded earlier runs, then focus runs.
- **Output share.** The output cap is computed over the expanded frames only, after the folded rows.
- **Alive at a glance.** A live run's active step always draws at least one output row: its latest line, or `│ waiting for output` (as quiet runs do, `pane.ts:259`). A step with no check (review) shows its agent's latest line.
- **Output first.** When output would get fewer than 8 lines, the folded rows reduce to the `+N earlier runs` row. If that is not enough, the live frame's ledger, session and hint rows go too.
- **Fits the body.** `layoutPane` never returns more than `max(room.rows, MIN_ROWS)` rows (`pane.ts:218`). The bound includes the quiet heads, `+N more quiet runs` and `quiet watcher down` (`pane.ts:220`, `:249-261`), which get only the rows the relais frames leave.
- **Overflow.**
  - The newest live run's frame is kept (else the newest awaiting run's).
  - Everything above it is trimmed from the top, and the first row becomes a dim `↑ N rows above`.
  - Each other focus run keeps at least its head row when rows allow (the newest live frame and its output win). An awaiting run also keeps its outcome row when rows allow.
  - Inside a kept frame that is still too tall, the trim order is: output down to its minimum row, then the folded rows, then the tail's dim rows. Its head and its agent Buttons always stay. `isLast` (the HINT row) means the last focus run.
- **Files:**
  - `docs/plans/2026-10-06-all-native-mod.pane.txt`, with a folded-runs example added before the code;
  - `claude-plugin/hooks/pane.ts` (`relaisRows`, `frame`, a new `foldedRow`, `layoutPane`);
  - `claude-plugin/hooks/timeline.ts` (`awaitsAPerson`) and `claude-plugin/hooks/runs.ts` (which uses it);
  - `claude-plugin/tests/views/relais-pane.test.ts`.
- **Tests** (the debug loop, made synthetic, at the `layoutPane` seam, plus rendered ones):
  - 20 finished runs and one live run mid-check, 40 rows → at least 20 output rows, the live run's head drawn, at most 40 rows in total;
  - the same with 2 quiet runs → at most 40 rows;
  - the same at 24 rows → at least 8 output rows, the folded runs reduced to `+N earlier runs`;
  - one live run with 3 agents and 3 stderr lines in a 12-row body → at most 12 rows, with its head and its agent Buttons drawn;
  - a live run older than an awaiting run, overflowing → the live run's head and output drawn, and `↑ N rows above` first;
  - one live run in review with no output yet → a `│ waiting for output` row;
  - one live run alone → deep-equal to a literal snapshot of today's `layoutPane` output, committed before the change;
  - rendered: an agent Button is focused, a second run finishes and folds the first → a Button is still drawn, found by its label.
  - The throwaway `tests/zz-debug/` (with its real-data fixture) is deleted.

## 2. Review findings below `high` get one repair (relais runner)

- **Severity in the prompt.** Every reviewer prompt asks each finding to start with `[blocking|high|medium|low]`: `install/mod.rs:285`, and the runner's own review prompt in `runner/review/`.
- **Parser.** `review/mod.rs` gains `finding_severities(text) -> Option<Vec<Severity>>`. A finding is a numbered line under the verdict that starts with `[sev]`. The parser returns `None` when any finding lacks a severity or none is tagged, and `None` means stop, as today. A test covers a prose-only answer → `None`.
- **Decision.** `Observation::ReviewFindings` carries the severities.
  - In `decide` (`machine.rs:573`): when every finding is `medium` or `low` and `repairs_used < max_repairs`, the run goes to `Repairing` with a new reason, `review_repair`.
  - Otherwise it stops `needs_review` (`review_findings`), as today.
  - **Continue path.** Findings reach `decide` through `Engine::stop` (`verification.rs:86`, `scheduler.rs:1050`), which errors on a non-Stop `Next` (`engine.rs:231`). Low and medium findings get a path that continues back to `attempt_loop`. `kind: verify` contracts (the Phase 3 successor included) and parts runs always stop, because they have no worker to repair.
- **What the worker gets.** `progress.last_failures` carries the findings, quoted as data (`data_list_block`). `prompt.rs` gains a review addendum: "a reviewer found these; fix them, change nothing else".
- **After the repair**, the candidate goes through checks and review again (`review_required` is per candidate). A second set of findings stops `needs_review`. The repair uses a repair slot (SPEC §9). There is at most one `review_repair` per run, counted from its transitions, because `max_repairs` can be 2 (`route/mod.rs:2994`).
- **Review evidence survives repair.** Store every whole-candidate review in an immutable dispatch-specific artifact, linked to its candidate SHA and dispatch ID. Evidence must reference that artifact rather than the mutable `review.txt` path. The repair-and-accept integration test reads both reviews afterward and verifies that each artifact still matches its recorded hash and candidate.
- **Reporting.** The harness report counts `review_repair` beside `review_findings` as caught by review (ADR-0042, `report/harness.rs:99`). Salvage and dataset labels are unchanged.
- **Tests:**
  - `machine.rs:879` gains the low and high cases;
  - `runner/tests/mod.rs:2377` gets a low-only twin that ends `accepted` after one repair, and a high-finding twin that still stops;
  - an unparseable-severity test that stops.

## 3. A scope overflow made only of tests is verified with the scope widened (relais runner)

- **When.** Every out-of-scope path matches the test-tree globs (`TEST_TREE_INPUTS`, `verify/mod.rs:1640`, made shared), and none is under a protected prefix (`workspace/mod.rs:781`).
- **What relais does.** It raises `scope_exceeded` and answers it at once, as `revise` with the scope widened, through the existing successor path (`runner/successor.rs:95-155`: a `kind: verify` contract, verified and reviewed again).
  - The answer is recorded on the decision row with actor `relais`, a new actor value (SPEC:333).
  - The successor intent is named on the `outcome` event.
- **Plugin.** The outcome line handler (`claude-plugin/hooks/runs.ts`, near :492) calls `startIntent` (`runs.ts:239`) for the `successor_intent {intent_id, repo}` it carries. That is the same shape as the decide reply (`decide.ts:186`).
  - Runs marked `foreign` are skipped.
  - The status-poll reconcile (`runs.ts:278-313`) stays the fallback, and relais's claim keeps it to one launch.
  - The outcome message says the scope was widened for tests, and names the paths as quoted data: each cut to `MAX_LINE`, at most 10, then `+k more`.
- **Reviewer told.** The successor's review prompt lists the widened test paths as out-of-scope edits to check closely: a weakened test is the risk.
- **Without the plugin.** A CLI run prints the successor intent and the command that launches it.
- **Otherwise unchanged.** A mixed or non-test overflow stops as today.
- **Policy.** `[execution] widen_scope_for_tests` defaults to `true`; `false` keeps today's stop.
- **Tests:**
  - tests-only overflow → successor intent recorded, actor `relais`;
  - a mixed overflow → `needs_decision`;
  - a test path under a protected prefix → `needs_decision`;
  - a plugin test: one outcome line carrying an intent → exactly one `relais run --intent <id>` child; models rebuilt from status → none.

## 4. A reviewer that ends without a result is retried once (relais runner)

- **Retry.** In `review_candidate` (`engine.rs:158`), classify the dispatch termination before assigning `UnavailableCause`. Only a missing terminal result without cancellation or resource exhaustion is eligible for one further `review_unit` call. Preserve cancellation and named exhaustion evidence without retrying. Require both `review_spend_blocked` clearance and positive remaining work and total budgets; clip the retry timeout to both remaining budgets, without renewing `REVIEW_MIN_WALL`. Test cancellation, timeout, exhausted work/total budgets and an eligible missing-result retry, asserting dispatch counts and recorded causes.
- **Typed cause.** `Reviewed` carries an `UnavailableCause` enum (`TerminalMissing`, `LaunchFailed`, …). The retry matches the variant, never the message (`engine.rs:545-550`).
- **No retry when the plugin is gone.** A launch refusal (`mod_gone`) or no reviewer tier is not retried: the same cause would fail again.
- **Recording.** Each reviewer that ends without a result records its dispatch end (exit, turns, last stderr line) in the transition detail, so the next count names a cause.
- **Unchanged.** A second failure stops `needs_review (review_unavailable)` as today, and `review-parts` stays the person's answer.
- **Tests:** a mock reviewer that fails once then answers → reviewed; one that fails twice → `review_unavailable`, with two dispatches; `mod_gone` → one dispatch.

## 5. Record it

- **SPEC:** §9 (the table and the repair rule), §10 (review prevents… now with one repair below high), §8 (the automatic widen for tests). Each changed step keeps its "prevents" sentence (ADR-0042 `harness.part-names-its-failure`).
- **ADR:** a new relais ADR, "a run settles what it safely can before asking a person". Its three rules are the three above, each kept reversible by policy or by the next stop.
- **CHANGELOG.** Then a relais release (the plugin rides along); the person reinstalls the plugin.

## Routing

Phases 1–4 each go to a relais run with its own contract and write scope; review and verification are relais's own. Phase 5 is written in the session. One PR, one commit per phase.

## Verification

- **1:**
  - `make plugin` (PLUGIN_SKIP_OK unset) → exit 0, with the new pane tests named.
  - After the release and the plugin reinstall, in a session that already has finished runs: start a run and watch the pane during verification. Its check output lines are drawn and the live run is on screen without scrolling. Screenshot it, compared with today's.
- **2–4:** `cargo test --workspace` and `cargo clippy --workspace -- -D warnings` → exit 0, with the new tests named; the pre-commit gate is green.
- **Cost:** the median cost of a repair attempt plus a re-review, from the ledger, is stated before merging.
- **After 7 days (10-30):** success means salvages for review_findings, scope_exceeded and review_unavailable together drop below half of their rate here (33 in 4 days). Then also: salvages by raised reason from the ledger (the query in Context), against the 56 measured here. Also the count of `review_repair` and automatic widens, and how many of those ended `accepted`.

## Decisions for the person

1. A review repair applies to `medium` and `low` only, while `high` and `blocking` still stop (proposed). The alternative also repairs `high` once.
2. The automatic widen is on by default, with a policy switch (proposed).

## Verification record

- **1 (pane):** `make plugin`, PLUGIN_SKIP_OK unset, on the final tree (f96838fc…) → exit 0, 325 pass, 0 fail. New tests:
  - "one live run alone draws as it did before the fold" (the literal snapshot, committed in d7497dcd before the change);
  - "20 finished runs and a live run mid-check: output first, the head drawn, 40 rows at most";
  - "the same at 24 rows keeps 8 output rows and folds the runs to one count row";
  - "a live run with 3 agents and 3 stderr lines in a 12-row body keeps its head and Buttons";
  - "a live run older than an awaiting one keeps its head and output, behind a rows-above row";
  - "a folded failed run starts with FAIL in the error colour; an accepted one is dim";
  - "relais rows are the same with or without a quiet run at 8 / 24 rows";
  - "a Button is still drawn after a second run finishes and folds the first".
  - The red loop over this session's real runs (0 output rows with 3 or more earlier runs) became these synthetic tests, and the throwaway loop was deleted.
  - The live pane check after the release and plugin reinstall is a release step, not a row here.
- **2–4:** on the rebased tree, `cargo clippy --workspace --all-targets -- -D warnings` → exit 0; `cargo test --workspace` → exit 0 (439 s). New tests:
  - Severity parser: `tagged_findings_give_their_severities`, `prose_untagged_mixed_and_none_give_none`, `the_reviewer_prompt_asks_for_a_severity_tag`.
  - Review repair (runner/tests/review_repair.rs): `low_findings_are_repaired_once_and_the_run_is_accepted` (both review evidence files keep their hashes), a high finding, a verify run, low findings twice; plus report and harness tests for `review_repair`.
  - Widen (verify_kind.rs, decide_actor.rs, release_scenarios.rs): the tests-only overflow (ledger, outcome event and Done agree on cancelled with the successor intent), mixed and protected overflows, the policy switched off, the CLI refusing `--actor relais`, the relais answer excluded from labels, the successor reviewer prompt naming the widened path, the CLI printing the intent; plugin "one outcome line carrying a successor intent launches exactly one child and sends no decide prompt" and "models rebuilt from status launch no child".
  - Retry (runner/tests/review_retry.rs, 8 tests): a missing result retried once then reviewed; twice, then unavailable; launch failure, cancel, timeout, work and total budgets (scripted ledger clock) not retried; the transition names the dispatch end.
- **Gates:** every commit ran the pre-commit gate. Six skipped only `plugin-test`, which timed out at 5 s under load averages of 40 to 125 (every failure a timeout): phase 3, phase 4, the retry tests, the ADR commit, and the review-fix commit. The two that change plugin code or tests (phase 3 and the review fix) were proven with the plugin tests run alone on the same tree; the review fix is covered by the `make plugin` run above.
- **Cost:** no `review_repair` run exists before the release. The October ledger gives a median of $0.20 per repair (48 repairs) and $0.68 per review (91 reviews), so about $0.89 for one repair plus one re-review, against a hand salvage finished on Opus in the main session.
- **After 7 days (10-30):** salvages raised for review_findings, scope_exceeded and review_unavailable together must stay under 29 (half of 33 in 4 days), read from the decisions table.

## Decision log

- 2026-10-23: the rendered focus test asserts that a Button survives the fold and that none claims autoFocus. The test harness draws no client and cannot focus a Button, so where focus lands after a fold is not observable in tests.
- 2026-10-23: on overflow, the newest live frame and its output take the rows first. Other focus runs keep their head row only when rows remain (implementation review finding 6; Section 1 amended).
- 2026-10-23: phases 2 and 3 were each split into two relais runs to fit the 45-minute work budget.
- 2026-10-23: four new relais snags hit while building this (next plan): accepting a run whose required tests do not exist (phase 4); a `from_candidate` run scope-checked against its parent's changes; an answered interrupted in-place run blocking its worktree until `resume --retire`; a successor contract unable to read-hint a file its candidate creates.

## Implementation review

approve-with-changes (round 1: 109k, 79 s; delta: 113k, 21 s).
- Fixed: the timeout test; a successor rebuild test that can fail; the artboard row; this record (results, gates, cost, Decision log); line 60 amended.
- New finding 8: the plugin tests had not been shown on the edited test. They were then run with `make plugin` on the final tree, and the result is recorded above.

<!-- panel: repos=relais adds=ui,lang:typescript reviewers=backend,lang:typescript,react,ui-design,ux-research,game-ux,codex body-sha=2277ae68b8bb -->
