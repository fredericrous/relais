---
status: done
branch: feat/compact-status-results
repos: [relais]
adrs: []
---
# relais: fewer and smaller status results

## Review panel
📄 Full reviews: [2026-11-09-compact-status-results.reviews.md](2026-11-09-compact-status-results.reviews.md)

👉 **Decide:** none — approve if a small, cheap win is worth doing (status is 0.5% of tool text; the Read/Bash volume stays with the deliberate-output plan).
📍 relais · plan reviewed, nothing built · next: `worktree-task` branch, Phase 5 freeze first. Panel: backend, lang:typescript, tui, unix (all opus); codex not run.
**Changed by review:** `Why` comes from `child.stderrTail`, quoted and only when `detail` is unset; failure lines are kept at write time, not at read time; size target set to about a fifth (90% bound).
**Verdicts:** round 1 four approve-with-changes; round 2 three approve-with-changes (all high findings applied); backend delta on the final body approve; 2 low findings carried into implementation.

## Context

The orchestrating session pays for every relais tool result. Measured over 14 days of
transcripts (2,406 session files): `status` made 556 calls and returned 1.23M chars
(median 1.4k, p90 3.9k, max 46k; `output:true` median 3.8k). Some runs were polled
10 to 25 times. Of 710 `status(run)` calls, about 57 followed that run's outcome message
(rough count, matched on outcome text); most happened while the run was still going.
`status` is only 0.5% of all tool-result text (the 57 post-outcome calls are about 126k chars, roughly 0.05%), so this is a small, cheap win. It is not the
big cost, which is Read (57%) and Bash (25%), covered by the deliberate-output plan.

Root causes found in the code:
- `startRun` returns no run id (`claude-plugin/hooks/runs.ts:235`; the id is assigned
  by the first event, `onEvent` `:459`). The only way to get it early is `status` with no
  `run` (165 listing calls). The outcome headline already carries the id (`:496`).
- SKILL.md step 4 (`claude-plugin/skills/relais/SKILL.md:142`) opens with "Progress: call
  `mcp__relais__status`", which invites polling.
- `statusReply` (`claude-plugin/hooks/timeline.ts:608`) returns every phase as an
  object with key names and millisecond durations.
- `output:true` returns a plain tail (`latestOutput`, `timeline.ts:537`), so a failure
  above the last 40 lines is missed and the model asks again.
- The outcome message (`runs.ts:475-511`) carries no stderr line, so a stop such as
  `review_unavailable` sends the model to `status` to learn why.

Intended outcome: fewer polling calls, and each default `status` result about a fifth smaller
(Phase 3 saves key names and milliseconds; the larger saving is the calls not made), with no loss of
what the model needs to act. `output:true` replies may grow by the failure windows; that is the price of
not asking twice.

## Picture

```
 today                                 after this plan
 -----                                 ---------------
 run --> (no id) --> status list       run --> wait
   |                   |                        |
   |   status(run) x N |  (full JSON)           v
   v                   v                 outcome {id, state, code,
 outcome {id, state,                       Why (quoted), changed, receipt}
   changed, receipt}                              |
   |                                              v
   `--> status again: why?               status only if stalled / asked
                                           (one-line phases, failing
                                            lines first)
