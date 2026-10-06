---
status: active
branch: feat/all-native-mod
repos: [relais]
adrs: []
---
# relais: every dispatch native, through a relais mod

## Review panel

📄 Full reviews: [2026-10-06-all-native-mod.reviews.md](2026-10-06-all-native-mod.reviews.md)
👉 **Decide:** approve if relais as a Claude Code plugin is the trade you want: every dispatch native, a live relais pane, no headless runs or sandbox, and reliance on the mod API.
📍 relais · plan reviewed (backend, lang:typescript, ui-design, ux-research, react, game-ux) · next: S0 measurements and pane mockups, then M1–M5 in one PR.
**Changed by review:**
- a `--protocol` stdout channel with stdin callbacks, an end-of-run signal from `$.agent.list()`, and rollback-safe usage;
- the mod API owned as a pinned dependency;
- the run's events persisted to `events.jsonl` so the pane survives reloads.

**Verdicts:** every reviewer approve-with-changes; no rework. The items below were found after the body was bound and are binding on the implementation.

**Binding on M1/M2, the protocol:**
- **A repair cannot hang:**
  - a rejected `SendMessage` is `stopped {failed}`;
  - a dispatch may end with no turn only on `failed`/`killed` after the agent's status has moved since the continue (the status at the continue is recorded);
  - test: a continue after a `failed` run.
- **Transcript ids:** the transcript is read for ids only; missing ids are logged as `rollback_ids_missing`, with an ids-count check against reality.
- **`usage import`:** the agent-file skip is added code in M2.
- **Timers:** every module timer is cleared on unload, retries stop when the child exits, and `bound` is idempotent. Test: no `hello` from an old instance within 60 s.
- **Type check:** S0 checks whether `claude plugin validate` type-checks `register.ts`. If not, the plan says the gate is syntax only.

**Binding on §8, the pane (UI panel):**
- **Placement:**
  - open with `focus:false` and without `holdToasts`;
  - decide placement from `isPlaced`/`reason`, never a width. Unasked, a pane is placed from 144 columns; once asked, from 110. When it is not placed, toast "relais run started: /relais-status";
  - `/relais-status` opens the pane, which a person-asked pane allows at any width, and prints into the transcript only when asked to;
  - S0 records `reason` at 80, 110 and 144+ columns, and the number of characters lost from a prompt typed as a run starts (target 0).
- **Before M1:** 2–4 text-cell mockups (110 and 144 columns, docked and inline), checked with `duro mockup check` where it applies, and one picked by the person. The pane leads with a header row (run, phase, attempt k/max, elapsed). Finished phases collapse to one line. The live output is capped by the rows the pane actually gets. Lines are truncated (`wrap:'truncate-end'`) and cut before they are stored.
- **States:** before the first event; a preflight failure; concurrent runs (one pane section per run id, with buffers keyed by run); more agents than fit; unknown cost.
- **Status line:** carries run count, phase, attempt k/max, elapsed time and `/relais-status`. After the outcome, it keeps the outcome until the pane is opened or the next run starts.
- **Toasts:** only for the outcome and for escalations that need the person. Decisions landing within a few seconds of each other are merged. Every decision is also in the timeline.
- **Labels and outcome:** failures and stderr carry text labels (`FAIL`, `stderr`) as well as `color="error"`. The outcome entry shows files changed and +/- lines, plus the receipt path.
- **State discipline:** one writer. The child-stream handler keeps the buffers in module memory and flushes one `$.state.set` per 100 ms tick; the `ui.render` hook only reads.
  - After `/clear`, `/resume` or `/branch` (`classic.SessionStart`), the timeline is reloaded through `relais native status`.
