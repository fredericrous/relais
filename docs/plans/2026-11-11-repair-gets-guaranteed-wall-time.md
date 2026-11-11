---
status: active
branch: feat/repair-wall-floor
repos: [relais]
adrs: [ADR-0010, ADR-0012, ADR-0014]
---
# A repair gets a guaranteed wall time

## Review panel

👉 **Decide:** none — approve if paying at most ~$65 a month in extensions beats paying for repairs that are sure to be cut.
📍 relais · plan reviewed, nothing built · next: worktree, Phases 1–3. Panel: backend, lang:rust, tui, unix (opus-5-5); ⚠ codex: usage limit, no review.
**Changed by review:** the extension moves to launch time (just before `worker_dispatched`); the refusal gets its own `Limit::RepairFloor` sentence; the 60 s wall test and claim are fixed.
**Verdicts:** round 1: 4 approve-with-changes; round 2: 2 approve, 2 approve-with-changes (1 high, applied; backend and rust re-ran: approve); 12 carried into implementation, backend#5 (wait budgets after `Work`) first.
📄 Full reviews: [2026-11-11-repair-gets-guaranteed-wall-time.reviews.md](2026-11-11-repair-gets-guaranteed-wall-time.reviews.md)

## Context

From 2026-10-10 to 11-09, 56 relais runs ended "the native worker did not stop within the attempt's wall time". 22 of them were repair or escalation attempts. Each was launched with `budgets.remaining()` (`runner/attempt.rs:375`), the run's leftover work budget, and was cut after 78–1934 s; 9 of the 22 were Opus escalations. The machine decides to repair or escalate from counters alone (`runner/machine.rs:464-494`, `:538-563`, `:720`). It never looks at time, so relais pays for attempts that cannot finish.

Successful attempts in accepted runs over the same window (attempt start to end, verification included):

| attempt | n | p50 | p75 | p90 |
|---|---|---|---|---|
| 2 (repair) | 164 | 120 s | 436 s | 907 s |
| 3 (escalation) | 14 | 531 s | 909 s | 1035 s |

**How often it fires, and what it costs.** This is an estimate from `events.jsonl`: work left at each worker dispatch is the repository's limit minus time since the first dispatch, slot waits excluded.
- 51 of 255 repair starts had less than 600 s of work left.
- 11 of 18 escalation starts had less than 900 s left.

Worker cost per minute in the same window: sonnet $0.093, opus $0.108, fable $0.216. Even if every extension were the whole floor, that bounds the extra spend at about 51 × 10 min × $0.093 + 11 × 15 min × $0.108 ≈ $65 per 30 days. Today, $130 of worker spend ends in a timeout, plus the Opus salvage time.

Reviews already have a floor: `remaining().max(REVIEW_MIN_WALL)` (300 s, `review/engine.rs:730`). That floor is silent, though: it is not recorded and it also overrides `total`.

Two related changes have already shipped:
- **ADR-0012, #431.** A timed-out worker with a changed tree now gets a `verify` successor. That rescues a finished change, but not a repair that never had time to make one.
- **#426.** relais's own `max_wall_seconds` is now 3600. That shortens the tail, but the leftover still goes to the repair.

## Goal

A repair or escalation attempt starts with at least a set minimum of wall time: 600 s for a repair, 900 s for an escalation, by default. Where the work budget has less left, it is extended by the shortfall and the extension is recorded. Where the run's `total` budget cannot cover the minimum, the attempt is not launched and the run ends `budget_exhausted`, as it does today when the work budget is empty.

## Picture

```
checks or review fail
  └─ machine picks Repair or Escalation (counters only, unchanged)
       ▼
attempt start, kind Repair/Escalation
floor = min(min_<kind>_seconds, effective max_wall)
  ├─ work left ≥ floor ─────► launch with what is left (today)
  └─ work left < floor
       ├─ total left ≥ floor ─► extend work by the shortfall,
       │                        record work_extended_s,
       │                        launch with the floor
       └─ total left < floor ─► no launch: budget_exhausted,
                                detail names floor and total left
Initial attempts, reviews, verification: unchanged
```

A repair or escalation either gets at least its floor, paid from the run's total budget, or is not launched at all; nothing else in the run changes.

## Behaviour

