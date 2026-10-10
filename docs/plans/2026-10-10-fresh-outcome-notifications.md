---
status: active
branch: fix/fresh-outcomes
repos: [relais]
adrs: []
---

# Fresh outcome notifications

## Review panel

👉 **Decide:** none — approve if checking the ledger at delivery, failing open, is the right place to stop stale outcome messages.
📍 relais · plan reviewed, nothing built · next: worktree, Phase 1 read-only ledger open. Panel: backend, rust, tui, unix.
**Changed by review:** ledger opened read-only (no create/migrate, 250 ms busy); rewrite only on a recorded resolution, headline without `detail`; entry kept until delivered (retry-safe).
📄 Full reviews: [2026-10-10-fresh-outcome-notifications.reviews.md](2026-10-10-fresh-outcome-notifications.reviews.md)

**Verdicts:** round 1: 4 approve-with-changes; round 2 backend approve-with-changes, final body approve. Two low test refinements carried into implementation (see Full reviews).

## Context

The plugin turns a run's `done` line into a frozen outcome message, queues it in `store.verdicts`, and submits it from the 100 ms pump, one at a time (the v0.11.2 `isSubmitting` guard). Nothing checks whether that outcome is still true when the message goes out.

Incident, 2026-10-09, session 16c85633: `run-65d6671dde41e-6d1a` ended `needs_decision (verification_inputs_changed)` at 11:32:24. The same session approved it at 11:34:19 (`decision_approved`, run now `accepted`). The outcome message was submitted at 11:42:10.076 and delivered at 11:42:10.259, telling the model "This is yours to decide". The transcript shows why: four outcomes were enqueued one by one, each about 20 ms after the previous one was delivered. So the text sat in the plugin's own queue for ten minutes, behind an earlier message the host held until the session went idle.

A second source of staleness: `relais native status`, the plugin's only status source, builds its answer from `events.jsonl` alone (`native_status_command`, `crates/relais/src/main.rs:3540`). `relais decide` records its answer only in the ledger (`resolve_decision`, `crates/relais/src/ledger/mod.rs:3020`). So status still says `needs_decision` for that run today. The plugin cannot learn the current state from anything it can read.

Intended outcome: a message announcing a state that waits on a person (`needs_decision`, `needs_review`, `interrupted`; `State::awaits_a_person`, `crates/relais/src/lifecycle.rs:430`) says what is true when it reaches the model. If someone already answered it, the message says so and gives no instruction to act. No outcome is ever lost: if the check fails, the original message is delivered.

## Host constraints (read from the 2.1.296 plugin API types)

- `$.prompt.submit` runs the `prompt.submit` event "once the session is idle", through every hook except the calling one, with `e.origin = { kind: 'plugin', name }`.
- A `prompt.submit` hook may rewrite with `next({ ...e, text })` or stop with `{ drop }`.
- No API cancels or edits a prompt the host has already queued. Freshness is therefore enforced in our own `prompt.submit` hook, the last point our code sees the text.
- The incident's ten minutes were in the plugin's queue, which the hook covers, because the submit call happens at the end of that wait. Whether the hook also runs after the host's own idle wait (for the queue head) is not stated precisely. Phase 3 measures it, and the residual window is reported, not assumed away.

## Phase 1: current state in `relais native status` (Rust)

- New `Ledger::open_read_only` in `ledger/mod.rs`, beside `Ledger::open` (`:2149`). `open` is not a read: it creates the state directory and the file, switches to WAL and runs `migrate`. The new constructor uses `SQLITE_OPEN_READ_ONLY`, creates nothing, writes no pragma, does not migrate, and sets a 250 ms busy timeout, not the 5 s `LEDGER_BUSY_TIMEOUT` (`:1565`). Missing file, or a schema version other than the binary's: it reports "cannot tell" and does not open.
- `native_status_command` (`crates/relais/src/main.rs:3540`): after building the timeline, add one key, `current`. It holds `{ "state": <Ledger::run_status>, "decision": <Ledger::decision_of_run or null> }`; the decision carries `raised_state`, `raised_reason`, `resolution`, `actor` and `resolved_at`. Reuse `run_status` (`:2340`) and `decision_of_run` (`:2994`); add no new SQL.
- `outcome` keeps meaning "how the run ended" (event-derived, unchanged); `current` is "what holds now". Existing readers (`rebuildRuns`, `reloadTimeline`, the status tool) ignore the new key.
- Run unknown to the ledger (normal for a run refused before it started): omit `current`, silently. Ledger missing, unreadable, busy past 250 ms, or schema mismatch: omit `current` with one stderr line. The timeline prints as today and the exit code is 0 in every case.
- `docs/SPEC.md:707`: document `current` and both omission rules in the same commit.
- Tests:
  - a run with a resolved decision prints `current.state = "accepted"` and the resolution;
  - a run with an open decision prints `resolution: null`;
  - for each of `needs_decision`, `needs_review` and `interrupted`, `current.state` serializes to the same string the `done` line's `outcome` uses, so the plugin's comparison cannot drift;
  - with an empty scratch state directory, the call exits 0, prints the timeline without `current`, and leaves no ledger file behind;
  - while a second connection holds `BEGIN IMMEDIATE` (WAL: readers are not blocked), the call returns within 300 ms *with* `current`;
  - while a second connection holds `PRAGMA locking_mode=EXCLUSIVE` after a write, the call returns within 300 ms *without* `current` (the 250 ms path);
  - on a populated WAL ledger with every writer closed (no `-wal`/`-shm` files, the normal state between runs), the read-only open succeeds and prints `current`.