```

The outcome message used to leave the model to ask `status` why a run stopped, and now it names the reason itself.

## Phases
- [x] Phase 1 — relais: Skill wording. `claude-plugin/skills/relais/SKILL.md` step 4, and `operating.md` if it repeats the advice: do not call `status` while a run goes; the outcome arrives as a message and carries the run id; call `status` with `run` only when the person asks, or the pane shows a stall. Never call `status` just to learn a run id. `output:true` only after a failure whose message does not say why.
- [x] Phase 2 — relais: Outcome says why. `claude-plugin/hooks/runs.ts` `onDone`: for any outcome other than `accepted`, when `detail` is unset (or the code is `review_unavailable`), put the last 2 entries of `child.stderrTail` (`runs.ts:420`) right after the headline as `Why (runner stderr, quoted): "…"`. Each is stripped of ANSI and control characters, collapsed to one line, cut to 160 chars (`cutLine`) and passed through `JSON.stringify`, because the text is submitted as a prompt (`runs.ts:507`). A stderr line that is a substring of `detail` is skipped. Build it outside `facts`: `answeredText` (`:187`) rebuilds from `facts` and must not carry a stale reason, and the `pendingOutcomes` key (`:509`) must stay identical between set and get. Update `outcome.test.ts` cases at `:54, 72, 235, 251, 350` and add: no `Why` on `accepted`, none when `detail` is set, present on a `run: null` refusal, ANSI and `\r` removed, absent from an answered run's rewrite.
- [x] Phase 3 — relais: Compact status default. `claude-plugin/hooks/timeline.ts` `statusReply`: keep the JSON array and the `run` / `phase` / `attempt` / `outcome` / `cost` / `stalled` / `waiting` / `foreign` fields, but render each phase as one string `"<title> · <state> · <detail> · <secs>s"` with empty parts dropped (`filter(Boolean)`, as `timelineText` does at `:593`), drop `maxAttempts` when null, and replace `elapsedMs` with `elapsed_s`. Keep the existing `MAX_STDERR_LINES` (`:7`) and `statusSummaryEntry` as they are. First confirm what the second caller of `statusReply` (`runs.ts:708`) serves, and correct the non-goal below if it is `/relais-status`. Expect only about a fifth off the size from this: the saving is key names and milliseconds, not content. Update `status.test.ts` (`:183-186`, `:81, 211, 229`) and `outcome.test.ts` (`:87-203`).
- [x] Phase 4 — relais: Failure-first `output:true`. `claude-plugin/hooks/timeline.ts` `appendOutput` (`:144`) keeps only the last 40 lines (`:157`), so it also keeps a bounded `failures` buffer per check: lines matching one anchored regex, `/\bFAIL(ED)?\b|^E\s|^error(\[E\d+\])?:|panicked at|^\s*[×✗]\s/` (vitest marks failures with `×`; pytest `XFAIL` must not match), each with 2 lines of context, at most 40. Strip one trailing `\r` from each piece first (CRLF output), then, on the joined line (the open line's text plus the piece, `:151`), keep only the text after the last remaining `\r`, and only then call `cutLine`, so a progress bar cannot push a marker past 240 chars and a CRLF line is not emptied. Context after a match needs a `trailing` counter kept in `Check`, because the next lines may arrive in a later chunk. When more than 40 lines match, keep the first 20 (the root cause) and the most recent 20 (the summary). `latestOutput` (`:537`) returns the failure lines followed by the tail, 40 lines in total, duplicates removed, windows separated by `…`; with no failure lines it returns the tail as today. Table tests go through `appendOutput` with more than 60 lines and the failure near the top: pytest (plus an XFAIL-only and a passing run containing an indented `  error: none` line or a `fail_*` test name, which must return the plain tail), cargo (a panic 120 lines before the end, a `\r` progress bar before `error[E0308]:`) and vitest. Also: a CRLF pytest fixture (`FAILED x\r\n` is matched), a `\r` arriving in the second chunk of one line, a match and its context split at every byte offset (same result as one chunk), and 100 noise lines matching `error:` followed by one real failure (the real one is returned).
- [x] Phase 5 — relais: Measure. Acceptance, not a feature. Do this first, before Phase 3 changes the code (implementation order: Phase 5 freeze, then Phase 3): in `claude-plugin/tests/status.test.ts`, save the current `statusReply` JSON for a fixture run like the sampled one (7 phases, 4 decisions, 2 stderr lines; about 1.7k chars) as a frozen file, plus one captured real run as a second fixture, and assert the compact reply is at most 90% of each frozen reply.
- [x] Phase 6 — relais: Docs. `docs/SPEC.md` §23 (`:747`) one sentence on the reply shape and the failure-first output; a `CHANGELOG.md` `## Unreleased` bullet.

## Non-goals

- No change to the Rust crates, the pane or the quiet-run protocol; `/relais-status` is unchanged unless the `runs.ts:708` check in Phase 3 shows it shares `statusReply`.
- No transport change (MCP stays); no compression library over tool results.
- No change to Read or Bash volume; the deliberate-output plan owns that.

