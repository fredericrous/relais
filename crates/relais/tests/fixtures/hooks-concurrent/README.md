# Hook payloads: five agents at once

Transcribed from a real session, like `../hooks`, but from a prompt that spawns
**five** general-purpose subagents in one message. `../hooks/README.md` recorded
"concurrent agents of one type" as the case it could not show; this is that case.

    Claude Code 2.1.282 (Claude Code)
    observed 2026-09-24, after the arrival-order fix (#68)

The earlier recorder derived order from a count of files in the directory, so
concurrent hooks collided and four ordinals were lost. These payloads were
captured after that was fixed; arrival order here is trustworthy, which matters
because arrival order is the only thing that pairs a spawn with its agent.

## The arrival order

| # | event | tool | `tool_use_id` | `agent_id` |
|---|---|---|---|---|
| 0000 | SessionStart | — | — | — |
| 0001 | PreToolUse | Agent | `toolu-01` | — |
| 0002 | SubagentStart | — | — | `agent-01` |
| 0003 | PreToolUse | Agent | `toolu-02` | — |
| 0004 | SubagentStart | — | — | `agent-02` |
| 0005 | SubagentStop | — | — | `agent-01` |
| 0006 | PostToolUse | Agent | `toolu-01` | — |
| 0007 | PreToolUse | Agent | `toolu-03` | — |
| 0008 | SubagentStart | — | — | `agent-03` |
| 0009 | SubagentStop | — | — | `agent-02` |
| 0010 | PostToolUse | Agent | `toolu-02` | — |
| 0011 | PreToolUse | Agent | `toolu-04` | — |
| 0012 | SubagentStart | — | — | `agent-04` |
| 0013 | SubagentStop | — | — | `agent-03` |
| 0014 | PostToolUse | Agent | `toolu-03` | — |
| 0015 | PreToolUse | Agent | `toolu-05` | — |
| 0016 | SubagentStart | — | — | `agent-05` |
| 0017 | SubagentStop | — | — | `agent-04` |
| 0018 | PostToolUse | Agent | `toolu-04` | — |
| 0019 | SubagentStop | — | — | `agent-05` |
| 0020 | PostToolUse | Agent | `toolu-05` | — |
| 0021 | SessionEnd | — | — | — |

## What it establishes

**The agents really do overlap.** `agent-01` starts at `0002` and does not stop
until `0005`, by which time `agent-02` has already started. Between `0004` and
`0005` two agents of the same type are alive at once. This is not a serialised
sequence dressed up as concurrency.

**Yet the spawn/start pairing is unambiguous.** Every `PreToolUse:Agent` is
immediately followed by its own `SubagentStart` — five times out of five, and in
the wall-clock recording 30–36 ms apart. No `PreToolUse` ever arrives before the
previous one's `SubagentStart`. So the *n*th start in a session belongs to the
*n*th spawn, and a binding made that way is known rather than guessed.

That matters for the design. Nothing in any single payload joins a `tool_use_id`
to an `agent_id`: the spawn carries no `agent_id`, and `SubagentStart` carries no
`tool_use_id`. The plan assumed correlation would therefore have to fall back on
`agent_type` and pick the oldest candidate, degrading to an inferred attribution
whenever several agents of one type were live. On this evidence that fallback is
needed only if two spawns arrive before any start — which has not happened in
five probe sessions, because the harness staggers them.

**`SubagentStop` precedes its own `PostToolUse`,** consistently: `0005` before
`0006`, `0009` before `0010`, and so on, by ~28 ms in wall clock. The agent's
lifecycle ends before the tool call that spawned it returns. A seat that frees on
`PostToolUse` frees later than one that frees on `SubagentStop`, and the earlier
corrupted recording made this look the other way round.

**`prompt_id` is identical across all 22 payloads** — one turn. It scopes a
correlation but cannot separate agents within a turn.

## What it still does not show

- **Two spawns before any start.** The ambiguous case the inferred attribution
  exists for. Five sessions have not produced it; a fixture for it would need the
  harness to issue two `Agent` calls without staggering, which nothing here has
  managed to provoke. Until such a recording exists, that code path rests on
  reasoning rather than evidence, and should say so where it is written.
- **`PostToolUseFailure`** — still never fired, in any probe session, including
  one whose tool call genuinely failed.

## Redaction

As in `../hooks`: values substituted, **keys, types, nesting and presence
untouched**. `session_id` → `session-0000`, `prompt_id` → `prompt-0000`,
`agent_id` → `agent-NN` and `tool_use_id` → `toolu-NN` numbered in arrival order
so the pairings above survive, transcript paths and file contents replaced.
Filenames are a short arrival index rather than the recorder's nanosecond clock,
which carried a real wall time nobody should assert on.

Assert on which fields exist, how they pair, and in what order — never on a
redacted value.