## Phase 2: check at delivery in the plugin (TypeScript)

- `claude-plugin/hooks/store.ts`: add `pendingOutcomes: Map<string, { run: string; outcome: string; code: string | null }>`, keyed by the exact message text, like `ownPrompts`.
- `claude-plugin/hooks/runs.ts` `onDone` (line 251): when `outcome` is one of the three person-awaiting states and `line.run` is a string, record the entry beside `store.verdicts.push(text)`.
- New `freshen(fx, store, text)` in `runs.ts`. If the text has no entry, return it unchanged. Otherwise run `relais native status --run <run>` (the call shape of `reloadTimeline`, `runs.ts:322`) with its own `FRESHEN_TIMEOUT_MS = 1_000`, not the 10 s `STATUS_TIMEOUT_MS`, because the call sits on the path to the session's next turn. Then read `current`.
  - Rewrite only when `current.decision.resolution` is non-null, meaning a person answered. A state that differs without a resolution, a null decision, a missing `current`, a non-zero exit, a timeout or bad JSON all leave the text unchanged (fail open: a lost outcome is worse than a stale one).
  - The rewrite rebuilds the headline without the runner's `detail`, because the instruction "This is yours to decide" lives in `detail` (`runner/mod.rs:2393`) and the headline carries it (`runs.ts:267-271`): `relais run <id> finished: <outcome> (<code>). Since then: <resolution> by <actor> at <resolved_at>; the run is now <state>. Nothing is waiting on you for this run.`, with the parenthesis left out when `code` is null. It keeps the receipt, changed summary and replay lines, and drops `NEXT_STEP[code]`.
  - Each freshened or failed-open outcome writes one line to the pane through the existing `note` (`runs.ts:73`), so the check is visible in production. A missing `current` (an installed binary older than Phase 1) is noted once per session, not once per outcome.
  - `freshen` does not delete the entry.
- `submitVerdicts` (`runs.ts:65`): delete the `pendingOutcomes` entry right after `store.verdicts.shift()`, so a failed submit retried on the next tick is checked again. The single-flight guard and retry-on-failure are otherwise unchanged.
- `claude-plugin/hooks/register.ts` `prompt.submit` hook (line 376): for any text with a `pendingOutcomes` entry (keyed by text, like `ownPrompts`; no new origin test), `return next({ ...e, text: await freshen(...) })`. `isOwn` and `onPersonPrompt` keep reading the original text first, which consumes the `ownPrompts` mark. The rewrite is not added back to `ownPrompts`: no hook of ours sees it again, and re-adding it would leak a mark. Wrapped so any throw falls through to `next(e)`.
- Tests in `claude-plugin/tests/outcome.test.ts`, using `script.runResult` (`tests/support.ts:109`) for the status reply and `submitHold` for the busy session. The harness runs our hook before the hold (`support.ts:185-193`), so a held queue head was checked before the hold. The incident is therefore modelled with the stale outcome second in the queue; the host's own idle wait for the head is not modelled here, and Phase 3 measures it.
  1. The incident: outcome A is delivered and held. The `needs_decision` outcome B queues behind it. Status for B is switched to `accepted` with a resolution, then A is released. B's delivered text contains "Since then: decision_approved" and not "yours to decide". `ownPrompts.size` and `pendingOutcomes.size` are 0 afterwards, and exactly one status call was made for B. A variant with `code: null` has no empty parenthesis.
  2. Still waiting: status replies with an open decision; the original text is delivered byte for byte.
  3. Fail open: status exits non-zero, times out or lacks `current`; the original text is delivered, with one pane note and no repeated warnings.
  4. `accepted`, `blocked` and `failed` outcomes make no status call.
  5. Retry: `submitFailures = 1` with status `accepted`; the retried delivery is the freshened text.
  6. The v0.11.2 test (a pending submit is not resubmitted) still passes.

## Phase 3: land, release, observe

- One branch and PR in `relais` (worktree via `worktree-task`): Phase 1 and 2 as reviewable commits, `amont agents-md` result in the same PR if it changes.
- Verification before push, matching CI: `make check` (fmt, clippy `-D warnings`, module-cycle check, `cargo test`, MSRV build, audit, and `make plugin`, which validates and tests the plugin). Implementation review (F4b), then merge on green.
- Release v0.11.3 with `tag-release` (the fix only reaches sessions through a released plugin). Changelog names the person's step: `relais install --claude --user --write`, then restart Claude Code sessions.
- Measure before release: the p50 and p99 of `relais native status --run <id>` over 20 calls on the live ledger, and again while a run is writing. Target: under 300 ms.
- Observe live, same machine: start a small relais run from a session, keep that session in a long turn past the run's end, answer the decision from a second session with `relais decide`, then let the first session go idle. Expected: the delivered message carries "Since then: …". Record from the transcript and the pane note the enqueue, hook-run and delivery timestamps against the decide time, for a queue-head outcome and for a queued one. This establishes whether a decision made while the head prompt is held by the host is also caught. If it is not, report the residual window and its size; no further change in this plan.

## Not in scope

- The `status` tool's in-memory model still shows the ended state after a decision; a follow-up can read `current` there.
- Provider substitution (`unapproved_substitution` with a blank model), learned routing, worktree retirement.

<!-- panel: repos=relais adds= reviewers=backend,lang:rust,tui,unix body-sha=163b82211d32 -->