- **The floor.** It is `min(min_repair_seconds, effective max_wall_seconds)` for a Repair and `min(min_escalation_seconds, effective max_wall_seconds)` for an Escalation. "Effective" means after `limits.wall_seconds` narrows it (`policy/mod.rs:2568`), so a contract with a 60 s wall caps each repair's floor at 60 s; its work can then reach about 60 s × `max_attempts`, which ADR-0014 states. Setting a key to `0` turns the floor off for that kind.
- **One check, at attempt start.** It runs before the existing `exhausted()` check (`attempt.rs:206`). An attempt that work alone would have stopped can still launch when `total` covers the floor.
- **Each attempt is guaranteed separately.** The overrun of one run is at most `(max_attempts − 1)` floors, and is always within `total`. With default settings (3 attempts, `total` = baseline + work + 1800 s + 600 s of wait budgets) that is 1500 s at most.
- **Recorded.** The `worker_dispatched` transition detail gains `work_extended_s` (absent when there was no extension). `relais explain` prints only a transition's `detail` string (`cli/status.rs:170-177`), so when there is an extension the transition JSON also carries `detail`: "a repair was extended by 70 s to its 600 s floor (`[execution] min_repair_seconds`), paid from the total budget". No rendering code changes. A refused launch ends `budget_exhausted` under `Reason::LimitReached`, with a new `Limit::RepairFloor` whose JSON reads `"limit": "total", "cause": "repair_floor_exceeds_total"`. Its detail says, for example, "a repair needs 600 s (`[execution] min_repair_seconds`); the run's total budget has 212 s left; raise `limits.total_seconds` or set `min_repair_seconds = 0`", followed by the kept-candidate sentence when there is a candidate (`engine.rs:1088-1089`).
- **Verification after an extension.** The extension covers the worker only; the repair's verification clips to what is left (`budgets.rs:289`). A test records what that verification is clipped to.
- **No new `Reason`.** An older binary can still read the ledger (`ledger/transitions.rs:699` treats an unknown reason as corruption).
- **Policy hash unchanged.** Both keys default via the `skip_serializing_if` pattern, so `authority_hash` stays the same for every repository at default values.

## Non-goals

- The machine's repair and escalation decision rules, and `max_attempts`.
- The review floor's silent override of `total` (`review/engine.rs:730`). It is noted for a follow-up, not changed here.
- Initial attempts, and the pre-package check (`scheduler.rs:675`).
- Per-repository values of `max_wall_seconds`, already done in #426.

## Code review

- relais: backend, lang:rust (unix and tui left out: no CLI flag or output format changes; architect left out: no new module, crate or repository)

## Phases

- [x] Phase 1 — Budgets can extend work: `crates/relais/src/policy/budgets.rs`
  - `Spent.work_extension: i64` and `pub fn extend_work(&self, by: Duration)`, converting with `i64::try_from(by.as_millis()).unwrap_or(i64::MAX)` and `saturating_add`. `work_remaining()` reads `limits.work + work_extension`; `total` is untouched.
  - Unit tests with the existing `ManualClock`: an extension lengthens `work_remaining` and leaves `total_remaining` alone; `exhausted()` returns `Work` only after the extension is spent too.
- [x] Phase 2 — Policy keys: `crates/relais/src/policy/mod.rs`, `crates/relais/src/route/candidate.rs`
  - `ExecutionPolicy.min_repair_seconds: u64` (default 600) and `min_escalation_seconds: u64` (default 900), each with `default = …` and an `is_default_*` `skip_serializing_if` predicate, as for `widen_scope_small_max_files` (L470-474).
  - `EffectiveAuthority` carries both, already capped at the effective `max_wall_seconds`.
  - `route/candidate.rs:753-808`: both keys are fixed fields, so a tuned candidate that changes them is refused with `FixedRecipeFieldChanged`, like `verify_timed_out`.
  - Tests: the authority hash is unchanged at default values (like `verify_timed_out_keeps_the_authority_hash_at_its_default`, ~L3052); a candidate that changes either key is refused; a contract with `limits.wall_seconds = 60` caps both floors at 60.
