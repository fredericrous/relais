---
status: active
branch: feat/native-workers
repos: [relais]
adrs: []
---
# relais: native workers, rendered by Claude Code, governed by relais

## Review panel

👉 **Decide:** approve if a native-subagent worker that relais governs through hooks is the trade you want. The worker is rendered exactly as Claude Code's own; relais keeps the run, verification, repair and escalation; it gives up a hard per-worker dollar cap and its own OS sandbox.
📍 relais · plan reviewed · next: S0, the measurements gate (worktree option A or B, else stop). Panel: backend.
**Changed by review:**
- the worktree became a go/no-go gate, with B confining the worker by hook;
- a marked spawn is denied when the coordinator is down;
- usage is booked by unbooked message ids;
- relais-crash detection uses heartbeats on the agent lease.

**Binding on N2 (low, not re-reviewed):** the before-bind crash path is the Unbound lease path, not the unclaimed reaper. It is stale after `LEASE_GRACE` (300 s, admission/mod.rs:32, 1574), then freed as `unbindable` after `UNBINDABLE_AFTER` = 3 reconcile rounds (:67, 2132-2149); heartbeats reset `rounds` (:1667). If relais crashes in the spawn wait, the seat is held for at least 300 s plus three rounds. Verify by killing relais 10 s into the wait: expect the dispatch to show up `stale`, then `unbindable`.
📄 Full reviews: [2026-10-06-native-workers.reviews.md](2026-10-06-native-workers.reviews.md)
**Verdicts:** backend rework → approve-with-changes, then three fresh binding passes (approve-with-changes each, all applied; the last item is the binding low item above).

## Context

When a person runs relais from Claude Code, they see nothing of the worker. `relais run` launches each worker as a headless `claude -p --output-format json` session, and the parent session receives only the final report through its Bash tool. The person asked for the worker's activity to look exactly as Claude Code renders its own agents, "as if relais were not in between".

Only Claude Code can render it exactly. The way to get that is to run the worker as a **native subagent** (the parent's `Agent` tool): a live row in the agents panel, and a full transcript that opens with Enter.

My first answer said native mode gives up the model, the budget, the sandbox and escalation. That was wrong. It repeated SPEC §3's "advisory" label, which describes *unmanaged* native delegation. Measured and documented facts (Claude Code docs, relais's own hook fixtures, files on this Mac) show:
- **Model and arguments can be pinned.** A `PreToolUse` hook on `Agent` can return `hookSpecificOutput.updatedInput`, which replaces the call's `model`, `subagent_type`, `prompt` and `isolation`. An agent definition's frontmatter pins `model:`, `effort:`, `tools:` and `maxTurns:`.
- **Subagents are visible to hooks.** Hooks fire for every tool call inside a subagent, with `agent_id` and `agent_type`. `SubagentStop` carries `agent_transcript_path` and `last_assistant_message`.
- **Usage is readable.** A subagent's transcript (`~/.claude/projects/<slug>/<session>/subagents/agent-<id>.jsonl`) has `model` and full token `usage` on every assistant record. relais already parses and prices these (`orchestration::parse_transcript`, `price`).
- **A subagent can be continued** with `SendMessage`, so a repair can reuse the same, natively rendered subagent.
- **What is genuinely lost:**
  - there is no per-subagent dollar cap (only `maxTurns`, plus a check between attempts);
  - relais's OS sandbox cannot apply, because a subagent inherits the session's permission mode and sandbox. The person (bypass permissions) doubts they need the sandbox.
- **What is in place:** `relais hook` is already installed on this machine's seven events. It admits, binds and releases native agents through the coordinator.

Gaps, from the code exploration:
- the hook reads no `tool_input`, never answers anything but deny, and never acts on `SubagentStop`;
- nothing links a native agent to a task contract or a run (only one coordinator run per session, and no ledger rows);
- the runner's worker launch (`dispatch_attempt`) is tied to `claude -p`.

The runner's judging half (`snapshot_attempt`, `judge_candidate`, `verify_candidate`, `machine::decide`, `seal`) already works on any candidate. The scheduler's assembled-candidate path proves it.

**Intended outcome:** `/relais` in Claude Code runs a supervised task whose workers appear exactly like Claude Code's own subagents. relais is invisible in the rendering, and keeps its run, ledger, verification, receipts, repair and escalation decisions. The headless supervised mode stays for terminal and unattended use.

