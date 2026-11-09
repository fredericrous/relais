---
status: active
branch: feat/loop-breaker
repos: [relais]
adrs: []
---
# relais: loop breaker for native workers

## Review panel

👉 **Decide:** none — approve if the bet is that a stop-and-advance-one-rung loop breaker, shipped off with a shadow phase first, is worth the new Observation and Reason.
📍 relais · plan reviewed, no code yet · next: fill the 30-day baseline, then Change 1 (`parse_tool_events`). Panel: backend, lang:rust, unix, tui, architect, po (all opus; codex: not run).
**Changed by review:** the shadow phase and rollback bars; the Stop-wait-then-fence rule; the loop text carried into the next attempt's prompt.
**Verdicts:** 6 approve-with-changes in round 1, 6 approve-with-changes in round 2, then a rebind to the final body: 3 approve, 1 resolved, 2 approve-with-changes; 0 rework. Medium and low findings were applied or carried (listed below).

📄 Full reviews: [2026-11-08-loop-breaker-native-workers.reviews.md](2026-11-08-loop-breaker-native-workers.reviews.md)

## Picture

```
 today                          after this plan
 -----                          ---------------
 worker --same failing cmd-->   worker --same failing cmd--> [loopguard]
   x N, no edit                   x N, no edit                  |
   |                                                            v
   v                            stop agent, wait for Stopped
 wall timeout                          |  (no ack: Lost, tree fenced)
   |                                   v
   v                            WorkerLooped --> decide: next rung
 Interrupted (run ends)           (repair effort or escalation),
                                  or Failed when none remains
```

A looping worker used to run until the wall timeout and end the run as interrupted, and after this plan it is stopped at the Nth repeat and the next ladder rung takes over.

## Context

A native relais worker that repeats the same failing Bash command, with no edit between, keeps running until `max_wall_seconds` (1200 s default). The effort ladder (`route/ladder.rs`) never moves, because the machine only advances on a verification outcome. The idea comes from quadius/greenharness (loop breaker: same failure N times, no file change, deny). Its other mechanisms are out of scope here.

Decisions made with the person: on a loop the attempt **advances one rung** (repair with higher effort, or escalation once repairs are used up). Progress is judged **from the transcript only**, with no git calls in the poll.

Baseline to fill before the code lands (the upper bound on runs this can rescue): runs in the last 30 days that ended `process_crash` with `timed_out: true`, and how many of their transcripts show one failing Bash fingerprint 3 or more times with no edit. If fewer than 3 runs match, ship only the shadow detector and revisit after 30 more days.

## Findings that shape the design

- Relais never launches the worker. The plugin spawns the Claude Code subagent, and `adapter/native.rs::await_stop` (≈678) polls the coordinator, which knows only coarse states. The per-tool-call record is the subagent transcript JSONL (`read_transcript` ≈883), available once the agent id is `Bound`; on `Route::Continue` the id is known before the first poll.
- `orchestration/transcript.rs::parse_transcript` (232) keeps only assistant usage. No Rust code parses `tool_use` / `tool_result`. A sampled transcript has `tool_use{name,id,input.command}` and `tool_result{tool_use_id,is_error}` blocks.
- Killing the worker today ends the run: `stop_agent` returns `Ended::TimedOut`/`Exited(1)`, which becomes `Observation::TerminalResultMissing`, then `State::Interrupted` (`runner/machine.rs::decide` 402). A new Observation, checked before `terminal_result_missing()` (`runner/attempt.rs` ≈521), is required for the ladder to advance.
- `stop_agent` publishes Stop and returns without waiting. Today every stop ends the run, so nobody races it; with a next attempt, the old agent could still write while the lease is handed back (`runner/launch.rs:313-338` fences only the Lost path).
- A stopped agent is never continued, so the next attempt spawns afresh and has no memory of the loop. The repair prompt is built from `RepairFailure` (`runner/prompt.rs:109`).
- `VerificationFailed` ends `Failed/RepairExhausted` and gates escalation on `escalation_authorized()` (`machine.rs:470-509`); `budget_exhausted` is a separate `LimitReached` end. SPEC §9 says a stronger model for a worker that changed nothing is not a repair (`machine.rs:439`): this plan is a stated exception.
- `Reason::ALL` has 68 entries and `State::ALL` 14 (`lifecycle.rs` 210, 388), with round-trip tests. The ledger schema is v48, checked strictly; an older binary refuses a newer ledger.
- `ExecutionPolicy` is `deny_unknown_fields`: an older relais refuses a policy that names a new key. A new key must be hash-neutral when unset (`skip_serializing_if`), as `repair_effort` is.

