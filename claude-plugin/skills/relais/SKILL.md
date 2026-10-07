---
name: relais
description: Route a bounded coding task through the relais supervised runner — explicit model, verification, escalation and accounting. Starts the run with the relais plugin's tools.
---

# /relais

Express the requested work as a task contract, then hand it to relais
through the plugin's tools. The parent does not supervise intermediate
turns.

## Inputs

Before a contract can be written, the caller must supply:

- the repository or task worktree the change belongs to (its root is
  where `relais.toml` is found);
- one precise objective sentence;
- the write scope: which paths the change may touch;
- acceptance criteria a command can verify — never "looks right" or a
  worker's own completion message.

Missing any of these is a reason to ask, not to guess one on the
caller's behalf.

The verification profile is `"default"` unless the repository's
`relais.toml` names another one the person asked for. A repository with
no `relais.toml` is not a reason to stop: step 3 sets it up with the
person.

## Steps

1. Work from the repository the task changes — its task worktree when
   one exists. `relais` finds `relais.toml` upward from the `cwd` you
   pass, so a subdirectory is fine, but a directory outside the
   repository is refused, never guessed.

2. Start the run with the `mcp__relais__run` tool: `cwd` set to the
   repository, and `task` set to the contract itself, as an object:

```json
{
  "schema_version": 1,
  "kind": "change",
  "objective": "<one precise sentence>",
  "base_ref": "HEAD",
  "write_scope": ["<paths the change may touch>"],
  "read_hints": ["<entry points>"],
  "acceptance": ["<criterion a command can verify>", "..."],
  "verification_profile": "default",
  "review": "optional"
}
```

   Use `"kind": "inspect"` with evidence criteria for investigations
   that must not edit files. relais validates the contract and saves it
   under `.relais/tasks/`; a refusal names the field to fix. Write no
   task file and run no `relais plan` yourself. The tool starts the run
   and returns at once. To replay a recorded run, call
   `mcp__relais__replay` with `{task, recipe, cwd}` instead.
   - **Never run `relais run` in Bash.** It is refused outside the
     plugin, and a run started there would not be seen, attributed or
     stopped by it.
   - The plugin starts each worker, continues it for a repair and stops
     it when the run is cancelled. Never spawn, message or stop relais's
     agents yourself: the plugin refuses it.
   - Do not work on the task yourself while the run is going. A worker
     finishing is not the run finishing: relais still verifies, and may
     review, repair or escalate.

3. If the outcome says the repository is not set up, set it up with the
   person, then go back to step 2 with the same contract:
   - `blocked (no_policy)`: call `mcp__relais__onboard` with the `cwd`.
     It proposes the checks found in the repository, asks the person to
     use them and to allow relais to run them, and commits `relais.toml`.
   - `blocked (missing_trust_grant)`: call `mcp__relais__trust` with the
     `cwd`. It shows the person the exact commands and asks.
   - Both ask the person themselves, in Claude Code's own dialog. Do not
     ask the same question first, and do not answer it for them.
   - `ready`: call `mcp__relais__run` again.
   - `declined (not_now)`: stop and report that relais was not set up;
     do not call run again unless the person asks for relais again.
   - `declined (dismissed)`: ask the person what they want.
   - `not set up (…)`: report the reason it gives (a refused commit, a
     detached HEAD) and what it says to do.
   - **Never edit `machine.toml`, and never run `relais trust grant`
     yourself.** A grant is the person's decision; the plugin refuses
     both.

4. Follow it and read the outcome.
   - Progress: call `mcp__relais__status` (phases, decisions, cost and
     the last lines of output), or tell the person to open
     `/relais-status`.
   - The outcome arrives as a message when the run ends. Do not end your
     turn before it arrives: a run whose session ends goes with it.
   - Read the outcome: accepted (receipt + patch), needs_decision,
     needs_review, blocked, failed, budget_exhausted or interrupted. The
     artifacts path is on every terminal state. A run refused before it
     started says `Nothing ran` and names why.

## Rules

- The contract is frozen once the run starts; changing objective, scope,
  acceptance or budget is a new run.
- Acceptance criteria must be executable by the verification profile; a
  worker's completion message is never acceptance.
- If the run needs a decision, make it explicitly — do not let a model
  invent it.

## Done when

- the contract was passed to `mcp__relais__run` as an object and relais
  accepted it;
- a repository that was not set up was set up through
  `mcp__relais__onboard` / `mcp__relais__trust`, with the person
  answering, or the person declined and that was reported;
- the run was started with `mcp__relais__run` (never `relais run` in
  Bash) and reached a terminal state (accepted, needs_decision,
  needs_review, blocked, failed, budget_exhausted or interrupted), and
  that state, not a worker's own summary, was read;
- a `needs_decision` outcome was resolved explicitly, not guessed;
- for `accepted`, the printed artifacts path was checked, not assumed.