## Design

### Shape: relais drives, the session spawns
`relais run --native` runs the existing engine unchanged, except for one thing: the worker's launch goes through a new **`NativeBackend`** (an `impl Backend`). Its `launch` publishes a *spawn request* and blocks until that dispatch's `SubagentStop` arrives. relais still decides every next step (repair, escalate, stop), so escalation stays relais's decision, not the parent model's.

1. **The skill starts the run in the background.** The `/relais` skill runs `relais run --native --task …` with `run_in_background`, and follows its output with the `Monitor` tool (or by reading the output file).
2. **relais asks for a spawn.** On each worker turn relais writes one machine-readable line to stdout, `RELAIS-SPAWN {json}`, and registers the same request with the coordinator. The json carries the dispatch id, `subagent_type`, `model`, a `prompt` (containing a run marker) and `run_in_background: true`. For a repair, relais writes `RELAIS-CONTINUE {agent_id, message}` instead.
3. **The session spawns it.** The parent calls `Agent` (or `SendMessage` to continue) with exactly those fields. Claude Code renders it natively.
4. **The hook enforces the spawn.** `PreToolUse(Agent)` finds the run marker and matches the call to a pending request by dispatch id.
   - It returns `updatedInput` with the requested `model`, `subagent_type`, `prompt` and `isolation`, so a parent that paraphrased or changed them cannot change what runs.
   - **Denied:** an Agent call whose marker names an unknown, already-bound or finished dispatch. So a second spawn for one request is refused, and only the first binds.
   - **`SendMessage` is gated too:** the hook matcher gains `SendMessage`. A continuation is allowed only when its target agent and message match a pending `RELAIS-CONTINUE`.
   - **Admission:** a marked call is exempt from the session's own coordinator admission (`derive_run_id(session)`), so it is charged once, to the native run.
   - **The coordinator unreachable:** a marked call is **denied**, whatever `on_coordinator_unreachable` says. `CarryOn`, the default, would otherwise let it run unenforced (`CarryOn => Silent`, hook/decide.rs:231). Unmarked calls keep today's setting.
   - Calls without a marker are ordinary native agents, handled exactly as today.
5. **Stop and usage.** `SubagentStop` settles the dispatch, keyed by (agent_id, dispatch), because a repair continues the same agent, so one agent_id carries several dispatches. Usage comes from `agent_transcript_path` through the existing parser and pricing, **counting only the message ids no earlier dispatch of the run has booked.** It is never a whole-file parse, which would book attempt 1 again inside attempt 2. It is never a record offset taken at bind either: a background subagent can write records before `PostToolUse` fires, and those would be lost. The result is booked as the attempt's `UsageEvent` with `CostKind::EstimatedApiEquivalent` and its completeness. The model comes from the records and checks the rung's model. `last_assistant_message` becomes `result_text`. The blocked `launch` returns a `LaunchResult` built from these (a plain struct with public fields), so the engine snapshots and judges as today.
6. **Repair and escalation.**
   - A repair continues the same subagent with `RELAIS-CONTINUE` (the repair addendum as the message).
   - An escalation is a fresh `RELAIS-SPAWN` on the next rung's model and agent type.
   - The budget is checked between attempts, as `ceiling_reached` already does. Within an attempt, `maxTurns` from the agent definition is the hard cap.
7. **Waits, each with a default and a flag** (`cli.robust.timeouts`):
   - **Spawn wait,** `--native-spawn-wait` (default 120 s): if no marked spawn binds in time, the attempt ends `interrupted (native_spawn_missing)` and the run stops. It is never silently unmanaged.
   - **After the bind,** `NativeBackend::launch` honours the attempt's existing `wall_timeout` (backend.rs:431) and cancellation. If no `SubagentStop` arrives in time (a dead parent session, a stuck subagent), the attempt ends `interrupted` as a `claude -p` timeout does today.
   - **relais crash:** while `NativeBackend::launch` waits, it sends the dispatch's existing `Heartbeat` (coordinator/mod.rs:212, 1108) every 30 s.
     - **After the bind,** each heartbeat renews the agent lease (admission/mod.rs:407), so a healthy worker keeps its rights past 120 s, up to its `wall_timeout`. When the heartbeats stop, that lease lapses at `agent_lease_ttl` (= `binding_lease_secs`, 120 s, admission/mod.rs:990-997), and the hook denies every marked call for that run. An orphaned subagent cannot be continued or respawned under it. No second timer is added.
     - **Before the bind,** during the spawn wait, the dispatch is admitted but unbound. There the existing unclaimed-dispatch reaper (admission/mod.rs:34-41) and `--native-spawn-wait` bound it, and relais's heartbeats keep it from being reaped while relais is alive.