## Changes

1. **Tool-event parsing** in `orchestration/transcript.rs`, beside `parse_transcript`: `parse_tool_events(&str) -> Vec<ToolEvent>` (Bash command, `tool_use` id, `is_error`, first error line; Edit-family uses). It is the one parser of the transcript format.
2. **Detector, pure** in new `adapter/loopguard.rs`, no I/O, consuming `ToolEvent`s. `LoopGuard` is a stateful struct: pending `tool_use` ids carried across polls (a `tool_result` can arrive in a later poll than its `tool_use`), and a count per fingerprint (a map).
   - Normalise a command by trimming and collapsing whitespace outside quotes; pipes and arguments are kept.
   - Fingerprint a failing Bash result as (normalised command, first non-empty line of the error result). A pipeline ending in `tail|head|grep` exits with that last command's status, so it is fingerprinted as (normalised command, hash of its output) whatever `is_error` says.
   - Progress clears every count: `Edit` / `Write` / `MultiEdit` / `NotebookEdit`, or a mutating Bash command judged on shell words (quotes respected), never on a substring: `sed -i`, `perl -pi`, `rm|mv|cp|touch|tee|patch`, `git commit|add|checkout|apply|stash|reset|restore`, `cargo fmt`, `npm|pnpm|cargo` install or add, and a `>`/`>>` redirect whose target is a file path (not `&N`, not `/dev/null`). `2>&1` and `>/dev/null` are not progress.
   - A successful result of a command clears only that command's fingerprints. Different commands in between do not clear anything.
   - The threshold is N (default 3, minimum 2); fires when one fingerprint reaches N.
3. **Transcript tail in `await_stop`** (`adapter/native.rs`): once `running` is `Some`, find the transcript path once with `read_transcript`'s logic, keep the `File` open and seek from the stored offset (start again from 0 if the file got shorter), moving the offset forward only to the last `\n`. On `Route::Continue` start at the file's current end, so earlier attempts do not seed streaks. A missing or unreadable transcript means no detection and no error. No `read_dir` per 250 ms poll.
4. **New end state** (`adapter/native.rs`, `adapter/mod.rs`, `backend.rs`). Add `Awaited::Looped { agent, command, count }`. On it, `await_stop` calls `stop_agent`, then keeps polling, bounded by `waits.spawn`, until the coordinator reports `Stopped` or `Failed`. If it does not, it returns the result with `lost_worker` set and `looped` left `None`, so the existing `fence_lost_worker` holds the tree instead of a new attempt starting in it. `LaunchResult` gains `#[serde(default)] looped: Option<LoopedWorker>`, set at every constructor: `backend.rs:590,639`, `adapter/mock.rs:131`, `adapter/native.rs` (`ended_result`, `stopped_result`), `runner/tests/native.rs:1051`. Usage is booked from the transcript through `parse_transcript` + `usage_of`, with its message ids in `booked_message_ids` so `usage import` does not count them twice.
5. **State machine** (`runner/attempt.rs`, `runner/machine.rs`, `lifecycle.rs`). In `attempt.rs` check `result.lost_worker` first (it ends as today), then `result.looped` before `terminal_result_missing()`, skip verification and keep the worktree. Add `Observation::WorkerLooped`: `decide` returns the shape of `VerificationFailed`: `Next::Attempt` at `budget.rung.next()`, Repair or Escalation by the existing `repairs_used >= max_repairs` and `escalation_authorized()` rule. When neither repair nor authorised escalation remains the run ends `Failed` with reason `WorkerLooped`. Add `Reason::WorkerLooped` (`ALL`, `as_str`, `parse`, round-trip tests) and a row in the `machine.rs:1042` observation table test.
6. **What the next attempt and the person see.** `RepairFailure::Loop { command, error_line, count }` in `runner/prompt.rs`, rendered in the repair addendum, since the fresh spawn has no memory of the stopped agent. The transition `detail` is `worker_looped: <redacted command> x<count>`, shown by the pane (`claude-plugin/hooks/timeline.ts:276`, final line 587) and by `relais report`. On a stop, one stderr event line: `relais: attempt <n> stopped: worker repeated <command> failing <count> times with no edit; next: <rung>` (or `next: none (run failed: worker_looped)` when no rung remains). The command text always goes through `router::capture::redact` (`router/capture.rs:116`) before it is recorded or printed. In the stderr line and the detail it is collapsed to one line, control bytes escaped, and cut to exactly 60 characters (chars, not bytes), the last one `…`, in the stderr line and the pane; the transition `detail` (a JSON field, no schema change) keeps the full redacted text.
7. **Declaration** (`policy/mod.rs`). `[execution] loop_threshold: Option<u32>`, `skip_serializing_if` when `None` so `authority_hash` is unchanged. A value below 2 is refused at parse time with the key named. Clamped in `EffectiveAuthority` (≈2470): the machine and the contract may only raise it (stop later) or disable it, never lower it; SPEC §5 states this direction. Report it in `relais doctor` beside repair effort (`cli/doctor.rs:23-39`).
8. **Docs.** `docs/SPEC.md` §5 (`loop_threshold` counts identical failing Bash results with no edit between; minimum 2), §9 (observation to transition table; the exception to "a stronger model for a worker that changed nothing is not a repair"; the exit code of a run that ends `Failed/WorkerLooped`), §23. A comment on the key in the repo's `relais.toml`. `CHANGELOG.md`.

