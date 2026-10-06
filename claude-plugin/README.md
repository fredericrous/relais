# relais, the Claude Code plugin

relais as a Claude Code **mod**: it starts relais runs from a Claude Code
session, shows every agent a run dispatches as a native agent, and keeps the
run's timeline live in a pane. relais keeps the run, the ledger, verification,
receipts and every decision; this plugin spawns, continues, stops and shows.

Plan: `docs/plans/2026-10-06-all-native-mod.md` (Design §1–§9, package M1).
The picked pane layout is `docs/plans/2026-10-06-all-native-mod.pane.txt`.

## Tested on

Claude Code **>= 2.1.291, < 2.2.0**. The mods API is early access and moves
between releases; the declarations Claude Code writes beside the plugin
(`.claude-plugin/types/`, generated, not committed) are the authority for the
version in use. The Makefile holds the range and the version CI installs
(`CLAUDE_CODE_MIN`, `CLAUDE_CODE_MAX`, `CLAUDE_CODE_PIN`). Widening the range
means re-running the S0 probes of the plan first.

`claude plugin validate` checks the manifest and the module's structure and
does **not** type-check `register.ts`; the tests carry the behaviour.

## What it does

- **Tools and command.** At `session.start` it registers `mcp__relais__run`
  (`{task, cwd}`) and `mcp__relais__status` (`{run?}`) and the command
  `/relais-status`. `run` starts `relais run --task <task> --protocol` in
  `cwd` with `RELAIS_HOST=claude-code-mod` and `RELAIS_SESSION_ID=<session>`
  and returns at once. It says `relais native hello --session <id>` at
  `session.start` and every 30 s.
- **The protocol.** relais's stdout is JSON lines `{"relais": …}`; a pure line
  splitter reassembles them from the child's chunks and ignores anything else.
  - `spawn` `{dispatch, prompt, subagent_type, model, cwd}` →
    `$.agent.spawn` with `description` `relais <dispatch>`; the plugin's
    `agent.spawn` hook sets that spawn's `cwd`; then `relais native bound`.
  - `continue` `{dispatch, agent, message}` → `SendMessage`; a rejected one
    is `stopped {failed}`.
  - `stop` `{agent}` → `TaskStop`.
  - `event` → the pane, the status line and the timeline.
  - `done` `{run, outcome, receipt, summary}` → a toast, the status line and
    a prompt to the model with the outcome, receipt and summary.
- **Results.** `turn.complete` of a relais agent adds the turn's usage to its
  dispatch and keeps the latest answer. A dispatch ends when `$.agent.list()`
  shows its agent `completed`/`failed`/`killed` (or gone) after a turn of that
  dispatch, or, for a `failed`/`killed` agent, after the status moved since the
  `continue`. Then exactly one `relais native stopped --dispatch <d>` goes out
  with `{agent, status, usage, answer}` on stdin. `bound` and `stopped` carry an
  explicit timeout and are retried (1, 2, 4, 8 s, then every 15 s) until
  acknowledged or the run's child exits.
- **Guards.** `agent.offer` hides `relais:*` agent types from the model (fail
  closed), except while the plugin resumes its own agent; `SendMessage` and
  `TaskStop` aimed at a relais agent are denied unless the plugin sent them; a
  relais agent's `task-notification` is dropped from `prompt.submit` and every
  other prompt is kept.
- **The pane.** Opened at run start with `$.ui.open` (focus left out). When it
  cannot be placed, a toast says `relais run started: /relais-status`, and
  `/relais-status` prints the timeline into the transcript. One section per
  live run: header (run, phase, attempt, elapsed), finished phases on one
  line, the current phase expanded with its live output (as many lines as the
  pane has rows, at most 40 stored per check, each cut at 240 characters),
  agents, cost and the child's stderr (`stderr` label, error colour). The
  stream handler keeps the per-run buffers in module memory (the last 200
  events, not counting `output` ones, and the last 40 output lines per check)
  and writes one `$.state` value per 100 ms tick; the `ui.render` hook only
  reads it. After `/clear`, `/resume` or `/branch` the timeline is reloaded
  from `relais native status`.
- **Status line.** Run count, phase, attempt, elapsed time and
  `/relais-status`; after `done` it keeps the outcome until the pane is opened
  or the next run starts.

## Layout

- `.claude-plugin/plugin.json` — manifest (`types` names the `$.state` contract).
- `hooks/hooks.json` → `hooks/register.ts` — every hook, and `effects($)`,
  the one place the sibling modules' effects are spelled on `$`
  (`claude plugin validate` follows `$` only inside a file).
- `hooks/*.ts` — pure logic and small effectful modules that take that `fx`
  object: `lines` (splitter), `dispatches` (usage and the end signal),
  `timeline` (the run model), `pane` (layout), `guards`, `callbacks`, `agents`,
  `runs`, `ui`, `store`.
- `agents/relais-worker-<model>-<effort>.md` — one worker definition per
  `native::worker_agent_types()` pair (`crates/relais/tests/plugin_agents.rs`
  keeps the two in step).
- `tests/*.test.ts`, `tests/views/relais-pane.test.ts` — the plugin's tests.

Two facts about Claude Code shape the code. A plugin's own `agent.spawn` hook
runs only for a spawn made from inside a hook frame, so the child's output is
read inside a `process.spawn` hook. And a prompt cannot be submitted from under
a tool or command hook, so the `done` verdict is sent by the session's timer.

## Running the tests

```sh
make plugin                       # validate, then test; part of `make check`
claude plugin validate claude-plugin
cd claude-plugin && claude plugin test
```

`claude plugin test` needs no sign-in and no network (it runs in a few
seconds).
