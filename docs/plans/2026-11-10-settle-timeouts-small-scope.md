---
status: active
branch: feat/settle-timeouts-small-scope
repos: [relais]
adrs: [ADR-0010, ADR-0011, ADR-0012]
---
# relais settles two more stops itself: a timed-out worker's tree, and a small scope overflow

## Review panel

👉 **Decide:** none — approve if a verify that is reviewed is cheaper than an Opus salvage by hand.
📍 relais · plan reviewed, nothing built · next: worktree, B+C first. Panel: backend, lang:rust, architect, po, tui, unix (opus-5-5), codex (gpt-6-astra).
**Changed by review:** B ships first, with its own CLI plumbing; the stop must be acknowledged and the candidate published atomically; every automatic successor is reviewed.
📄 Full reviews: [2026-11-10-settle-timeouts-small-scope.reviews.md](2026-11-10-settle-timeouts-small-scope.reviews.md)
**Verdicts:** round 1: 6 approve-with-changes; round 2: 3 approve, 3 approve-with-changes; codex approve-with-changes (2 high, applied); 10 carried into implementation.

## Picture

```
worker hits wall time
  └─ stop agent ── no ack ──────────────► interrupted, as today
       │ ack
       └─ tree changed? ── no ──────────► interrupted, as today
            │ yes
            ▼
     snapshot: candidate ref + patch + sha on the attempt
            ▼
     interrupted (process_crash, timed_out, candidate)
            ▼
     relais answers revise ─► successor: kind verify
                              (checks + review, own budgets)

scope_exceeded
  ├─ tests only ─────────────────────────► widen (today)
  ├─ tests + ≤ 1 source file, ≤ 10 lines,
  │  not protected, counts known ────────► widen (new)
  └─ anything else ──────────────────────► needs_decision
```

The top flow is rule A, the bottom one rule B; every new branch ends in a verify successor that is checked and reviewed again, and every unmet condition falls back to today's stop.

## Context

In the 30 days to 2026-11-09, the ledger shows two stops that the orchestrating session (Opus, at main-session prices) still finishes by hand, though the next step is mechanical. ADR-0011 already settles three such stops (review_repair, a test-only scope overflow, a reviewer with no result); the reviewer one is fixed (0 since 10-21).

- **Wall time.** 57 runs ended `interrupted (process_crash)` with `timed_out: true` ("the native worker did not stop within the attempt's wall time"), $130 of worker spend. 39 were salvaged; the notes repeat "the worker hit its wall time after writing the whole change; verified by hand: suite passed". Today `snapshot_attempt` (`runner/attempt.rs:592`) ends such an attempt with no candidate at all, so the only way on is the orchestrator.
- **Small scope overflow.** 88 `scope_exceeded`; test-only ones are already widened by relais (ADR-0011 rule 2). Of the rest, 30 are one file with 10 or fewer changed lines (a field the change needs, a line in an example app), each costing an orchestrator decision and a revise round.

Outcome: relais answers both itself, with actor `relais`, through the same successor path as the test widen (`decide revise --widen-scope`, SPEC §8 / §383): a `kind: verify` of the candidate, verified and reviewed again. Nothing is ever accepted without checks and review; each rule is bounded and recorded.

Decided with the person (2026-11-10): A's successor is `kind: verify` (no worker); B's limit is 1 file, ≤ 10 changed lines (added + removed), test paths still unlimited.

Worktree from `origin/main` via the `worktree-task` skill (`relais-wt-settle-timeouts`, branch `feat/settle-timeouts-small-scope`); this plan lands as `docs/plans/2026-11-10-settle-timeouts-small-scope.md` in the first commit. Order: B1–B3 with C (report) first, releasable alone; then A1–A3; D grows with each.

## Phases

### A1. Confirm the stop before using the tree

Files: `crates/relais/src/adapter/native.rs`, `crates/relais/src/backend.rs`, `crates/relais/src/procs.rs`, `crates/relais/src/runner/tests/native.rs`.