## Rollout

Ship with `loop_threshold` unset. While unset, the detector still runs in shadow: it records what it would have fired on, as a `relais: shadow: would stop attempt <n>: <redacted command> x<count>` line in the run's events (same one-line escaping as Change 6), and never stops the agent. No ledger event kind and no migration: the schema stays v48, because a new kind needs v49, which an older binary refuses and which cannot be rolled back. The shadow detector uses the default threshold (3), so it measures the N that would be enabled. Shadow events are appended to each run's `events.jsonl` (`reap.rs:252`) and counted with `grep -h 'relais: shadow:' <runs-dir>/*/events.jsonl | wc -l`. Enable the key in this repo's `relais.toml` only when (a) shadow has recorded at least 5 events and at most 1 in 5 of them had its worker pass verification in the same attempt, and (b) this release is the installed binary and the CI pin (`deny_unknown_fields` makes an older relais refuse the key).

Measures for the first 10 stops: rescued runs (looped, then accepted at a later rung); wall seconds between the stop and the 1200 s timeout; false positives, meaning shadow events whose worker then passed verification in the same attempt, plus a hand-labelled sample of the first 10 stops. Roll back (unset the key; never by installing an older binary, which calls a stored `worker_looped` reason corrupt, `ledger/transitions.rs:699`) if more than 2 of the first 10 hand-labelled stops are false positives, or no stopped run is rescued after 10 stops. A next rung that fails the same way is not a false positive: the stop was right.

## Verification

- `loopguard` unit tests: three identical failures stop; an edit between clears; a mutating Bash clears; `2>&1` and `>/dev/null` do not clear; different commands interleaved still count; a successful result of the same command clears only it; `cargo test 2>&1 | tail -40` repeated with the same output stops; N=2 minimum, below 2 refused; malformed lines ignored; a partial last line is held for the next poll; a `tool_result` in a later poll than its `tool_use` pairs.
- `parse_tool_events` tests on a real transcript sample.
- `machine.rs` row test, `lifecycle.rs` round-trip, and a prompt test that the repair addendum carries the loop text.
- `runner/tests/native.rs` scripted plugin: it appends looping lines and never sends `stopped`. Expect Stop, then a second attempt at the next rung with the loop text in its prompt, and no tool call of the first agent landing after the second attempt started. Second test: Stop never acknowledged ends Lost and fenced, with no second attempt. Third: `Route::Continue` with an older loop in the transcript does not fire. Fourth: key unset runs to the wall timeout as today (and records shadow).
- Pane and ledger: the repair line and the final line show the command and count; a planted `Bearer xyz` in the command is redacted; reading the ledger still reports v48.
- Policy: an `[execution]` without the key keeps its `authority_hash` byte for byte; `loop_threshold = 1` is refused with the key named; a contract asking for a lower threshold than the repo's is clamped to the repo's value.
- Live check, before enabling: one real native worker on a task whose verify command cannot pass, bare and piped. Confirms the transcript shape and that `is_error` is set on a non-zero Bash exit; if not, fingerprint on the exit-code text.
- `make check`, then land through the PR flow.