- [x] Phase 3 — Apply the floor: `crates/relais/src/runner/attempt.rs`, `crates/relais/src/runner/engine.rs`, `crates/relais/src/runner/machine.rs`, `crates/relais/src/runner/scheduler.rs`
  - **Prologue (go or refuse).** In `ceiling_reached` (`attempt.rs:206`, where `kind` is known; see `:1409`), before `exhausted()`: if `kind` is Repair or Escalation, the floor is above zero, and `total_remaining < floor`, refuse. A refused launch writes no attempt or dispatch row. When the total covers the floor, a `Work` exhaustion no longer stops a Repair or Escalation here.
  - **Refusal path.** `Limit::WallClock` is a unit variant with a fixed sentence (`machine.rs:820-823`), and `end_on_exhausted` takes no detail (`engine.rs:1066-1074`). Add `Limit::RepairFloor { kind, floor_s, total_left_s }`, matched where `Limit` is (`machine.rs:1193`, `scheduler.rs:678`), built like the AdmissionWait branch (`engine.rs:1076-1094`). It keeps `Reason::LimitReached` and the `budget_exhausted` state. Its JSON is `{"limit": "total", "cause": "repair_floor_exceeds_total", "floor_s", "total_left_s"}`, so a reader of `limit` gets the total that refused it (ADR-0010) and no new `Reason` is stored.
  - **Extension at launch.** In `dispatch_attempt`, just before the `worker_dispatched` transition (`attempt.rs:361`), so its detail can carry the amount: work out the shortfall `floor − work_remaining` then and call `extend_work` with it. At `remaining_wall` (`:375`), launch with `remaining().max(floor.min(total_remaining()))`, so the milliseconds between the transition and the launch cannot shave the floor.
  - The `worker_dispatched` detail (`attempt.rs:361-369`) gains `work_extended_s`, the amount applied at launch, and, when there is an extension, a `detail` sentence naming the floor key. A test asserts that the `explain` output contains it.
  - Tests in `crates/relais/src/runner/tests/budgets.rs`, driven by a scripted ledger clock where the fixture allows (`tests.no-real-waits`; budgets.rs:15-17), which today run real-time budget scenarios (`wall_seconds_caps_the_work_and_nothing_else`, L205):
    - first worker leaves its check red and uses most of a small work budget (e.g. `max_wall_seconds = 4`, `min_repair_seconds = 3`, the slow-idle worker for 3.5 s): the repair's `LaunchSpec.wall_timeout ≥ 3 s` (asserted as `tests/mod.rs:9471` does), and the detail carries `work_extended_s`;
    - the same with `limits.total_seconds` too small: no repair is launched, the run ends `budget_exhausted` and the detail names the floor;
    - `min_repair_seconds = 0`: today's behaviour, the repair gets the remainder;
    - an Initial attempt is never extended;
    - an escalation uses `min_escalation_seconds`.
    - `wall_seconds_caps_the_work_and_nothing_else` (L205) is updated: it sets `min_repair_seconds = 0` to keep its refusal, and a sibling asserts the 1 s extension.
- [ ] Phase 4 — Decision and docs: `.adr/records/0014-a-repair-gets-a-guaranteed-wall-time.md`, `.adr/records/HEADS.md`, `docs/SPEC.md`, `CHANGELOG.md`
  - ADR-0014 supersedes the `execution.resource-budgets` head (ADR-0010). The head keeps the four budgets and adds: a repair or escalation starts with at least its floor; the work budget is extended by the shortfall within `total`, and is recorded; otherwise the attempt is not launched.
  - SPEC: §9 L381 (work budget), §10 L438 (the "smaller of its own timeout and what is left" sentence gains the exception), and the `[execution]` key reference.
  - CHANGELOG `## Unreleased`.
  - `amont agents-md` if the generated block moves.

## Verification

- `make check` in the worktree: exit 0. Record the test count before and after, and list the new tests by name.
- **Real-input check, on the real binary but a scratch ledger.** Build the branch and use a scratch git repo with `RELAIS_STATE_DIR` set to a scratch directory (never the real ledger). The contract is a `change` whose check is a script that fails until a file holds a marker. The policy sets `max_wall_seconds = 120` and `min_repair_seconds = 90`. A native worker instructed to stall about 100 s before giving up on attempt 1 then leaves about 20 s of work.
  - Expected: the repair's `worker_dispatched` detail shows `work_extended_s ≈ 70`, the repair is launched with a wall time of at least 90 s, and `relais explain` prints the extension.
  - What it catches: a floor that is computed but not applied to the launch.
- **Real-input check of the refusal.** Same setup, with `limits.total_seconds` set to baseline + 150 s (above the first attempt's ≈100 s, below 100 + 90 s).
  - Expected: `budget_exhausted` with the floor detail, and no repair dispatch in the `dispatches` table.
  - What it catches: a floor that overrides `total`.
- **Persisted-format risk.** A ledger written by the branch is read by the released v0.53.0 binary (`relais explain <run>` succeeds), proving no new `Reason` value reached the ledger.
- **After release and install.** A dated check 14 days later recounts, for repair and escalation attempts, `process_crash` with `timed_out` against the 22 in the 30 days to 11-09 (about 5.1 a week). It also counts `work_extended_s` transitions and the outcomes of extended attempts.
  - Success: at most 1.5 leftover-cut repairs a week.
  - Roll back by setting both keys to `0` if, over at least 6 extended attempts, more than half still end timed out or failed.

## Decision log

- 2026-11-10 — The floor sits at attempt start, not in the machine's decision, because only the attempt prologue has both `kind` and the budgets (`attempt.rs:268`), and `decide` is pure over counters.
- 2026-11-10 — Default floors are 600 s and 900 s, from the p75–p90 of successful repair and escalation attempts in the table above. They are capped by the effective wall time, so tests and small contracts are unaffected.
- 2026-11-10 — The extension is recorded as a detail field, not a new `Reason`, so ledgers stay readable by older binaries.

<!-- panel: repos=relais reviewers=backend,lang:rust,tui,unix body-sha=6f67ee6c657c -->
