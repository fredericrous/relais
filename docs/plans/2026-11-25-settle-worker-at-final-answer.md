---
status: active
branch: prog/follow-ups/f21-settle-at-final-turn-with-shells
repos: [relais]
adrs: []
plan-id: p-01M87Y75NXBZX48S7ND7PEN8W5
---
# Settle a worker at its final answer, shells stopped

## Review panel
📄 Full reviews: [reference](2026-11-25-settle-worker-at-final-answer.reviews.md).
👉 **Decide:** none — approve if stopping a worker that answered, and reporting it `completed`, is the right end.
📍 relais · 1 phase · 1 step for you · next: the go/no-go probe, then Phase 1. Panel: backend, typescript, tui, unix (opus-5-5), codex (gpt-6-astra).
**Changed by review:** the stop's own last turn keeps the latch; a stop is confirmed only by a listing ordered after `TaskStop`'s answer; the process-group probe gates the build.
**Verdicts:** 4 approve-with-changes → 3 approve, backend approve-with-changes; codex approve-with-changes; 3 carried; words 1425 → 1822 (×1.28, high findings); remove 0, merge 0.

## Picture

```
 worker starts a background shell, then answers "DONE" (turn.complete)

 today                              after this plan
 list: running until the shell      list still running 5 s after the answer
   ends: 124 to 8,193 s late          -> TaskStop(agent): its shells die
 or the wall time kills it:         list: killed or gone
   stopped{killed}, answer            -> stopped{completed, answer + note}
   dropped, no verify                 -> snapshot, verify, repair as today
```

The plugin stops a worker that has answered but is held open only by
its own background work, and reports the answer as completed.

## Context

Follow-up ticket `docs/programmes/follow-ups/f21-settle-at-final-turn-with-shells.md`.
A native worker that ends its turn with a background shell still running is
listed `running` by `$.agent.list()` until the shell ends. `observe`
(`claude-plugin/hooks/dispatches.ts`) ends a dispatch only on an ended
listing, so it stays open. f18 measured the cost: 33,220 of 81,284 s (41%)
of timed-out wall time came after the worker had answered. 9 of 9 successful
workers that stopped with a shell running settled 124–1,108 s late. No
worker resumed on a task notification.

Facts checked in this session (2026-11-25):
- **`TaskStop` on the agent kills its shells.** Probe: a subagent ran
  `sleep 987` with `run_in_background` and ended its turn. `TaskStop` of the
  agent left no `sleep 987` process 2 s later. Not yet shown: which signal
  TaskStop sends, and whether a child in its own process group (a greengate
  tree gate, amont `tree_run.rs:305`, reaped only by a signal watcher that
  SIGKILL bypasses, amont#302) dies too. Probe before Phase 1: a
  `trap … TERM` shell records the signal; `pgrep -g <pgid>` is empty after.
- **A stopped agent can still be continued.** `SendMessage` to the stopped
  probe agent resumed it, and it answered. A repair `continue` therefore
  still works after the plugin stops a worker.
- **`killed` drops the answer.** `native.rs` maps `stopped{killed}` to a
  failure, and `result_text = answer.filter(|_| failure.is_none())`. A run
  stopped by the plugin after an answer must report `completed`, or it ends
  `TerminalResultMissing` with no snapshot and no verify.
- **`background_tasks` is not per agent.** The probe's `SubagentStop`
  payload listed the probe agent itself (`type: "subagent"`) beside its
  shell. `BackgroundTaskSummary` has no owner field. With two dispatches at
  once, one worker's payload cannot say which shells are its own.
- **Slots are `flock`s held by the job's process** (close-on-exec,
  `greenharness-slots`). A killed `greengate slot run`/`amont slot run`
  releases its slot, or its queue ticket, when the process dies.
- **relais never reads `DONE`.** The answer is recorded as the attempt's
  `WorkerResult` evidence. Only a line opening with `relais-blocked:` changes
  the route (`claims_blockage`). Verify judges the tree. So "I'll wait for
  the completion notice" already reaches relais as an answer, and verify,
  then repair, decides.
- f20 (`relais-wt-f20`) touches nothing under `claude-plugin/hooks/`.

## Approach

The ticket's four questions, answered:

1. **The end signal is `turn.complete` with `reason: 'answer'`**, not
   `SubagentStop`. `onTurn` already handles it for every dispatch's agent.
   `SubagentStop` goes to the relais hook binary (admission release). Its
   `background_tasks` cannot be attributed to one agent (above). A turn
   whose `reason` is absent (an older engine) counts as an answer when
   `isAborted` is not true and the answer is non-empty. `aborted`, `error`
   and `refusal` keep today's path.
2. **Stopping the shells.** `Dispatch` gains `answeredAt` (the clock at the
   first look after the last answer turn) and `isEnding` (the plugin stopped the agent itself).
   `observe` stays pure and gains one outcome: an answered dispatch whose
   agent is still listed `running` at least `ANSWER_GRACE_MS` (5 s) after
   the answer is `stop`. That leaves time for the 40 runs with no shell,
   which settled within 1 s. In `check`, a `stop` outcome:
   - writes `isEnding` into the store before any await (the `check`s of
     `onTurn` and the poll overlap, `agents.ts:241,281`), calls `TaskStop`
     as `stopAgent` does, and re-reads the dispatch after the await;
   - leaves the dispatch open. Once `TaskStop` has answered success
     (`stopSentAt` set; ordered by a store sequence number taken before each `list()` call, not by the clock), a listing asked for after that answer that shows
     the agent `killed`, `completed` or gone ends it as **`completed`**,
     with the kept answer. While the call is pending, no `stopped` goes out;
   - if `TaskStop` is refused or errors, clears `isEnding` and writes a
     timeline note of its own (not `stopAgent`'s generic text):
     `could not stop agent <id> after its answer (<why>); the run ends when
     its background work ends or at the wall time.` It sets `stopFailed`, so the dispatch does not try again
     and ends as today (shell end or wall time);
   - if the agent is still `running` `STOP_CONFIRM_MS` (30 s) after the
     `TaskStop`, writes one timeline note, once per dispatch (a latch, not
     every 2 s poll): `agent <id> still runs 30 s after its stop; it ends
     with its shell or at the wall time. To end it now, stop it in Claude
     Code.` `stopped` waits for a confirmed stop.

   A new answer turn of the agent (`isAnswerTurn`: a resume that answered) clears `answeredAt`, `isEnding` and
   `stopFailed`; an aborted or empty turn (what a stopped agent's last turn is, `dispatches.ts:75`) clears nothing. **The in-place fence** is untouched: `stopped` goes out
   only after the listing confirms the stop. The agent's own shells are dead
   before the snapshot, and the runner's `wait_for_writers` still runs
   before it. The probe above is a go/no-go gate: if any
   descendant of the stopped shell survives, Phase 1 stops there and the
   finding goes to the person before any code.
3. **Held slots.** They are released when the process holding the `flock`
   dies, so stopping the agent frees them. A live check in Verification
   confirms it once the host has a free slot.
4. **The answer text.** When the plugin stopped the agent, `stoppedPayload`
   appends, after a blank line (`\n\n`), one line to the answer:
   `relais plugin: the worker ended its turn with background work still
   running; it was stopped before the snapshot, and its output was not
   read.` It is not a `relais-blocked:` line, so the route stays
   verify → repair. The repair worker, and the person, can see why a
   "waiting for the notice" answer was cut short.

Cancellation, the wall time and relais's own `stop` keep today's path.
`handleStop` does not set `isEnding`. So an agent killed by relais still
reports `killed`, unless it had already answered and the plugin's own stop
is the one in flight.

## Phases

- [ ] Phase 1 — settle at the answer. `claude-plugin/hooks/dispatches.ts`
  (`answeredAt`, `isEnding`, `stopFailed`, `isAnswerTurn`, `observe`'s
  `stop` outcome and the `isEnding` → `completed` mapping, the note in
  `stoppedPayload`); `claude-plugin/hooks/agents.ts` (`onTurn` stays synchronous and marks the answer turn; `check` reads `fx.clock.now()` once per look and passes it to `observe(d, listed, now)`, which stamps `answeredAt` on the first look after the answer; `check` acts on `stop`: `TaskStop`, confirm, note on
  failure); `claude-plugin/tests/end-signal.test.ts` (pure cases);
  `claude-plugin/tests/settle-at-answer.test.ts` (replay, below);
  `claude-plugin/README.md` (Results), `CHANGELOG.md` (Unreleased).
  First, before any code: the probe of Approach 2 (go/no-go), with its result in the PR body.
  Through relais: one `kind: change`, acceptance `make plugin`.

### 🧑 Your steps
- Reinstall the relais plugin with relais's own install command for Claude — the settle runs only once the installed plugin carries it (installed: 0.72.0). The follow-ups handoff already lists this step.

## Non-goals
- Reading `SubagentStop` in the plugin, or any Rust or wire change.
- Stopping a worker that has not answered (a long foreground suite).
- Worker guidance on polling (f18: at most 9%).
- Keeping a background job's output for the repair attempt.

## Code review
- relais: backend, lang:typescript (unix left out: no command or flag change, and the panel's unix seat reviewed the one process question, TaskStop's signal and escaped groups; tui left out: the new pane text is two timeline notes through the existing `note()` path, fixed by the panel's tui seat)

## Delivery
One worktree and one PR from `origin/main`, merged on green. The plugin
ships with the next relais release (no release cut for this alone), and
takes effect after the person's plugin reinstall. The one-week measure (no
worker stop settles more than 60 s late in `hook_journal`) is filed as a
dated follow-up ticket when this one closes, counted from the reinstall.

## Verification
- `make plugin` (the plugin suite) passes, red-checked: each new test of the new behaviour (not the no-outcome case) fails
  on `origin/main`'s `dispatches.ts`/`agents.ts`.
- `end-signal.test.ts`, pure, with a fixed clock:
  - answered, listed `running` 1 s later → no outcome;
  - answered, listed `running` 6 s later → `stop`;
  - `isEnding`, listed `killed` → ended `completed`;
  - `isEnding`, gone → ended `completed`;
  - not answered (`reason: 'aborted'`), listed `killed` → today's result;
  - `stopFailed` → no second `stop`;
  - `isEnding`, then an aborted turn with an empty answer, then listed `killed` → ended `completed` with the kept answer;
  - a turn with no `reason`, `isAborted: false`, non-empty answer → counts as an answer.
- `settle-at-answer.test.ts`, the replay: the answer is the
  `last_assistant_message` of a real `SubagentStop` from `hook_journal`
  that had a `running` shell ("I'm working f12 … next I merge it once CI
  goes green."). Spawn `d1`, `turn.complete` with that answer, keep the
  agent `running`, advance the scripted clock by polls of 2 s (no minute
  jumps):
  - exactly one `TaskStop` with `task_id: 'agent-1'`, no `stopped` yet, with a turn-triggered `check` overlapping a poll (the `TaskStop` answer delayed past the next poll);
  - flip the fake agent to `killed`, poll → one
    `stopped{status: 'completed'}` whose answer is the replayed text, then
    `\n\n`, then the note;
  - a `TaskStop` that is refused → a timeline note, no `stopped`, no second
    `TaskStop`; then the agent `completed` → `stopped{completed}` without
    the note;
  - an answer that settles normally (listed `completed` within the grace)
    → no `TaskStop`;
  - the agent still `running` after `TaskStop` across 16 polls of 2 s → one timeline note, no `stopped`;
  - a `list()` begun before `TaskStop` answers that returns `killed` after it → no `stopped` until the next poll.
  - The ticket asked for a `SubagentStop` replay. Its payload is the input
    here, fed through the signal the design uses.
- Live, in the first session after the plugin reinstall: one relais run
  whose worker ends with a background shell. `dispatch_ended` comes at most
  60 s after its `SubagentStop`, `pgrep` shows the shell and every process
  group it started (`pgrep -g`) gone, and the
  attempt is verified (not `TerminalResultMissing`). With a free host slot,
  the same probe with `greengate slot run -- sleep 988` shows the slot back
  in `greengate slots --json` and the job's own pid gone (`kill -0` fails):
  the flock is the runner's, close-on-exec, so a free slot alone does not
  prove the job died. These belong to the dated follow-up, not to
  this PR's record.

## Decision log
- 2026-11-25: `turn.complete` over `SubagentStop` as the signal (session-wide `background_tasks`, already wired, no Rust change).
- 2026-11-25: report `completed` after the plugin's own stop (`killed` would drop the answer).

<!-- panel: repos=relais adds= reviewers=backend,lang:typescript,tui,unix,codex body-sha=91beb136793d -->