### The worktree (S0 is a go/no-go gate)
relais keeps making the task worktree (setup included) at the resolved base, and the subagent must work there. Claude Code's own `isolation: worktree` cannot be prepared beforehand: it creates the tree at spawn time from a session-wide `baseRef`, which does not pin `base_sha`. So:
- **A, if S0 shows it: a `WorktreeCreate` hook** answers the subagent's `isolation: worktree` request with relais's prepared task worktree path.
- **B, otherwise: no isolation, with confinement by the hook.**
  - The hook matcher gains the subagent's own tool calls, for marked agents only; any other agent is silent at once.
  - Every `Bash` call of a marked agent is rewritten through `updatedInput` to `cd <task worktree> && <command>`.
  - `Edit` and `Write` (and `NotebookEdit`) outside the task worktree are denied.
  - Without this, the subagent's cwd is the parent's live checkout, the person's working copy, which must never be the place it works.
  - **The limit, stated:** a Bash command can still `cd` elsewhere inside itself. B confines the defaults, not a determined worker. Under the person's bypass permissions, that is the same trust the session already extends.
- **If neither A nor B measures sound in S0, the plan stops** and comes back to the person. Native mode does not ship on an unconfined working directory.

relais does not `retire_worktree` a tree it did not make (option A's path is relais's own tree, so retiring stays as today). `SubagentStop` stands in for the write-lease wait, because the tree stops changing when the agent ends.

### Agent definitions and prompt
- `relais install --claude` ships native worker definitions, one per (tier, effort) the route can choose, e.g. `relais-worker-sonnet-medium`. Each has `model:`, `effort:`, `tools:` and `maxTurns:`, so the hook only has to pick the type. `isolation: worktree` follows S0's choice:
  - under A, the definitions carry it;
  - under B, they do not, and the hook removes `isolation` from any marked call's `updatedInput`. Otherwise Claude Code would create its own tree from `baseRef`, which B exists to avoid. The research and review definitions are unchanged.
- `build_prompt` becomes `pub(crate)`. A new `WorkerMode::Native` rules text says: commit nothing, push nothing; you may not spawn agents; work only in this worktree. It drops the sandbox and `$TMPDIR` wording. The repair addendum names the log paths in the run's artifacts by absolute path.

### What changes in the hook
- `event.rs` reads `tool_input` for Agent calls, and on `SubagentStop` reads `agent_transcript_path`, `last_assistant_message` and `stop_hook_active`.
- `HookAnswer` gains a `Rewrite { input }` variant, rendered as `updatedInput`, for marked native dispatches only.
- **SPEC §23's rule "the hook never says yes on a person's behalf" changes on purpose.** `updatedInput` pairs with `allow` (which skips the prompt) or `ask`. The decision, by the payload's `permission_mode`:
  - in `bypassPermissions`, `acceptEdits` or `auto`, a rewritten call answers `allow`. The person already approves such calls without a prompt, so nothing is widened in practice.
  - in `default` or `plan`, it answers `ask`, and the person still approves (S0 confirms `ask` plus `updatedInput` keeps the rewrite).
  - in `dontAsk`, an unapproved call becomes a deny, so `ask` would stop native mode on every call. S0 measures `ask` plus `updatedInput` there. If it denies, `dontAsk` answers `allow`, scoped exactly as below, and the plan says so.
  - The `allow` is scoped to marked calls whose whole input relais itself wrote: the pending spawn or continue, or B's Bash `cd` prefix. It never extends to a call relais did not author.
  - SPEC §23 and the forbidden-word test change to say exactly this, and the change is called out in the CHANGELOG.
- The coordinator gains the pending native-dispatch registry: request, bind on `PostToolUse` (`tool_response.agentId`), settle on `SubagentStop`. It extends the existing `bind_agent_lease` and `settle_by_agent`.
- Ledger dispatch rows record `source = native_run` with `agent_id`.
- `relais usage import` skips the agent-transcript records a run already booked, matched by message id, so usage is never counted twice.