## Code review
- relais: backend, lang:typescript, unix, tui (lang:rust left out: no Rust file changes; architect left out: no new module or contract, and the reply is read only by the model, not by another repository)

## Left open (not decided here)

- Whether `run` should wait for the first event (a second or two) so its result carries the
  id. Not planned: it blocks the tool call; item 1 removes the need.
- Per-run `since` cursor for repeated status calls: skipped until item 1 and 3 are measured.
- The Rust `done.detail` text was not read; if it already carries a reason, item 2 may
  duplicate it. Phase 2 skips `Why` when `detail` is set (except `review_unavailable`); confirm against the Rust text in the implementation.

## Verification

- `make plugin` (validate + `claude plugin test`), `cargo test` for any Rust touched (none
  expected), then the repo's pre-push checks via `amont land`.
- Targets: post-outcome `status(run)` calls under 5 a week (from 57 in 14 days, about 28 a week), and a median of one `status` call per run. Re-run the transcript count one week after release: `status` calls per run (median and
  p90) and chars per `status` result, same script as today's baseline (n=556, 1.23M chars).
- Live check (after release and the person's plugin reinstall): start a real relais run that ends `needs_review`; confirm the outcome message
  names the reason and the session makes no `status` call.

## Process

Check `amont-agent board --repo relais` first; work in a worktree from `origin/main`
(`worktree-task`); run `amont agents-md` if AGENTS.md's generated block changes; merge on
green (`merge-when-green`). Release is separate: a plugin change needs the person's plugin
reinstall.

## Decision log
- Phase 3: the phase string is `<state>: <title> · <detail> · <secs>s`, not `<title> · <state> · …` (implementation review: titles and details contain ` · `, so the state could not be told apart).
- Phase 2: the line reads `Last runner stderr (quoted): "…"`, not `Why (runner stderr, quoted)`, because the code quotes the last lines and does not know they are the cause. It keeps only the last `\r` frame and compares with `detail` before cutting. `cutLine` stays one-argument (`.map(cutLine)` passes an index); `cutLineTo(line, width)` is the new function.
- Phase 4: the reply carries at most 30 failure lines, a third from the earliest matches, so at least 10 tail lines remain; storage keeps 20 + 20 as written. Empty chunks do not close the open line; OSC sequences are stripped by `cutLineTo`.
- Deliberate: `onDone` does not pass the outcome `reason` into the live model (the `rebuildOne` path does). It predates this plan; the outcome message itself now carries the reason. Carried to a later change.

## Verification record
- `make plugin` (validate + 405 plugin tests) → expected 0 failures → observed 405 pass, 0 fail, on the tree before the closing commit.
- Frozen pre-change fixtures (`claude-plugin/tests/fixtures/*.frozen.ts`) → expected compact reply at most 90% → observed: both fixtures pass the 90% assertion in `status.test.ts`.
- Failure-first output → expected the root-cause line survives a 120-line tail for pytest, cargo and vitest, CRLF, split chunks and 100 noise lines → observed: table tests in `status.test.ts` pass.
- Outcome message → expected `Last runner stderr (quoted)` only on an unaccepted outcome without detail (or `review_unavailable`), absent from an answered rewrite → observed: `outcome.test.ts` pass.
- Not observed here: the live `needs_review` run and the week-later transcript count; both wait for the release and the person's plugin reinstall.

## Implementation review
- implementation-review (opus): round 1 approve-with-changes (plan text out of step with the 30-line cap); delta approve-with-changes (tests that could no longer fail, fixed). 70k + 28k tokens.
- backend (opus): round 1 empty chunk closed an open line, fixed; delta ANSI-only lines filtered too late, fixed.
- lang:typescript (opus): round 1 live model drops the outcome reason, `deliberate` (existing, Decision log); delta two low points fixed.
- unix (opus): round 1 `\r` frames, OSC sequences, reinstall note, all fixed; delta approve.
- tui (opus): round 1 ambiguous phase strings, label claimed a cause, both fixed; delta reinstall command in the CHANGELOG skipped (no installer command is documented in this repository).

<!-- panel: repos=relais reviewers=backend,lang:typescript,tui,unix body-sha=880cdbf7e3d9 -->
