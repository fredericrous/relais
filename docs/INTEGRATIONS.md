# The Claude Code hook integration

`relais install --claude` writes agent definitions and three skills —
`/relais`, `/relais-verified-push` and `/relais-architecture-conflict` —
and never touches `settings.json`. `relais install --claude --hooks` is a
separate, explicit ask on top of that: it wires a live hook into
`.claude/settings.json` so relais can see — and, on one event, refuse —
native Claude Code subagent spawns. This document explains what that
write does, what the numbers on it mean, how to undo it, and what
`relais doctor` will tell you about it. The normative rules live in
[SPEC §23](SPEC.md#23-multiple-claude-code-sessions-and-nested-agents);
this page only explains how to use them.

## What `--hooks` writes

Seven targets, the same seven every time — this list and the one
`relais doctor --probe-hooks` records from are kept identical by a test,
so they cannot drift apart silently:

| event | matcher |
|---|---|
| `PreToolUse` | `Agent\|Task` |
| `PostToolUse` | `Agent\|Task` |
| `PostToolUseFailure` | `Agent\|Task` |
| `SubagentStart` | (none) |
| `SubagentStop` | (none) |
| `SessionStart` | (none) |
| `SessionEnd` | (none) |

The three tool-scoped events are matched to the Agent tool and its
legacy name, `Task` — Claude Code still sends either, depending on
version — so relais sees a subagent spawn and nothing else. The four
lifecycle events carry no matcher at all, because they are not tool
calls.

The command each handler runs is the **absolute path** of the relais
binary that ran `install`, never a bare `relais`. A bare name resolved
from `PATH` that later goes missing exits 127, which Claude Code treats
as a non-blocking hook failure — the tool call proceeds and nothing
reports that the hook never ran at all. An absolute path fails the same
way if the binary is moved, but at least resolves the same way every
time regardless of the shell's `PATH`.

`settings.json` is a file a person maintains by hand, so the write is
conservative by construction:

- It never guesses at reformatting. Before writing anything, relais
  parses the file, re-renders it with `serde_json`, and compares
  byte-for-byte (tolerating only the trailing newline a pretty printer
  omits and an editor usually adds). A file that does not survive that
  round trip is left untouched, and the fragment to paste in by hand is
  printed instead — rewriting a file relais cannot reproduce losslessly
  would bury the one line it wants to add inside a reformatting nobody
  asked for.
- It joins rather than duplicates. An existing entry on the same event
  with the same matcher gets relais's command appended into it; an entry
  on a different matcher gets its own, because a matcher states which
  tool calls a handler wants to see and widening someone else's is not
  relais's call to make.
- Nothing is deleted except the leaf command relais itself added.
  `relais uninstall --claude --hooks` recognises only the hook object
  whose command is its own binary path and removes exactly that leaf,
  never a whole entry or matcher another tool might still need.

### Handler timeouts

An **absent** `timeout` field on a Claude Code hook waits indefinitely —
measured directly (a 300-second handler ran to completion, with no
ceiling in sight) — so every handler `--hooks` installs carries an
explicit one:

- **`PreToolUse` gets a derived timeout**, because it is the one event
  that actually holds a tool call open for as long as it runs (also
  measured directly). The hook has to stop polling for a seat at its own
  deadline (`queue_wait_secs`, under `[admission]` in `machine.toml`)
  with enough of its own budget left to withdraw the request and print a
  refusal — a connect, plus two coordinator round trips for the admit
  that gave up and the withdraw that follows it, plus a fixed margin for
  process startup and scheduling jitter around the hook binary itself.
  Raising `queue_wait_secs` and re-running `--hooks --write` raises this
  timeout with it; the two cannot drift apart, because one is computed
  from the other rather than chosen by hand.
- **Every other handler gets a modest, explicit timeout** (10 seconds).
  None of them can hold a tool call open — only `PreToolUse` can — so
  none needs the derived budget, but each still needs a number, for the
  same reason `PreToolUse` does: an absent one waits indefinitely.

An expired `PreToolUse` handler is not a refusal relais made — the
harness kills it, discards whatever it was about to answer, and lets the
tool call proceed anyway. That is an admission nobody decided, and it is
why the derived timeout exists: raising `queue_wait_secs` without also
raising the handler timeout would make that failure mode more likely,
not less.

## Removing it

`relais uninstall --claude --hooks` reverses exactly what `--hooks`
added: the leaf command on each of the seven targets, and nothing an
entry's matcher was already carrying for someone else. Preview first,
same as install; `--write` applies it.

## What `relais doctor` says about it

Three separate findings, because they answer three separate questions:

- **`hook-compat`** — is there a compatibility record, and is it stale?
  `relais doctor --probe-hooks` is what produces that record (see below);
  a fresh machine has none, and `doctor` says so rather than assuming
  compatibility. A record that exists but names a Claude Code version
  different from the one on `PATH` now is reported stale, not silently
  trusted.
- **`hook-timeout`** — does the timeout actually recorded in
  `settings.json` still cover the configured `queue_wait_secs`? The hook
  cannot read its own handler timeout to check this itself, so `doctor`
  reads the file and fails — not warns — when the recorded number no
  longer covers the wait, or when a `PreToolUse` entry carries no
  `timeout` field at all. Both are corrected by re-running
  `relais install --claude --hooks --write`.
- **`hook-live`** — does the hook that is actually wired in enforce
  anything? `doctor` takes the exact command recorded in `settings.json`
  and runs it for real, against a scratch config and state directory of
  its own (one whose machine settings refuse admission when the
  coordinator cannot be reached, which a coordinator-less scratch
  directory never can), feeding it a fixture `PreToolUse` spawn payload.
  This never touches your real hook journal or reserves a seat in your
  real coordinator. Outcomes: no relais command found in any settings
  file doctor can see; a recorded command that ran and refused, as
  configured; a recorded command that ran but printed no refusal — wired
  in and enforcing nothing, reported together with how the run ended; and
  a relais command recorded in MORE than one settings file, which is a
  failure of its own — Claude Code merges the files and runs every one on
  every spawn, so this is never a case of picking one to check and
  ignoring the rest. A further case is not a verdict on the hook at all:
  if doctor could not stage or spawn the exercise, it says the hook's
  behavior is unknown and why, rather than reporting silence as failure.

`doctor` checks every settings file Claude Code merges: the project's own
`.claude/settings.json`, the project's `.claude/settings.local.json`, and
your `~/.claude/settings.json` — all three, not just the first that names
a relais command on `PreToolUse`, so `hook-live` and `hook-timeout` see a
hook wired only in the local file exactly as they would one in the
committed file. `hook-compat` reads no settings file at all — it compares
the compatibility record against the Claude Code on `PATH`.

### The compatibility matrix

`relais doctor --probe-hooks` is a separate, explicit command, never part
of the ordinary `doctor` run: it needs a real Claude Code session (a
throwaway settings file under the state directory, never your own
`settings.json`), costs money, and touches the network. It runs one
`claude -p` session prompted to force a nested agent call, records every
payload that arrives on the seven targets, and writes a compatibility
record naming the Claude Code version observed and, per target, whether
it fired and what fields its payload carried — reported as what was
seen, never as what was expected. There is no shipped compatibility
table, and none is implied to exist on a fresh machine: the matrix is
written by this command, not carried as a static claim in this
document. A target that never fires for a given Claude Code version is
recorded as not firing; that is a fact about that version, not a probe
failure.

Nothing here narrows which models a hook-admitted agent may use. That
would need both a compatibility record confirming the harness reports
enough to act on and a setting to act on it with, and neither exists
today.

## Known limits of the hook path

These four are specific to `--hooks`; the general limits that are not
about hooks stay in the [README](../README.md#known-limits).

- **A native subagent is admitted but never individually tracked.** A
  spawn's own `PostToolUse` fires at launch — a real Claude Code 2.1.282
  session measured `duration_ms: 8` and `tool_response.status:
  "async_launched"` on it, while the agent it launched kept running for
  6–16 seconds more — so a seat released at `PostToolUse` would have been
  released before the agent it names finished. relais instead binds the
  seat to the agent at `PostToolUse` and only releases it at
  `SubagentStop`. An agent whose `SubagentStop` never arrives holds its
  seat until `binding_lease_secs` lapses: the cap over-counts for that
  long rather than under-counting, and an agent that runs longer than the
  lease loses its seat before it actually stops, since nothing tells the
  coordinator otherwise.
- **Depth is not enforced, and a subtree cannot be cancelled.** Nothing in
  a single hook payload joins a spawn to the dispatch it descends from
  (`crates/relais/tests/fixtures/hooks/README.md` records which payloads
  carry `agent_id` and which do not), so every hook-admitted spawn is
  requested at depth 0 and `max_agent_depth` governs managed dispatch
  only. For the same reason relais cannot name the agents beneath one
  spawn, so it cannot cancel a subtree: `relais cancel` reaches managed
  dispatch, and on this path only the session's own run.
- **Without the hook, or with the coordinator unreachable, native
  subagents are only observed.** Nothing is refused, and `relais
  coordinator status` says so in as many words (`enforcement: observed
  only (nothing is capped)`). Managed dispatch through `relais run` is
  then the sole path the coordinator caps.
- **A hook-admitted spawn writes no ledger row.** Nothing on this path
  calls the ledger's dispatch-intent recording, so a coordinator restart
  has no row to adopt it from: its seat is gone the moment the
  coordinator that granted it exits, and the agent it admitted keeps
  running unwatched. This is a real limit of the hook-admitted design,
  not something a restart's adoption logic is asked to paper over by
  inventing a row for a dispatch that never had one.
- **No money cap applies on this path, only an agent-count cap.** A
  hook-admitted spawn is governed by the machine's own `[concurrency]`
  limits — the same caps that apply to any other run — and by nothing
  else. The run a hook registers carries no budget, and the coordinator
  only refuses on budget for a run that has one, so raising
  `dispatch_reserve_micros` above zero changes what a spawn reserves
  against an unbounded total and still refuses nothing.
- **A cancellation on this path lasts only until the next reconcile.**
  `relais cancel` marks a session's derived run terminal, but a
  cancelled run with nothing outstanding is reaped at the next
  reconcile — 15 seconds later — after which the next spawn in that
  session finds no record, registers the run again, and is admitted. The
  cancellation lives in coordinator memory only; nothing on disk lets a
  hook tell a reaped-cancelled run from a session it has never seen.
  Cancelling a tab's agents is not a durable stop — closing the tab is.

## Troubleshooting

- **More than one settings file names a hook.** Nothing wins: Claude Code
  MERGES them and runs every matching handler. Measured on 2.1.282 — a
  hook in `.claude/settings.json` and another in
  `.claude/settings.local.json`, both on `PreToolUse`, both fired on one
  `Bash` call. So two relais handlers in two files mean two hooks on every
  spawn, each asking the coordinator; the second sees the first's dispatch
  and answers `AlreadyAdmitted`.

  `relais doctor` reads every settings file the harness merges — the
  project's `.claude/settings.json`, its `.claude/settings.local.json`,
  and the user's `~/.claude/settings.json` — and reports a hook recorded
  in more than one of them as a failure naming every file it found it in,
  rather than silently checking whichever one it saw first. `relais
  install --claude --hooks` also refuses on the same condition: it looks
  for a relais command in the OTHER files before writing, and if one is
  already there it writes nothing rather than adding a second handler.

  So if you already have two — from before this existed, or from wiring one
  by hand into the local file — delete the ones you do not want. Install
  will not put them back, and it will not add another while any of them
  remain: with a handler still recorded in a file install is not writing,
  it refuses and names that file. Keeping the one in
  `.claude/settings.json` is the case that needs nothing further, since
  that is the file install itself targets; keep one elsewhere and install
  stays refused, which is correct — there is already a hook, and a second
  is what you are trying to avoid.
- **Each git worktree is a separate session.** A Claude Code
  `session_id` is per process, and opening a worktree normally starts a
  fresh `claude` process for it — so `max_active_agents_per_session`
  caps agents **per tab**, not per repository. Three tabs on three
  worktrees of the same repo each get their own session cap; running the
  same task in parallel across tabs adds up against
  `max_active_agents`, the per-machine total, not against any one
  session's limit.
- **CI should not inherit `carry_on` by accident.** A CI runner
  typically has no relais coordinator running at all, and
  `on_coordinator_unreachable` defaults to `carry_on` precisely so an
  ordinary coordinator restart on a workstation does not turn into
  blanket admission refusal. On a CI machine that same default means the
  hook stays silent on every spawn — enforcing nothing, without saying
  so anywhere a person would see. Either don't wire hooks into whatever
  `settings.json` a CI job uses, or set
  `on_coordinator_unreachable = "refuse"` deliberately in that job's
  `machine.toml` so a missing coordinator is a loud refusal instead of a
  quiet no-op.
- **The per-session cap adds up across tabs on one machine.**
  `max_active_agents_per_session` bounds one tab; `max_active_agents`
  bounds the machine. Several tabs each under their own per-session cap
  can still collectively reach the machine-wide cap, at which point a
  further spawn in any of them is refused or queued (`queue_wait_secs`)
  regardless of that tab's own headroom. `relais coordinator status`
  reports both counts, broken down by dispatch source, so a refusal that
  looks like a per-session problem can be told apart from one that is
  actually the machine running out of room.