### Unchanged
- Headless `relais run` (allowlist and sandbox modes) is unchanged.
- Native agents without a relais marker are unchanged.
- Verification, acceptance, review, inspect report review and receipts are reused as-is.

## Packages
One branch and one PR (`work.*`), reviewable commits. The plan lands first.

0. **S0, measure first** (Claude Code 2.1.291, no code changes; hand-built settings and a throwaway session). Record each outcome in the plan:
   - `relais doctor --probe-hooks`, to refresh the fixtures recorded on 2.1.281–2.1.283;
   - `updatedInput` on `PreToolUse(Agent)`: does a rewritten `model` or `subagent_type` actually run? Is `permissionDecision` required?
   - `WorktreeCreate`: can a hook supply an existing worktree path for `isolation: worktree`?
   - `SendMessage` continuation of a finished background subagent, and whether the continued records append to the same `agent-<id>.jsonl`;
   - option B's mechanisms: a `Bash` call of a subagent rewritten through `updatedInput` (does the `cd` prefix run?), and `ask` plus `updatedInput` in default mode;
   - a marked spawn while the coordinator is stopped is denied;
   - whether a background subagent writes transcript records before its spawn's `PostToolUse` fires;
   - `ask` plus `updatedInput` under `dontAsk`;
   - under B, the hook's latency on an unmarked `Bash` call (p50 and p99), since the widened matcher runs `relais hook` on every `Bash`, `Edit` and `Write` of the session;
   - agent frontmatter `effort:` and `maxTurns:` honoured;
   - the `Monitor` tool following a background Bash's stdout line by line.

   The design above takes S0's answers. A failed assumption changes the plan, not the code.
1. **N1, the hook** (relais run): read `tool_input` and the SubagentStop fields; add the `Rewrite` answer; add the coordinator's pending native-dispatch registry, bind, settle and ledger rows; SPEC §23.
2. **N2, the engine** (relais run): `NativeBackend` (publish the request, block on the coordinator until stop or timeout, build the `LaunchResult` from the transcript); `relais run --native` with `--native-spawn-wait`; the worktree mechanism S0 chose; `native_spawn_missing`. It also includes the per-dispatch, transcript-derived `UsageEvent`, so a native run is budgeted from its first commit (`ceiling_reached` sees real spend).
3. **N3, the parts the person touches** (relais run): native worker definitions and the `/relais` skill flow (background run, Monitor, spawn and continue exactly as asked), `WorkerMode::Native` rules, and install and uninstall of the new owned files.
4. **N4, import dedup** (relais run): `relais usage import` skips agent-transcript records a run already booked, matched by message id.

Each package: `make check`, falsification of its key test with a forced rebuild, and the implementation review before the push.

## Verification
- **Unit and runner tests** with a scripted native backend:
  - spawn, stop, judge, accepted;
  - a repair is a `RELAIS-CONTINUE` to the same agent;
  - an escalation is a `RELAIS-SPAWN` on the next rung's model;
  - a missing spawn ends `native_spawn_missing`;
  - usage is booked once, from the transcript;
  - a parent that changes `model` gets it rewritten back.
- **Hook tests on fixtures** recorded on 2.1.291:
  - a marked spawn is rewritten, and an unmarked one is silent as today;
  - an unknown marker is denied;
  - a second spawn for a bound dispatch is denied;
  - a `SendMessage` that matches no pending continue is denied;
  - a marked call after relais's heartbeats stopped is denied, while a marked call at t > 120 s from a run still heartbeating is allowed;
  - under B, a marked agent's `Bash` is rewritten with the `cd` prefix, and its `Edit` outside the task worktree is denied;
  - `SubagentStop` settles the right dispatch.