- **Bounds:**
  - `events.jsonl` keeps phases, decisions, cost and outcome. Each check's output there is capped to its last 64 KB, plus an "N bytes elided" event. The full log stays in the artifacts directory.
  - relais's own stderr is mirrored into `events.jsonl`.
  - Excess output is dropped with an elided count, never buffered without limit and never blocking a check (test: a check printing 10 MB keeps relais's memory bounded and its duration within 5%).
  - `mcp__relais__status` returns phases, decisions, cost and the last 40 output lines at most.
- **Pass bar for the pane** (S0, and the reality check):
  - emit-to-render p95 at most 250 ms at 50 events/s, with no lost lines at 5k;
  - pane events = `events.jsonl` events with three checks streaming;
  - otherwise `/relais-status` becomes the main view.
- **Tests:** `tests/views/relais-pane.test.ts`, modelled on diff's `pane-view.test.ts`: phase order, the line cap, a `stderr` entry, the not-placed fallback.
- **Unbacked choices:** the 40-line tail, the 200-event buffer and dropping completion notices are unbacked. A five-person hallway test on a recorded run, after one minute away, asks each person to say the run's phase and outcome within 5 s.

## Context

#171 (merged, unreleased) made worker attempts native. The parent session copies a `RELAIS-SPAWN` line into its Agent tool, and the hook enforces the call:
- a marker rewrites the call;
- a machine-wide `WorktreeCreate` answers every isolated spawn;
- usage is parsed from the transcript, with message-id dedup.

**Today, before this plan,** the rest is blind: every other model call relais makes is a headless `claude -p` the person never sees (patch review, inspect report review, the planner, dataset replay, the doctor probes), and so is a plain `relais run`'s worker, in relais's OS sandbox (#105). relais's non-model work is just as invisible from Claude Code: preparing worktrees and setup, the verification commands, the decision to repair or escalate, and cost. This plan removes all of it.

The person asked for **everything native, as the only mode**, and for **"everything in the Claude console, no blind spots"**. They also asked whether Claude Code mods are the right road.

Mods measured on Claude Code 2.1.291 (this session, a probe plugin loaded with `--plugin-dir`):
- **Spawn:** `$.agent.spawn({prompt, description, subagentType, model})` starts a native background subagent. It is rendered like any agent.
- **Directory:** an `agent.spawn` hook returning `next({...e, cwd})` runs it in that directory. The `cwd` spawn argument alone is ignored.
- **Result:** `turn.complete` fires per subagent turn with `agentId`, `answer` and exact `usage` (input, output, cache read, cache write, model).
- **Continuation:** `$.tool.call({tool:'SendMessage', to, message})` continues the agent in the same directory, in an interactive session. Its next turn has its own `turn.complete` and usage. In `claude -p` it fails once the main turn has ended.
- **The catch:** the main model is told when a plugin-spawned agent completes.

From the docs and the 2.1.291 declarations:
- `$.tool.register` gives the model a tool, `mcp__<plugin>__<name>`.
- `agent.offer` returning `{isOffered:false}` hides an agent type from the model while `$.agent.spawn` still works.
- `prompt.submit` can `drop` a `task-notification`.
- `$.process.spawn` streams a child's output for the session's life.
- `$.ui.status`, toasts and panes are available.
- Mods are documented as on by default from 2.1.287; the env var `CLAUDE_CODE_ENABLE_FUNCTION_HOOKS` is said to be ignored. My first probe failed validation for an unrelated reason, so S0 re-measures this.
- The API "can change between releases": the version-pinned `.d.ts` is the authority.

**Why mods are the correct road.** They remove the three fragile parts of #171:
- **The parent model in the loop.** It no longer copies spawn lines, so there is no paraphrase, no foreground-Bash deadlock, and no "the parent ended its turn and killed the run".
- **The machine-wide `WorktreeCreate` takeover.** Claude Code's default tree comes back for everyone else.
- **Usage from transcripts.** `turn.complete` gives exact per-turn usage instead of transcript parsing with message-id dedup.

What it costs:
- relais requires an interactive Claude Code with mods on. Unattended or terminal runs end, as the person chose.
- relais ships a small JS plugin, pinned to and tested against a Claude Code version range.

**Intended outcome:**
- `/relais` (or the model, through the relais tool) starts a run.
- Every agent relais dispatches appears as a native agent: worker, repair, escalation, patch review, report review, planner.
- relais keeps the run, the ledger, verification, receipts and the decisions.
- relais never launches `claude -p`.
- Nothing relais does is out of sight: every step of a run is shown live in Claude Code (the relais pane, below).

## Design

### Shape
1. **The relais plugin** (`relais@relais-local`), installed by `relais install --claude`. It ships the mod, the agent definitions and the `/relais` skill.
2. **Start.** The mod registers a tool `mcp__relais__run {task, cwd}`, a tool `mcp__relais__status {run?}` and a `/relais-status` command.
   - The run tool starts `relais run --task <task> --protocol` with `$.process.spawn` (cwd = the repo). It returns at once with the run id.
   - The child lives for the session. Session end, or an unload or reload of the module, kills it; the ledger then shows the run interrupted, and `relais resume` reconciles it, as for any crash.
3. **The protocol channel.**
   - Under `--protocol`, stdout carries only protocol lines, `{"relais":"spawn"|"continue"|"stop"|"done", …}`, written through one writer. Every human line goes to stderr, including the 247 `println!` in `main.rs`, which are routed through the run's output sink under `--protocol`.
   - `$.process.spawn` hands over text in chunks, not lines. The mod reassembles lines with a pure line splitter that carries the remainder between chunks, and ignores any line that is not a protocol object.
   - The requests:
     - **`spawn`:** the mod calls `$.agent.spawn({prompt, description, subagentType, model})`. `description` carries the dispatch id (`relais <dispatch>`), and a pending map keyed by that id holds the spawn's `cwd`. The mod's `agent.spawn` hook matches only spawns whose `next.origin` is the relais plugin and whose dispatch id is pending, and sets their `cwd`: the attempt's worktree for a worker, the review directory for a reviewer, the repo for the planner. It then reports `bound`.
     - **`continue`:** `$.tool.call SendMessage`.
     - **`stop`** (cancellation): `$.tool.call TaskStop`. S0 measures whether that accepts a plugin agent's id.
   - **Back to relais.** The mod calls short `relais native bound|stopped` subcommands through `$.process.run`, with the JSON payload on **stdin**: the mod has no file access, since a hooks module cannot import `node:*`. Each call sets `timeoutMs` explicitly. A non-zero exit or a timeout is retried with backoff (1, 2, 4, 8 s, then every 15 s) until the coordinator acknowledges or the run ends. The payload stays in the mod's memory until then, and no file is written.
4. **Result.** A dispatch ends when its agent's run ends, and `turn.complete` alone does not say that a turn was the last.
   - **Accumulating.** The mod's `turn.complete` hook, for an agent it spawned, adds the turn's usage to that dispatch's total and keeps the latest answer.
   - **The end signal.** The run has ended when `$.agent.list()` shows that agent's `status` as no longer `running` (`completed`, `failed` or `killed`), **and** the dispatch has seen at least one `turn.complete` since it began. The mod checks right after each `turn.complete`, then every 2 s while a relais agent is still listed as running.
   - **A repair** continues the same agent under a new dispatch: the `continue` line carries the dispatch id, and the mod opens a fresh total for it. Right after `SendMessage`, `$.agent.list()` can still show the previous run's `completed`; the turn condition keeps that from being reported as the new dispatch's end, with zero usage.
   - **Reporting.** On the end signal, the mod sends `stopped {dispatch, agent, status, usage (summed over the dispatch's turns), answer}`. That records `Stopped` in the coordinator registry: the existing `admission/native.rs`, keyed by dispatch, now carrying usage and answer. `stopped` is idempotent per dispatch.
   - `bind_native` already accepts a stop that arrives before the bind (admission/native.rs). `NativeBackend` keeps polling `NativeStatus` and builds the `LaunchResult` from it.
   - Usage is exact and per turn, so there is no transcript parsing or message-id dedup. Cost = `price(usage)` with the machine table (`Cost::Estimated`); the N5 unpriced refusal stays.
   - S0 measures how many `turn.complete` events one subagent run raises when it makes several tool calls, and when `$.agent.list()` flips its status. The design holds for one or many turns, because it never treats a turn as the end.
5. **The verdict.** When the run ends, relais writes `{"relais":"done", "run", "outcome", "receipt", "summary"}`.
   - The person sees a toast and the status line.
   - The model learns it through the channel S0 finds for putting a message into the main loop from a plugin. The fallback is `mcp__relais__status`, which the `/relais` skill tells the model to call when the person asks, or when a toast says the run ended.
6. **Guards, in-process.**
   - **Agent types:** `agent.offer` hides `relais:*` agent types from the model. It fails open, so the hook is written to fail closed.
   - **Messages:** a `tool.call` hook on SendMessage/TaskStop refuses calls aimed at a relais agent unless they come from the mod itself (`next.origin`).
   - **Notifications:** `prompt.submit` drops a relais agent's `task-notification`. The match is the exact notification format S0 records, plus the agent id, plus `spawnedBy` where `$.agent.list()` gives it; an unrelated notification that merely mentions the id is kept.
   - **Status:** `$.ui.status` shows one line: run, attempt, phase.
7. **No mod, no run.**
   - `relais run` refuses unless `RELAIS_HOST=claude-code-mod` is set, as the mod sets it on the child, and `RELAIS_SESSION_ID` (the existing override, coordinator/mod.rs) names a session whose mod has said hello to the coordinator within the last 60 s. The mod sends `relais native hello` at `session.start` and every 30 s.
   - This is a guard against mistakes, not a security boundary: a person who sets both variables by hand gets a run that waits for spawns nobody makes and ends `native_spawn_missing`. The error says to start runs from Claude Code with the relais plugin.
   - `--native` and `--native-spawn-wait` are removed. The spawn wait stays internal: no `bound` within 120 s ends the attempt `interrupted (native_spawn_missing)`. S0 and the verification measure spawn → bound (p50 and p95 over 20 spawns) to justify the 120 s.
8. **No blind spots: the relais pane.** Every step a run takes is shown live in Claude Code, not only its agents.
   - Under `--protocol`, relais also writes `{"relais":"event", …}` lines:
     - phase changes (each also appended to `events.jsonl`, below): preflight, worktree, setup, baseline, attempt k (kind, tier, model, effort), verification, review, decision, receipt;
     - each verification command's start and end (argv, exit code, duration) and its output, streamed as chunked `output` events while it runs;
     - routing and escalation decisions, with their reason;
     - cost after each dispatch (booked, estimated or unknown);
     - the outcome.
   - **Kept on disk.** relais also appends every event to `<artifacts>/<run>/events.jsonl`, a file and not a ledger table, so no migration. `relais native status` and `mcp__relais__status` read that file. A run interrupted by a reload therefore still shows its whole timeline after `relais resume`.
   - **Bounded output.** `output` events are capped: chunks of at most 4 KB, merged every 100 ms. The mod keeps only a ring buffer (the last 40 lines per running check, the last 200 events) in `$.state`.
   - **stderr is shown too.** The mod streams the child's stderr (relais's human lines, any panic) into the pane and the timeline as `stderr` entries.
   - **The pane.** A relais pane (`$.ui.open`), opened at run start and reopened with `/relais-status`, draws a timeline of the events, the live output of the running check (its last 40 lines), the agents with their status, and the cost so far.
   - **Elsewhere.** The status line always carries the current phase. A toast marks each decision and the outcome.
   - **Narrow terminals.** Where the pane cannot be placed (S0 measures the width; the docs say 110 columns), the same timeline is reachable through `/relais-status`, which prints it into the transcript. If S0 finds the pane unusable, `/relais-status` becomes the main view.
   - **What remains outside the console:** relais's own files (the ledger, the artifacts directory). The pane links their paths, and `mcp__relais__status` returns the same timeline to the model.
9. **The mod API as a dependency** (`change.a-new-dependency-is-owned`).
   - The plugin declares the Claude Code range it was tested on, starting at 2.1.291. `relais doctor` and `relais run` refuse a Claude Code outside that range, naming the range and the installed version.
   - Each Claude Code bump that moves the range re-runs S0's probes, scripted as `make mod-probe`, before the range is widened. The person owns that, like the toolchain pin.
   - The plugin source is TypeScript (`register.ts`), which Claude Code loads directly. Its effects (`$.process`, `$.agent`, `$.tool`) sit behind a small deps object, so its tests inject them. `make check` runs `claude plugin validate` and `claude plugin test` on it. There is no tsc and no npm dependency.

### Every dispatch native
- `LaunchSpec` loses `presentation`, `sandbox`, `env`, `allowed_tools`, `pid_slot` and `tools: ToolSet`. It gains `agent: AgentKind` (`Worker`, `Reviewer`, `Planner`) and `cwd`.
- `Backend` keeps one implementation, `NativeBackend`.
- **Agent definitions** ship in the plugin, one per (kind, model alias, effort):
  - **Worker:** Read, Grep, Glob, Edit, Write, Bash.
  - **Reviewer:** Read, Grep, Glob. Used for the patch review and the report review.
  - **Planner:** Read, Grep, Glob, Bash, `maxTurns: 8`.
  - None of them has Agent.
- **Callers:**
  - `dispatch_attempt` (worker, repair, escalation), `review_candidate`/`dispatch_reviewer` and `scheduler::propose_plan` all go through `managed_launch` → `NativeBackend`.
  - `relais dataset replay` runs inside a session too (cwd = its scratch checkout).
- A repair continues the same agent when the model is unchanged; otherwise it spawns fresh. This is unchanged from N2.

### Removed (with the person's choice of "everything native")
- **Headless launching:** `ClaudeBackend::launch`, `ProbeLauncher`, the JSON-result parsing and `total_cost_usd`. `claude --version`/`--help` probing stays, for the harness id and the effort catalog.
- **The OS sandbox:** `sandbox/**` (6,150 lines including tests), the runner's sandbox gate, launch and denial recording, `refusal_addendum`, `headless_worker_rules`, `doctor --verify-sandbox`, and the machine.toml `[sandbox]` section. #105 closes as dropped.
- **Headless permissions:** `[permissions] allowed_tools` and `LaunchEnv` (the worker env allowlist and scrub). `disallowed_tools` stays as a written rule in the agent definitions.
  - A machine.toml that still has `[sandbox]` or `allowed_tools` keeps parsing. Doctor reports them as no longer read.
- **#171's hook-side native path:**
  - the marker, `RELAIS-SPAWN`/`CONTINUE`, `hook/native.rs`, `hook/worktree.rs` (the machine-wide `WorktreeCreate`), the hook's `Rewrite`/`WorktreePath`/`WorktreeFailed` answers, and the SendMessage/WorktreeCreate hook wiring (install migrates them away);
  - the transcript usage *booking* (usage now comes from `turn.complete`). `usage import` skips both the message ids in `native_usage_messages`, as today, and the `subagents/agent-<id>.jsonl` files of agents the ledger records as relais dispatches (`dispatches.agent_id`). No migration, so an older binary still opens the ledger.
  - The classic `relais hook` stays for what it did before #171: admission caps on the person's own spawns.
- `doctor --probe-hooks` (it runs `claude -p`) is replaced by `claude plugin test` of the shipped plugin, run by `make check`, plus a doctor check that the plugin is installed and enabled.

### Rollback
- One PR, one revert. Reverting restores the headless mode, the sandbox and #171's hook path.
- The ledger gains no migration in this change, so a binary from before it reads the ledger as it was.
- machine.toml keys this change stops reading (`[sandbox]`, `allowed_tools`) are kept as written, so a reverted binary finds them again.
- **Usage after a revert.** The old binary's `usage import` skips the message ids in `native_usage_messages`. This change keeps that table written, so the skip still works after a revert.
  - On `stopped`, relais reads the agent's subagent transcript, found by session and agent id (`~/.claude/projects/*/<session>/subagents/agent-<id>.jsonl`), with the existing `parse_transcript`. It records those message ids in `native_usage_messages`, in the same transaction as the usage event. The usage itself comes from `turn.complete`; the transcript is read for the ids only.
  - A test proves the old skip on a ledger holding two such runs.
  - The revert runbook in the PR body also gives a fallback SQL for a transcript the ids missed. It does not live in the reverted code.
- `relais install --claude` after a revert puts back the hook wiring. The plugin it no longer installs is removed by `relais uninstall --claude`, or with `claude plugin uninstall relais@relais-local`.

### Kept
- The coordinator, its native registry (re-keyed to carry usage and answer), admission, leases, heartbeats, the ledger, verification, receipts, routing, the effort ladder, escalation, and N5's unpriced refusal.

## Packages
One branch and one PR (`work.*`), with reviewable commits. The plan lands first.

0. **S0, measure first** (2.1.291, the probe plugin, an interactive pty session). Record each in the Decision log:
   - mods load with no env var;
   - a persistent install route: a local marketplace + `claude plugin install`, or `CLAUDE_CODE_PLUGIN_DIRS` in settings `env`; and the agent type names a plugin's agents get (`relais:worker-…`?);
   - `agent.offer` hides them while `$.agent.spawn` still spawns them;
   - `prompt.submit` drops a relais agent's completion notification and nothing else;
   - `$.process.spawn` streams a long child line by line;
   - `TaskStop` stops a plugin-spawned agent;
   - a SendMessage from the model to a relais agent is refused by the mod's `tool.call` hook while the mod's own goes through;
   - how many `turn.complete` events a subagent run making 5+ tool calls raises;
   - `$.agent.list()` lists plugin-spawned agents, with `status` moving to `completed`/`failed`/`killed`, and the time from the run's last `turn.complete` to a non-running status (p95 over 20 runs);
   - the exact text of a plugin agent's task-notification, and whether it reaches `prompt.submit` both idle and mid-turn;
   - a way for a plugin to put a message into the main loop (the run's verdict);
   - the pane: open one with `$.ui.open`, record the narrowest width at which it is placed, and measure redraw latency at 50 `output` events/s and with 5k streamed lines;
   - `claude plugin validate` and `claude plugin test` on 2.1.291, and whether the pinned Claude Code installs on the CI runners (`npm i -g @anthropic-ai/claude-code@<pin>`) and runs `claude plugin test` there with no sign-in, as the docs say it needs none. If it does not, the plugin tests are a local gate and CI prints that it skipped them;
   - `$.process.spawn` chunk boundaries for a 64 KB line; `relais native stopped` latency with a 200 KB answer on stdin; spawn → bound p50 and p95 over 20 spawns.

   A failed assumption changes the plan, not the code.
1. **M1, the relais plugin** (`claude-plugin/` in the repo; `register.ts`, loaded directly by Claude Code, no tsc; tests with `claude plugin test`):
   - the tool and the command;
   - the relais pane: the event timeline, live check output (ring-buffered), agents, cost and the child's stderr; `/relais-status` as the fallback for narrow terminals;
   - the child stream;
   - spawn with the `cwd` rewrite, continue, stop;
   - `turn.complete` accumulation and the end signal from `$.agent.list()`, `stopped` idempotent per dispatch, with stdin payloads, explicit timeouts and retries;
   - the line splitter (a request split across two chunks; a 64 KB line);
   - `agent.offer`, the SendMessage guard, the notification drop and the status line.
2. **M2, the relais side of the protocol:**
   - the stdout request lines;
   - `relais native hello|bound|stopped|status` subcommands over `coordinator::Client`, and the `done` line;
   - a session's hello that lapses mid-run (no hello for 60 s) ends the run `interrupted (mod_gone)`;
   - the registry carrying usage and answer;
   - `NativeBackend` building results from it;
   - `relais run` refusing outside the mod (`RELAIS_HOST`, the hello heartbeat), and refusing a Claude Code outside the plugin's range;
   - `--protocol`: every stdout line is a protocol object, everything human goes to stderr; the `event` lines for phases, checks (with streamed output, capped at 4 KB per chunk and merged every 100 ms), decisions, cost and outcome, each also appended to `events.jsonl`;
   - removal of the `--native*` flags.
3. **M3, every dispatch native:** the `LaunchSpec` reshape, `AgentKind`, the reviewer and planner definitions, replay, and removal of `ClaudeBackend::launch`/`ProbeLauncher`.
4. **M4, removals:** the sandbox, allowlist and env plumbing; headless rules; denial recording; verify-sandbox; probe-hooks; #171's hook-side native path (`native_usage_messages` stays written, from the transcript, for rollback); and the `usage import` agent-file skip.
5. **M5, install, docs and uninstall:** plugin install and uninstall, the hook wiring migration back to `Agent|Task`, skill text, SPEC (§3, §8, §11, §15, §23), README and INTEGRATIONS.

Each package: `make check` (which now includes `claude plugin test claude-plugin/`), falsification of its key tests, and an implementation review before the push. M1–M5 run as relais runs where the repo's policy allows. Note that relais runs while this branch is in progress use the installed (headless) relais.

## Verification
- **Unit and runner tests** with a scripted mod, standing in for the plugin, that reads the request lines and calls `relais native bound/stopped`:
  - worker, repair-as-continue, escalation-as-spawn, reviewer and planner dispatches all go out as spawn lines with the right agent kind and cwd;
  - usage is booked from the reported usage;
  - no spawn → `native_spawn_missing`;
  - `relais run` without `RELAIS_HOST`, or without a recent hello for its session, is refused;
  - under `--protocol`, every stdout line of a whole run, accepted and failed, parses as a protocol object;
  - a `stopped` sent twice for one dispatch books once;
  - a session whose hello lapses mid-run → the run ends `interrupted (mod_gone)`;
  - a `stopped` that arrives before `bound` is kept.
- **Plugin tests** (`claude plugin test`):
  - a spawn line → `$.agent.spawn` with `cwd` set by the hook;
  - `turn.complete` of a relais agent → usage added to its dispatch; status leaves `running` → exactly one `relais native stopped`, with the summed usage; `turn.complete` of another agent → nothing;
  - a `continue` followed by a stale `completed` status → no `stopped` until the repair's first `turn.complete`; then one `stopped` with only the repair's usage;
  - `agent.offer` hides `relais:*`;
  - a model SendMessage to a relais agent is refused;
  - a task-notification of a relais agent is dropped, an unrelated one that mentions the id kept;
  - a request split across two chunks is reassembled; a garbage line is ignored;
  - a failed `relais native stopped` is retried until it succeeds, with the payload on stdin;
  - two spawns in flight get their own `cwd`;
  - a run's event lines draw a timeline with each phase, a check's streamed output, the decision and the cost; a narrow terminal gets the same timeline from `/relais-status`;
  - a dispatch whose agent raises three `turn.complete` before its status leaves `running` is reported once, with the three turns' usage summed.
- **Against reality**, an interactive Claude Code session (pty-driven) in a scratch repo with the plugin installed. Each check: driven → expected.
  - `/relais` on a one-file change → the worker row appears; the run is accepted; the dispatch row `native_run` with the agent id; usage equals the reported `turn.complete` usage; no "background agent completed" message reaches the main model; the main model can name the run's outcome and receipt (from the `done` message or `mcp__relais__status`).
  - The plugin reloaded mid-run → the run ends interrupted, and `relais resume` reconciles it.
  - **No blind spots:** during the one-file run, the relais pane shows, live and in order, preflight → worktree → baseline → attempt 1 (worker agent) → verification with `python3 -m unittest` output streaming → decision → receipt and cost. A screenshot is recorded. Every model call of the run is in the agents panel: the run's distinct `dispatches.agent_id` = the relais agents in `$.agent.list()`, checked on the one-file run and on the run with a repair (two dispatches, one agent). Every verification command in the ledger has its event lines. A forced panic in `relais run` appears in the pane as `stderr`. After a plugin reload mid-run and `relais resume`, `/relais-status` shows the full timeline from before the reload, read from `events.jsonl`.
  - `make check` time added by `claude plugin validate` + `test`: measured and recorded.
  - A first attempt red → the repair continues the same agent → accepted, with the repair booked from its own turn.
  - A required patch review → a reviewer agent (read-only) appears → accepted. An inspect task → report review native → `settled_via = report_review`.
  - The model tries to spawn `relais:worker-…` or message a relais agent → refused.
  - `relais run` from a plain terminal → refused with the message.
  - `rg -n 'claude", "-p\|"-p"' crates/` → no launch site left (only `--version`/`--help`).
- The live checkout's `git status --porcelain` before and after the runs → identical.
- In the same session, the model spawns an unrelated agent with `isolation: "worktree"` → it runs in Claude Code's own `.claude/worktrees/agent-<id>` on a `worktree-agent-<id>` branch, and the relais hook journal shows no `WorktreeCreate` entry.

<!-- panel: repos=relais adds=lang:typescript,ui reviewers=backend,lang:typescript,ui-design,ux-research,react,game-ux body-sha=ba59de77b8c9 -->

## Decision log

**2026-10-06, S0** (Claude Code 2.1.291; a probe plugin `relais-s0` with one shipped agent, driven in interactive sessions through a pty at 120 and 160 columns). The gate passes.

| Item | Measured |
|---|---|
| mods load with no env var | yes: `CLAUDE_CODE_ENABLE_FUNCTION_HOOKS` is not needed (the first probe's failure was an invalid `node:fs` import) |
| plugin agent naming | `<plugin>:<agent>` (`relais-s0:s0-worker`, `source=plugin`); `$.agent.spawn` of it works; it renders in the agents panel (`◯ relais-s0:s0-worker  relais d1`) |
| `agent.offer` | returning `{isOffered:false}` keeps the type out of the model's list. **It also blocks resuming that agent** ("Agent type … is not offered in this session"), so the hook lets the type through while the mod's own `SendMessage` is in flight |
| `turn.complete` count | one per subagent run, even with 6 tool calls; `$.agent.list()` shows `completed` ~150 ms later |
| agent eviction | a completed agent leaves `$.agent.list()` ~30 s after it ends, and is still resumable 2 min later. **The end signal is `completed`/`failed`/`killed` or absent, after a turn** |
| continuation | `$.tool.call SendMessage` resumes it in the same cwd (with the offer let through), with its own `turn.complete` |
| `TaskStop` | stops a plugin agent (`task_type: local_agent`); status `killed`; one empty `turn.complete` follows |
| model's SendMessage | refused by the mod's `tool.call` hook (origin `engine`); the mod's own passes (origin `{plugin: relais-s0}`) |
| notifications | none for a plugin agent's first run; after a continued run: `prompt.submit` with origin `task-notification` and text starting `<task-notification>\n<task-id>{agentId}</task-id>`. The drop works |
| verdict into the main loop | `$.prompt.submit({text})` from a timer or stream callback; refused from inside a `command.run` hook. The model reads "The relais-s0 plugin sent a message: …" |
| `$.process.spawn` | a 64 KB line plus 5,000 lines arrived as one 114 KB chunk in 73 ms; the line splitter stays (chunking is not guaranteed) |
| stdin | 200 KB to `wc -c` via `$.process.run` in 23 ms |
| spawn → started | 189–307 ms over 5 spawns (the 120 s wait has room to spare) |
| pane placement | unasked at session start: not placed at 120 columns (`reason: "unasked below 144 columns (120 now): …"`), placed at 160. From a command it is placed at 120 |
| pane focus | `focus:false` is always refused ("focus is true or left out", host check). Opened with focus left out or true, text typed right after reached the prompt intact (0 characters lost) |
| pane redraw | 250 events at 50/s → 154–162 renders, lag at most 23 ms (the bar was p95 at most 250 ms) |
| `claude plugin validate` | **does not type-check**: a `register.ts` with two type errors passes. The gate is structural; the tests guard behaviour |
| `claude plugin test` | runs with an empty `HOME`, no sign-in, in under 2 s. CI install of the pinned Claude Code is confirmed on the first push |
| transcripts | written in a normal interactive session (main and `subagents/agent-<id>.jsonl`). A pty session that inherits the parent's `CLAUDE_*` variables (`CLAUDE_CODE_CHILD_SESSION`) writes none; the test driver strips them |
| persistent install | deferred to M5 (local marketplace + `claude plugin install`, or `CLAUDE_CODE_PLUGIN_DIRS`), measured there with an isolated `HOME` |

**Decisions taken from S0:**
- `agent.offer` hides `relais:*` except while the mod resumes its own agent (a module flag set around its `SendMessage`).
- A dispatch ends when its agent is `completed`, `failed`, `killed` or no longer listed, after at least one turn, or after the status moved since a continue.
- The pane opens with focus left out, never `focus:false`. Placement follows `isPlaced`.
- `$.prompt.submit` carries the verdict, called from the stream handler, never from inside a command hook.
- No type check in the gate; the plugin's tests carry it.

**2026-10-06, the pane's direction** (the person, from three text-cell directions: timeline, split, dashboard): **A, timeline**. The artboard is `2026-10-06-all-native-mod.pane.txt`, which M1's pane view tests follow.