At `native.rs:917`, at wall time, call the existing `stop_and_confirm` with its own grace (`waits.spawn`, not `min(attempt_end)` which has already passed), and carry the result as a new variant `Awaited::TimedOut { detail, stop: StopAck }` (the six existing `Awaited::Ended` constructors stay untouched) → `#[serde(default)] timed_out_stop: Option<TimedOutStop>` on `LaunchResult` (`backend.rs`, beside `looped`/`lost_worker`). `StopAck::Cancelled` maps to `Ended::Cancelled`; `running == None` (no agent bound) or `StopAck::Unconfirmed` is unconfirmed. The subprocess worker sets the flag at its own call of `procs::wait_for_exit` (procs.rs:410/828), never inside it (verify/mod.rs:1582,1606 share it), and only when `Supervised.group` reports the group killed and `kill_group` did not return `GroupKill::Refused` (procs.rs:576-584), not merely the leader reaped; on Windows, where re-parented descendants survive `taskkill /T` (procs.rs:1625), it is always unconfirmed. An unconfirmed stop keeps today's path exactly (no snapshot).

### A2. Snapshot a timed-out tree

Files: `crates/relais/src/runner/attempt.rs`, `crates/relais/src/runner/machine.rs`, `crates/relais/src/runner/tests/native.rs`.

At `attempt.rs:592`, in the `terminal_result_missing` branch, when `timed_out_stop` is confirmed, the tree has changes from its floor, and `wait_for_writers` finds no holder: run the snapshot part of `snapshot_tree` (candidate sha, `name_candidate`, `candidate-N.sha`, `candidate-N.patch`, `candidate-latest.patch`, `CandidatePatch` evidence) and finish the attempt `Interrupted` with that sha. Split `snapshot_tree` so the commit/name/export part is one function both paths call, taking the attempt's end `State` as a parameter (today it writes `State::Verifying`, attempt.rs:711) and returning errors instead of calling `fail_preflight`; it writes `candidate-N.sha`, `candidate-N.patch` and `candidate-latest.patch` as temp file + `sync_all` + `rename` (approval.rs:305-307 pattern) and records the evidence last, so a SIGINT/SIGTERM mid-snapshot leaves no truncated patch (unix#4). On the timed-out path `UnresolvedConflict` falls back to the no-candidate end, a storage-full error still propagates (attempt.rs:708,717), and "changes from its floor" means `changed_paths_from(floor, sha)` is non-empty, checked before the ref is named; the timed-out path does not run the scope check, holdout collision or verification (the verify successor does, from the base, SPEC §341). The `process_crash` detail gains `candidate: <sha>`. A non-storage-full snapshot error falls back to today's no-candidate end, said in the detail. The candidate is published last: the attempt's `candidate_sha` and its `CandidatePatch` evidence are written in one ledger transaction only after the artifact files are complete, because `finish_attempt(.., None)` keeps an earlier sha through `COALESCE` (`ledger/runs.rs:480`) and `candidate_attempts` selects any non-null sha (`runs.rs:791`). Add `crates/relais/src/ledger/runs.rs` to this phase; a test injects a failure between the artifact writes and the publication and asserts no selectable candidate, earlier candidates still resolvable, and storage-full still propagating.

### A3. relais answers the timeout

Files: `crates/relais/src/runner/engine.rs`, `crates/relais/src/runner/successor.rs`, `crates/relais/src/protocol.rs`, `crates/relais/src/cli/run.rs`, `crates/relais/src/runner/tests/verify_kind.rs`.

Beside `widen_for_tests`, a new `verify_timed_out` settle: gate on the raising `interrupted`/`process_crash` transition (a new `successor::interrupted_transition`, since `raising_transition` finds only `needs_decision`) whose JSON detail has `timed_out: true` and `candidate` (A2 writes it there, not only in the prose detail), `parent.is_none()`, purpose not Replay, `contract.kind == change`, and `[execution] verify_timed_out` (default true). It builds the contract with `successor::widened_contract(contract, run, &[])` (verify of the candidate, scope unchanged), plans with `successor::plan`, and writes `resolve_decision_with_intent` (`DecisionRevised`, actor `Relais`, note "worker hit its wall time; relais verifies its tree (execution.verify_timed_out)", `Interrupted → Cancelled`). The run ends `cancelled` with code `timed_out_verify_queued`, and its kept worktree is retired once the intent is written (an `Interrupted` end keeps it, `machine.rs:129`); `Request::Done` and the `outcome` event carry the code and `successor_intent`, the plugin launches it as it does the test widen. `RunOutcome` carries `Settled.code` to the CLI: `cli/run.rs:617-623` prints `cancelled (<code>)` and one sentence for each rule (`widened_lines` stops hardcoding "test paths"), then `run it with: relais run --intent <id>`; `release_scenarios.rs` asserts the stderr for each code. It reuses the `Settled` plumbing and CLI printing from B3. Bound: a successor of a relais settle is never settled again (no widen, no timeout verify); the successor is a verify (no worker, cannot time out this way), and a timed-out verify never qualifies (kind check).