- **Against reality, in a real Claude Code session in the relais repo.** Each check: driven → expected.
  - `/relais` on a small change task → the worker row appears in the agents panel while it runs (screenshot); Enter opens its transcript; `relais run` exits 0 `accepted`; `receipt.json` exists.
  - The ledger → one `dispatches` row with `source = native_run`, a non-null `agent_id`, and a `usage_events` row with `cost_kind = estimated_api_equivalent` whose tokens equal that agent transcript's sum over distinct `message.id`, taking each id's last usage.
  - A task whose first attempt fails a check → exactly one `RELAIS-CONTINUE` to the same `agent_id`, then `accepted`. The repair's booked tokens equal only the records appended after the continue, not attempt 1 again.
  - A marked spawn with the coordinator stopped → denied, and the run ends `interrupted`.
  - **Under B, the live checkout is never touched:** a worker `Edit` aimed at the parent's live checkout is denied, and `git status --porcelain` of the live checkout is identical before and after the run. Worker writes outside the task worktree: 0.
  - Usage across a run with one repair: tokens booked for attempts 1 and 2 together equal the sum over distinct `message.id` of the whole `agent-<id>.jsonl`, taking each id's last (largest) usage, because Claude Code writes one message as several records whose `output_tokens` grow, with nothing missing and nothing counted twice.
  - The parent changes `model` on the spawn → the agent runs on relais's model, read from the transcript.
- **Inspect task:** a natively rendered worker, then relais's report review → `accepted` with `settled_via = report_review`.
- **Headless `relais run` unchanged:** `relais run` on the same small change task, no `--native` → `accepted` with a `managed_run` dispatch, as before this change.
- **Latency measured:** the time from a `RELAIS-SPAWN` line to its bind, p50 and maximum over 10 runs, compared with the 120 s spawn wait.

## Decision log

**2026-10-06, S0 results** (Claude Code 2.1.291, a `claude -p --model haiku` parent, a logging hook in a scratch repo). The gate passes: option A.

| # | Question | Result |
|---|---|---|
| E0 | `relais doctor --probe-hooks` | PreToolUse, PostToolUse, SubagentStart, SubagentStop, SessionStart and SessionEnd fire. SubagentStop carries `agent_transcript_path`, `last_assistant_message`, `stop_hook_active`, `effort`. PostToolUseFailure did not fire (as before). |
| E1 | `updatedInput` on `PreToolUse(Agent)` | A rewritten `subagent_type` and `model` run (transcript model haiku, `.meta.json` agentType the rewritten one). **No `permissionDecision` is needed.** The hook's tool name is `Agent`. |
| E2 | A subagent's `Bash` rewritten with a `cd X &&` prefix | Runs; the command's file lands in X. (B's mechanism works, but B is not needed.) |
| E3 | `WorktreeCreate` supplying a path | **Works.** The subagent works in the hook's path. The payload's `name` is `agent-<agentId>`, the same id SubagentStart reports, and it fires about 90 ms after `PreToolUse(Agent)`. Claude Code did not remove the tree afterwards. |
| E4 | `SendMessage` continuing a finished subagent | `{to: <agentId>, message, summary}` continues it. SubagentStart and SubagentStop fire again with the same `agent_id`, and records append to the same `agent-<id>.jsonl` (8 records for 4 distinct `message.id`: per-id dedup is required). |
| E5 | Frontmatter `effort:` and `maxTurns:` | Both honoured (partial result after 3 turns; transcript `effort` low). |
| E6 | Permission modes | In `default` and `dontAsk`, the rewritten call runs with no decision. `ask` in `-p` is refused. |
| E7 | Monitor following a background Bash | `tail -f` on the output file streams lines; the parent called `Agent` on seeing a `SPAWN-NOW` line (+22 s from start). |

Not measured, and why it does not matter: a marked spawn with the coordinator stopped is relais's own code (N1 tests it); records written before `PostToolUse` are covered by booking by `message.id`; hook latency on unmarked `Bash` only mattered under B.

**Decisions taken from S0:**
- **Option A.** Worker definitions carry `isolation: worktree`; the hook answers `WorktreeCreate` with the pending dispatch's prepared tree. The pairing is the dispatch the same session's `PreToolUse(Agent)` just rewrote, and the `agent-<id>` name binds it, before `PostToolUse`. B's Bash rewrite and Edit/Write confinement are dropped, and so is the widened matcher.
- **No `allow`.** The rewrite answers `updatedInput` alone, with no `permissionDecision`. SPEC §23's rule "the hook never says yes on a person's behalf" therefore stays as it is; the per-mode allow/ask table above is not built. SPEC gains one sentence: the hook may rewrite a marked call's input to what relais itself asked for, which grants nothing.
- The hook matcher gains `SendMessage` and the `WorktreeCreate` event; `SendMessage` input is `to` and `message`.

<!-- panel: repos=relais adds= reviewers=backend body-sha=ebe649d81b94 -->
