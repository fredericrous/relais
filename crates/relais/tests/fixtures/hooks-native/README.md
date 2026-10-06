# Native-worker hook payloads

Recorded from a real Claude Code session, not written by hand, then redacted
by the rules in [`../hooks/README.md`](../hooks/README.md#redaction). One extra
substitution: the tree a `WorktreeCreate` hook handed out is `/RELAIS-TREE`.

    Claude Code 2.1.291 (Claude Code)
    observed 2026-10-06, S0 of docs/plans/2026-10-06-native-workers.md (E12)

A record-only handler ran on every event; its `WorktreeCreate` answer printed
an existing worktree's path. The session spawned one background agent with
`isolation: "worktree"`, waited for it, then continued it with `SendMessage`.
The subagent's own Bash calls and the parent's `ToolSearch` (which loaded
`SendMessage`, a deferred tool) are left out.

| order | event | tool | `agent_id` | `tool_use_id` |
|---|---|---|---|---|
| 0001 | PreToolUse | Agent | absent | `toolu-01` |
| 0002 | WorktreeCreate | — | absent (`name: agent-agent-01`) | absent |
| 0003 | SubagentStart | — | `agent-01` | absent |
| 0004 | PostToolUse | Agent | absent (`tool_response.agentId: agent-01`) | `toolu-01` |
| 0005 | SubagentStop | — | `agent-01` | absent |
| 0006 | PreToolUse | SendMessage | absent (`tool_input.to: agent-01`) | `toolu-02` |
| 0007 | SubagentStart | — | `agent-01`, again | absent |
| 0008 | PostToolUse | SendMessage | absent (`tool_response.resumedAgentId: agent-01`) | `toolu-02` |
| 0009 | SubagentStop | — | `agent-01`, again | absent |

What these show:
- `WorktreeCreate` names no tool call. Its `name` is `agent-` followed by the
  agent id that `SubagentStart` and `tool_response.agentId` report later, and
  it fires before either.
- A continued agent keeps its id: `SubagentStart` and `SubagentStop` fire again
  with the same `agent_id` (and the same transcript file).
- `SendMessage`'s `PreToolUse` input carries `to` and `message`, plus
  `recipient`, `content`, `summary` and `type`; its `PostToolUse` input carries
  only `to`, `summary` and `message`.
- `SubagentStart` and `SubagentStop` carry the agent's worktree as `cwd` (`/RELAIS-TREE`),
  while the other events report the session's `cwd`.