### B1. Small source overflow

Files: `crates/relais/src/runner/successor.rs`, `crates/relais/src/runner/engine.rs`, `crates/relais/src/runner/tests/verify_kind.rs`.

At `successor.rs:133-160`, replace `is_tests_only` in the planner with `widenable(profile, changes, limits)`: no path protected; `classify_verification_inputs` finds no policy input; every path is a test path, except at most `widen_scope_small_max_files` (1) non-test paths whose `added + removed` sum to at most `widen_scope_small_max_lines` (10); a change with unknown counts (`None`, binary) is not small. It reads `changes[]` from the raising transition (`machine.rs:658-680` already records `{path, added, removed}`; add `out_of_scope_changes` beside `out_of_scope_paths`). `plan_test_widening` → `plan_scope_widening`, returning which rule fired (`tests` or `small`). Every automatic successor contract (B's widen and A3's timeout verify) has its review raised to at least `Review::Required`, keeping a stronger one: today review is required only by routing or changed test inputs (`attempt.rs:900`), so a source-only successor of a review-off contract would be accepted unreviewed. One shared helper prepares both contracts; tests cover a review-off contract with a source-only small widen and with a timeout verify: each dispatches a reviewer and is not accepted without its review.

### B2. Policy keys

Files: `crates/relais/src/policy/mod.rs`, `crates/relais/src/route/candidate.rs`.

At `policy/mod.rs:455-495`: `[execution] widen_scope_small_max_files` (default 1, 0 = off) and `widen_scope_small_max_lines` (default 10), with the same `skip_serializing_if` default trick as `widen_scope_for_tests` so the authority hash does not move; `widen_scope_for_tests = false` still turns off all widening. Route candidate comparison (`route/candidate.rs:761-776`) refuses a tuned candidate that changes them, as for the existing key. `verify_timed_out: bool` (A3) follows the same pattern.

### B3. Wording

Files: `crates/relais/src/runner/engine.rs`, `crates/relais/src/runner/review/engine.rs`, `crates/relais/src/cli/run.rs`, `claude-plugin/hooks/runs.ts`, `claude-plugin/tests/successor.test.ts`.

B ships before A, so the `Settled { code, sentence, successor }` generalisation of the `widened` plumbing (`engine.rs:325-372`), `RunOutcome` carrying `Settled.code`, and the `cli/run.rs:617-623` stderr change (one sentence per code, `widened_lines` no longer hardcoding "test paths") land here; A3 only adds its own code and sentence to them.

The plugin's `widenedText` (`runs.ts:528`) picks its sentence by `code`: tests widen, small widen naming the path (+a/-r), and timeout verify ("the worker hit its wall time; relais verifies its tree", with no path list).

The decision note and the outcome sentence say which rule fired ("only by test paths" / "by one small edit: `path` (+a/-r)"); the code stays `scope_widened_for_tests` for tests-only and is `scope_widened_small` for the new rule. The successor's review prompt already lists widened paths "as out-of-scope edits to check closely"; for the small rule it says the edit is source, not a test, and must be required by the objective.

### C. Report the settles

Files: `crates/relais/src/report/harness.rs`.

The exploration found no report code counting automatic widens, though ADR-0011 says they are counted. Add `auto_settled { review_repair, widen_tests, widen_small, timed_out_verify }` from decisions with actor `relais`, classified by their `successor_intents` row (joined on `decision_id`: empty `scope_delta` = timed_out_verify, else tests vs small by the widened paths), never by note text, and `review_repair` transitions, and the successors' outcomes (accepted / failed / needs_review). Report schema bump, expand-only; `HarnessReport::render` gains an `auto settled:` line (counts by rule, then successor outcomes).

### D. Docs

Files: `.adr/records/0012-a-run-settles-a-timeout-and-a-small-overflow.md`, `.adr/records/HEADS.md`, `docs/SPEC.md`, `CHANGELOG.md`.

