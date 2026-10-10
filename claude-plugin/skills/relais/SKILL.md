---
name: relais
description: Use proactively, without being asked, for any bounded implementation or inspection task — one objective, a write scope, acceptance a command can verify. relais picks the model for it, runs and verifies it in an isolated worktree, escalates and accounts. Works in any repository; one that is not set up yet is set up on the first run, with the person answering.
---

# relais

Route a bounded task through relais instead of doing it yourself: relais
chooses the model and effort for it, runs it in its own worktree,
verifies it with the repository's checks and reports the outcome. The
person does not have to ask for it, and the parent does not supervise
intermediate turns.

## The contract comes from you, not from the person

Write the contract from what you already have: the approved plan and the
step being worked on, or the request in front of you. Do not ask the
person for its fields.

- **Repository:** the one the task changes (its task worktree when one
  exists).
- **Objective:** one precise sentence, from the plan step or the request.
- **Write scope:** the paths the plan names, or the files the change
  plainly touches. Keep it as narrow as the work allows.
- **Acceptance:** what a command can verify. The plan's verification
  items that a command can check, and the repository's checks passing.
  Never "looks right" or a worker's own completion message.
- **Verification profile:** `"default"`, unless the repository's
  `relais.toml` names another the work calls for.

Ask the person only when there is no task to route at all. A repository
with no `relais.toml` is not a reason to ask or to stop: step 3 sets it
up, and the person answers there.

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
   - `declined (not_now)`: do the task yourself, without relais, and say
     so in one line. Do not route work through relais in that repository
     again this session unless the person mentions relais.
   - `declined (dismissed)`: ask the person what they want.
   - `not set up (…)`: report the reason it gives (a refused commit, a
     detached HEAD) and what it says to do.
   - **Never edit `machine.toml`, and never run `relais trust grant`
     yourself.** A grant is the person's decision; the plugin refuses
     both.

4. Follow it and read the outcome.
   - Progress: call `mcp__relais__status` (phases, decisions, cost and
     outcome; add `output: true` only when the last lines of a check's
     output are what you need), or tell the person to open
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