## Open risks

- Transcript flush timing: the detector sees a call a poll late. Acceptable.
- An older relais calls a stored `worker_looped` reason corrupt (`ledger/transitions.rs:699`), so a binary downgrade breaks salvage of those runs. Rollback is by unsetting the key.
- Whether the plugin's Stop reports `AgentStatus::Killed` for a self-initiated stop: the `Looped` path depends only on the coordinator leaving the running states.
- `is_error` semantics for Bash non-zero exits are unverified (live check).
- The output-hash fingerprint for piped commands misses loops whose output varies (timestamps, durations). That is safe; the shadow count shows how many are missed.

## Non-goals

- Output distillation, read dedupe, de-escalation and daily budget guards from the same comparison.
- Detecting loops in non-native (probe, check or verification) commands: `procs.rs` already bounds those.
- Any git call in the poll loop.
- Moving the detector into the plugin: the decision stays in Rust so `decide` stays a pure function.

## Code review

- relais: backend, lang:rust, unix (tui left out: the output change is one stderr line and a pane detail, covered by unix; architect left out: no new crate or repository)

## Outcome

Observed on branch `feat/loop-breaker` before the push, per check (input → expected → actual).

- **Baseline** (the 30-day count, from the ledger and the saved subagent transcripts): the 56 runs that ended `process_crash` with `timed_out: true` → how many show one failing Bash command 3 or more times with no edit → 0 of the 22 whose transcripts were still on disk (34 had none). That is under the plan's bar of 3, so the stopping path ships **off**: only the shadow detector runs, and `loop_threshold` stays unset in this repository. The count is a lower bound on a partial sample; `is_error` was confirmed set on failing Bash results in real transcripts (91 of 1284).
- **Decision log**: the plan reads "if fewer than 3 runs match, ship only the shadow detector". The stopping path is in the release but switched off (the key is unset), so what runs by default is the shadow detector alone. That reading is deliberate: the code is the same either way, and the key is the switch.
- **Detector and parser**: unit tests for every rule in Change 2 (three identical failures, an edit between, mutating Bash, `2>&1` and `>/dev/null`, interleaved commands, success clearing only its own fingerprint, a piped `tail`, heredoc bodies and `#` comments, partial last line, a result in a later poll than its use) → pass → pass.
- **Scripted plugin** (`runner/tests/native.rs`): a looping worker is stopped and the next attempt runs at the next rung with the loop text in its prompt; a Stop that is never acknowledged ends lost and fenced with no second attempt; `Route::Continue` with an older loop in the transcript does not fire; what a stopped agent writes later is not booked to the next attempt; key unset runs to the wall timeout and prints exactly one shadow line; a contract `0` prints none; a transcript that is never found says so once → pass → pass.
- **Policy**: an `[execution]` without the key keeps its authority hash; `loop_threshold = 1` is refused with the key named; a contract may raise the value or turn the guard off with `0`, never lower it → pass → pass.
- **Deviation from Change 7**: the plan said the machine and the contract may raise or disable the value. No machine-level key exists in this plan, so only the contract does; SPEC §5 says so.
- **Not observed**: the fence of the tree after a confirmed stop. The harness shows that a stopped agent's late transcript line is not booked to the next attempt, but it cannot tell a late file in the tree apart, so "no tool call of the first agent lands after the second attempt started" rests on the plugin's `Stopped` meaning the agent has really stopped (the live check). The give-up time of the transcript lookup is scaled in test builds (3 lookups, 20 in production), so the production figure of about 20 s is not shown by a test. The live check (a real native worker on a task whose verify command cannot pass, bare and piped) and the claim that the shadow line reaches `events.jsonl` in a real run. Both are required before the key is enabled, and neither is needed for the shadow-only release.
- **Carried, not done**: a `LoopThreshold` newtype and a serde-tagged transcript block (rust profile); an injected sink in place of the test-only fields; counting a piped filter only when its output also matches a failure marker (decided by the shadow data); a `relais report` count of shadow events.

<!-- panel: repos=relais adds=lang:rust reviewers=backend,lang:rust,unix,tui,architect,po body-sha=3051cf24ecd7 -->