A new ADR superseding the `execution.settle-before-asking` head (HEADS.md row updated): five settles, with the 30-day evidence above. It states that ADR-0010 still holds: a wall-time end is still recorded as a decision, and relais answers it with a verify of the existing tree, not a repair or a new worker dispatch. SPEC §8 (the test-widen paragraph at :345 grows the small rule; a new paragraph for the timeout verify), §9 table row "Process crashes or terminal result is missing" (:403) gains the timed-out-with-candidate case, §11 report fields. The ADR's consequences say that a settled run whose verify successor fails ends `failed` with no open decision, and that a person can still finish it with `decide --answer salvaged` (architect#3). SPEC also says that once an interrupted attempt carries a candidate, any `--candidate <run>` without an index resolves to it (lang:rust#5). CHANGELOG `## Unreleased`, naming that a timed-out run with a candidate now exits as cancelled (7), not interrupted. `amont agents-md` if the generated block moves.

## Tests (written before each change, in the existing harnesses)

- `runner/tests/native.rs` scripted plugin: a worker that edits then outlives the wall time and acks the stop → attempt `Interrupted` with a candidate sha, patch files present, detail `timed_out: true, candidate`; same with no ack → no candidate, detail as today; same with no edit → no candidate; a cancel during the stop wait → `cancelled`, no candidate; an unresolved conflict at snapshot → no candidate.
- `runner/tests/verify_kind.rs` (beside `widened_run`, :1380): the timed-out run ends `cancelled (timed_out_verify_queued)` with a relais-actor decision and one intent; launching the intent verifies and reviews the candidate; a red check ends the verify `failed`; `verify_timed_out = false` leaves the run `interrupted` with the decision open; a verify run that times out is not settled; planning the successor from the `interrupted` run passes `candidate::resolve` (state and `candidate_attempts`); a timed-out tree with the objective half done (one of two required edits) is not accepted by the verify's review (ends `needs_review` or `failed`) (po#3).
- Subprocess worker (`procs.rs` tests): a timeout with edits yields a confirmed `timed_out_stop` and the same settle; a timed-out verification command carries no flag.
- Scope: 1 non-test file of 10 lines widens (code `scope_widened_small`); 11 lines, 2 files, a protected path, a verification input, a binary (`None` counts), or `max_files = 0` → `needs_decision` as today; tests + one small file widens; tests-only unchanged (existing tests stay green).
- Policy: the authority hash is unchanged with all three keys at default; a tuned candidate changing them is refused.
- Report: `auto_settled` counts on a fixture ledger; an older report schema reader still parses.

## Verification

- `make check` in the worktree: exit 0, test count before vs after (new tests listed by name).
- Live check before release, on a scratch repo with `RELAIS_STATE_DIR` pointed at a scratch dir (never the real ledger, see the v40 incident): a native worker given a task with a tiny `limits.wall_seconds` that edits a file → the run ends `cancelled (timed_out_verify_queued)`, the plugin launches the verify, the verify reviews the tree. A contract with a write scope missing one file the change needs, edited by ≤ 10 lines → `cancelled (scope_widened_small)` and a verify successor.
- After release + install (the person reinstalls the plugin): a dated check in 14 days counting `process_crash` salvages and non-test `scope_exceeded` decisions answered by the orchestrator, as weekly rates against the 30 days to 11-09 (39 salvages ≈ 9.1/week; the non-test `scope_exceeded` count queried from the ledger and written here before release, with 30 of them small), plus `auto_settled` outcomes (how many verify successors were then accepted vs failed) and their worker + review cost per settle against the orchestrator salvage it replaced. Success: orchestrator timeout salvages ≤ 3/week (from ~9.1/week). Roll back a rule if any candidate it settled and that was accepted is later reverted or fixed by hand, or if over ≥ 6 settles more than half end failed or needs_review (the settle only adds a round). Roll back by setting `verify_timed_out = false` / `widen_scope_small_max_files = 0`, never by an older binary.

## Non-goals

- Raising `max_wall_seconds` per repo (option C, running in another session).
- `native_spawn_missing` (15 runs, $0.2) and the reviewer no-result retry (fixed).
- Enabling the loop breaker (separate bar, own memory note).

## Code review

- relais: backend, lang:rust, unix, tui (CLI stderr and plugin pane sentences change for each code; architect left out: no new module, crate or repository)

<!-- panel: repos=relais reviewers=architect,backend,lang:rust,po,tui,unix,codex body-sha=9cf37f8ea7e3 -->
