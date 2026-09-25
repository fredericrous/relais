# Hook payload fixtures

These are **transcribed from a real Claude Code session**, not written by hand.
`relais doctor --probe-hooks` wired a throwaway settings file to a record-only
handler, ran one session, and wrote every payload verbatim; these files are those
recordings with their values substituted (see *Redaction* below).

    Claude Code 2.1.281 (Claude Code)
    observed 2026-09-24

Written this way on purpose. A hand-written fixture is a claim about what the
harness sends, and a wrong claim is indistinguishable from a right one until
production disagrees — a fixture and the code can share one misconception and
corroborate each other. Regenerate these with `make probe-hooks` when the Claude
Code version moves, and regenerate them *in the same commit* as any change that
reads a new field.

## What one session showed

| order | event | tool | `agent_id` | `tool_use_id` |
|---|---|---|---|---|
| 0000 | SessionStart | — | absent | absent |
| 0001 | PreToolUse | Read | absent | `toolu-01` |
| 0002 | PreToolUse | Agent | absent | `toolu-02` |
| 0003 | SubagentStart | — | `agent-01` | absent |
| 0004 | PreToolUse | Read | `agent-01` | `toolu-03` |
| 0005 | PostToolUse | Read | `agent-01` | `toolu-03` |
| 0006 | SubagentStop | — | `agent-01` | absent |
| 0007 | PostToolUse | Agent | absent | `toolu-02` |
| 0008 | SessionEnd | — | absent | absent |

Three things worth reading off it:

- **`agent_id` is the CALLER's agent, and it is absent at the top level.** It is
  missing from `0002` (the spawn itself, made by the main session) and present on
  `0004`/`0005` (calls the subagent made). An earlier probe matched these events
  to the `Agent` tool only, saw `agent_id` nowhere, and looked like evidence that
  the field does not exist. It was evidence that the instrument was too narrow.
  Absence only means something once the case that would show presence is
  exercised.
- **`SubagentStart`/`SubagentStop` carry no `tool_use_id`,** and the spawn's
  `PreToolUse`/`PostToolUse` carry no top-level `agent_id`. Nothing BEFORE the
  agent runs joins the tool call that admitted it to the agent that then ran;
  that join has to be made from `agent_type`, arrival order, and `prompt_id`.
  Afterwards there is one: the Agent `PostToolUse`'s `tool_response.agentId`
  (see `0009`/`0010` below).
- **`prompt_id` is identical across every event of one turn** (absent only from
  `SessionStart`). It scopes a correlation to the turn, which narrows the
  candidates before `agent_type` has to disambiguate — it does not by itself
  separate two agents spawned in the same turn.

## What this session did NOT show

- **`PostToolUseFailure` did not fire.** The probe's prompt deliberately read a
  nonexistent path; the failure came back as an ordinary `PostToolUse` (`0001`)
  with the error in `tool_response`. So on 2.1.281 a failing tool call was not
  observed to produce this event. "Did not fire" is recorded as exactly that —
  not as "does not exist" — because a prompt that fails to provoke an event says
  as much about the prompt as about the harness.
- **Concurrent agents of one type.** One agent ran here, so the ambiguous case
  the binding logic exists for is not represented. A fixture for it has to come
  from a session that spawns several at once.

## `0009`/`0010`: an async launch, and the agent's real end

These two come from a **different, later session** (Claude Code 2.1.282), not
the one tabulated above — hence `session-0001`/`prompt-0001`/`toolu-11`/
`agent-11`. Like `0000`–`0008` they are verbatim recordings with their values
redacted: keys, types and presence are untouched. That session ran four agents
against a per-session cap of three with the hook wired by `relais install
--claude --hooks`, and admitted all four — which is how the launch/end
distinction below was found rather than reasoned.

The two files are one agent's pair: `0009`'s `tool_response.agentId` and
`0010`'s `agent_id` are the same agent, aliased `agent-11`. Distinct real
agents keep distinct aliases, which is why `0010`'s `background_tasks` names
`agent-11` beside `agent-12` — a sibling still running when this one stopped.

| order | event | tool | `agent_id` | `tool_use_id` | `tool_response.agentId` |
|---|---|---|---|---|---|
| 0009 | PostToolUse | Agent | absent | `toolu-11` | `agent-11` |
| 0010 | SubagentStop | — | `agent-11` | absent | — |

What they showed:

- **`duration_ms` on an Agent `PostToolUse` measures the LAUNCH, not the
  agent's run.** The `PostToolUse` arrived ~100 ms after its `PreToolUse` with
  `duration_ms: 8` and `status: "async_launched"`, while the agent it launched
  ran on for 6–16 seconds more until its `SubagentStop`. A seat released on
  `PostToolUse` is released at launch, and a per-session cap enforced that way
  does not bind. (`0007`'s `duration_ms: 3250` with `status: "completed"` is the
  synchronous case, where the call does wait for the agent — and even there its
  `SubagentStop` arrives first; see `../hooks-concurrent/README.md`.)
- **`tool_response.agentId` is the only place a tool call and the agent it
  produced appear together.** Every other payload carries one side of the join:
  the spawn's `PreToolUse` has the `tool_use_id` and no agent, `SubagentStart`/
  `SubagentStop` have the `agent_id` and no tool call. The `PostToolUse` has
  both — which is why relais binds a spawn's seat to its agent there, and gives
  it back on the `SubagentStop` that names that agent.

`tool_response.agentId` was left unredacted in `0007` (and in
`../hooks-concurrent`) when `agent_id` was substituted, so in those recordings
the two do not match by value. In `0009`/`0010` they are substituted alike, so
the join the harness makes is visible in the fixtures too.

## Redaction

Values were substituted; **keys, types, nesting and presence were not touched**,
because those are the evidence. Substitutions: home paths → `/REDACTED`, this
repo's path → `/REPO`, `session_id` → `session-0000`, `prompt_id` →
`prompt-0000`, `agent_id` → `agent-NN`, `tool_use_id` → `toolu-NN` (stable, so
the joins above still hold), transcript paths → `/REDACTED/transcript.jsonl`,
and file contents and assistant messages → a placeholder string.

A fixture asserting on a redacted value is asserting on the redaction. Assert on
which fields exist, how they relate, and which payloads carry them.
