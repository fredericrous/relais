---
status: active
branch: feat/session-router
repos: [relais]
adrs: [new: relais session routing]
---

# relais: route the session's own model, and learn from it

## Review panel

👉 **Decide:** none beyond decision 1, which the person favours. Approve if the route-then-recover design matches "complete work correctly at lower total cost".

📍 relais · S0, R1 and R1b (full automation: no R3, envelope at install, built-in prices) done; reviews approved · next: merge, upgrade the installed relais, then R2 after weeks of data. Panel: backend, lang:rust, unix, react.

**Changed by the person's review:**
- Routing goes by difficulty, scope, uncertainty and verification.
- Recovery happens mid-task on failure evidence, with no "try again" needed.
- Learned changes activate only if they lower the total cost per completed task.

📄 Full reviews: [2026-10-07-session-router.reviews.md](2026-10-07-session-router.reviews.md)

**Verdicts:**
- Rounds 1–2: approved after rework.
- The person's delta: backend and react approve-with-changes; backend approved after three binds.
- Bypass-mode delta: backend approved after three binds; consent is honoured only from the plugin's store, `mode_effective` is computed in the plugin, and the stated limit is a deliberate store edit (superseded by R1b: no store records).
- Lows carried to implementation:
  - prompt-hash matching for same-description subagents;
  - the "Otherwise it is shadow" wording;
  - tying the `/relais-r3` record to the latest R3 run (moot: R3 removed in R1b).

## Context

The person wants relais to "choose the best model for the task at hand… make us spend less and learn". In their own words, after round 2:
- switching models is the mechanism; completing work correctly at lower total cost is the goal;
- the cheapest *capable* model is the target;
- relais must recover from a weak choice without the person having to step in.

Today relais only picks models for supervised runs: a contract, a worktree, verification. The model handles everything else in the session itself, on whatever `/model` says.

**How the person works:** every session runs with `--dangerously-skip-permissions`. The design relies on no permission prompt:
- routing, recovery and learning run in mods hooks, which fire in every permission mode;
- the only questions relais asks (setup and trust; the routing envelope is recorded without asking, R1b) are the plugin's own `$.ui.ask` dialogs, not permission prompts.

S0 confirms both in bypass mode. Every headless and live check below runs with that flag.

**What bypass mode means for the consent gates, stated honestly.** With `--dangerously-skip-permissions` the model can do anything the person's shell can, including editing machine.toml or running `relais native router-envelope` through Bash. No local mechanism withstands a *deliberate* attempt; the threat model, as in SPEC §5 for the trust grant, is an **accidental or "helpful" self-grant**. So:
- **The envelope is the only switch** (R1b, the person's decision): it is recorded once, by `relais install --claude` or `/relais-routing`, and the plugin follows `router-state`'s mode. There is no plugin-store consent record and no R3 gate. A model in bypass mode could write the envelope deliberately; the same limit as SPEC §5's trust grant.
- **The accepted limit:** with the person's full shell, a model can deliberately edit machine.toml; the guards below are a nudge, not a boundary.
- `tool.check` guards refuse the model's Bash calls of `router-envelope` and `relais install --claude`, and its Write/Edit/Bash writes to machine.toml. That is a nudge, not a boundary.
- relais records every envelope write with its source (`install`, `plugin-ask`, or `cli (unattributed)`).

**The person's decisions (2026-10-07):**
- haiku classifies, and relais learns whether that call was right;
- relais acts without asking for each choice, inside an envelope the person authorizes once;
- subagents are routed too, and relais decides how;
- spend less and learn. Since the person's review, "spend less" is measured per completed task.

**What the code gives us** (exploration and the review panel):
- **Mods API:**
  - `turn.step` (an async generator) can resend a request on another model or effort, through `next({...e, model, effort})`; `e.agentId` marks a subagent's request.
  - `agent.spawn` returns `{model}`.
  - `tool.call` sees every tool's result, e.g. a Bash test command's exit code and `isError`, after `await next(e)`.
  - `$.model.complete` makes a side call.
  - `turn.complete` carries `usage`, `isAborted` and `agentId`.
  - A hook's own time is capped at 10 s; its own awaited promises count toward that, mods API calls do not.
- **The hot path:**
  - The hooks module cannot reach the coordinator socket, and each `relais` spawn is a process.
  - So nothing on the per-request path spawns relais.
- **Cost is already counted:** `usage import` books every main-session message, and every message of a subagent that is not relais's own, by `message_id`, with the model and both cache-write durations (`main.rs:5155`, `:5208-5236`). The router stores per-step usage rows (tokens, model) in `router_usage`, never a cost, and never adds them to `usage import`'s totals.
- **The task learner cannot serve turns:** its features are contract fields, and its labels are verified acceptances (`learn/features.rs:67-94`, `learn/dataset.rs:382`). A draw counts as randomized only when seeded, with its probability recorded (`learn/comparison.rs:47-50`, `crate::rng::SplitMix64`).
- **The prompt cache is per model:** a switch makes the next request rewrite the cache for the whole context (`orchestration/pricing.rs:22-23`).
- **SPEC conflicts this plan amends:**
  - §2 L29: no transparent model proxy;
  - §3 L41: native delegation is the parent's choice;
  - §3 L70: no classifier for every trivial request.
  - Activation follows §17 L501, automatic "within an explicitly authorized envelope with independent evaluation gates", so §25 L803 is unchanged.
- **SPEC invariants this plan keeps:**
  - relais's own run agents are never rewritten (§6 L257);
  - everything fails open (§23);
  - an unknown cost is never zero, and savings are API-equivalent estimates (§11).

👉 **Decision 1 (decided, R1b): the envelope, recorded without asking.** `relais install --claude` records it when the plugin is installed: installing is the authorization. `/relais-routing` records it on a machine where install did not, and turns routing off again (`/relais-routing off`). It allows:
- routing on: initial routing and automatic recovery (§2–§3), within `allowed_models`;
- downward exploration at ε ≤ 0.1;
- automatic activation of a learned adjustment that passes every gate in §5.

It writes `[session_routing] envelope = { granted_at, by, epsilon_max, source }` to machine.toml. **The mode is `on` whenever the envelope is recorded.**

**The plugin follows `router-state`'s mode** (recorded as `mode_effective` on every observation, and shown on the status line). Without an envelope it is `shadow`: it decides and records, and switches nothing. An env `RELAIS_SESSION_ROUTING=off|shadow` can only narrow it. Every learned activation is recorded with its evidence, and `relais router rollback` undoes it.

## Design

### 1. The task, and what the router knows about it

The plugin keeps `store.task` in memory, a small structured summary of the work in progress:
- the task id;
- the opening request (first 1 KB);
- kind, difficulty, scope, uncertainty and verification (below);
- the tier in use and its recent history: escalations, the verification results seen (command and pass/fail), the files touched, the subagents started;
- the turn count;
- the last outcome.

**A task starts** on a prompt the classifier marks `new_task`, and ends on the next new task, a relais run's completion, `session.end`, or `/clear`. After a `clear` the next prompt is always `new_task`.

A subagent's task state lives in `store.subtasks[agentId]`. It is created only by the router's own `agent.spawn` decision and removed when the subagent completes (its `turn.complete` with that `agentId`, per S0) or at `session.end`.

**The classifier.** `prompt.submit` makes one detached `$.model.complete` call to haiku (`maxTokens` about 120, 3 s timeout). The person's own prompts only: the `isOwn` check runs first.
- Its input: the prompt (first 2 KB) plus the `store.task` summary (about 600 bytes). That way "yes, do it", "continue" and "same issue in the other service" are read against the active task.
- It returns JSON:
  - `relation`: `new_task`, `continuation` or `correction`;
  - `kind`;
  - `difficulty` (1–5);
  - `scope`: `local`, `module`, `cross-cutting` or `unknown`;
  - `uncertainty`: `low`, `medium` or `high` (a subtle bug, unclear cause);
  - `verifiable`: whether a test or check is likely available or named;
  - `explicit`: `accept`, `correct` or `none`, with the quoted span;
  - `user_model`: a model the person named in the prompt, else null;
  - `confidence` (0–1).
- Task notifications from any agent are never classified: only the person's typed prompts are.

**A continuation** inherits the task's classification. Only `difficulty` may rise, never fall. A `correction` is recovery evidence (§3).

### 2. Initial routing: required capability, not task type

**The capability table** (data, served by `router-state`, versioned) maps (difficulty, scope, uncertainty, verifiable) to a capability tier: research, implementation or escalation. Kind is only a feature. The defaults:
- **escalation:** difficulty ≥ 4, or `uncertainty: high` together with scope beyond `local`. So a subtle concurrency investigation gets the strongest model even when its kind is "research".
- **research tier:** difficulty ≤ 2, **and** `verifiable` or a question with no edits, **and** `uncertainty: low`. A rename with tests to back it can run cheap; a hard question does not.
- **implementation tier:** everything else.

**Downgrade safety.** The router chooses a model cheaper than the person's `/model` only through the table's research rule, or a learned adjustment (§5). Classifier confidence alone never justifies it. Below 0.6 confidence a `new_task` stays at `/model`'s tier. For a continuation or a correction the task's tier is kept, never lowered. A low-confidence `correction` still counts as escalating evidence, and is recorded with its confidence.

**Pinning:**
- The first main `turn.step` of a turn races the classifier promise against `min(2 s, budget − 500 ms)`, then pins the decision in `store.route`.
- Later steps read memory only: no await, no spawn.
- A timeout or an error: inside an active task the task's current tier is kept, so nothing is downgraded. With no active task, the request goes unchanged (`next(e)`).
- **S0 checks** whether `$.model.complete` itself passes through `turn.step`; if it does, the router skips it.

**Cache gate** (main session): defined in §4.

### 3. Automatic recovery: move up on evidence, mid-task

**Reassessment points**, evaluated in memory and pure. The task a check belongs to comes from `tool.call`'s `agentId`. S0 checks that it is present. If it is not, recovery applies to the main session only, and subagents keep their initial decision.
- **a failed verification:** a Bash command the task already treats as a check fails (`tool.call` result non-zero or `isError`). A check is a test, lint or build command, matched by the repository's `relais.toml` profile commands when there is one, and by a small known list otherwise (`cargo test`, `npm test`, `pytest`, `go test`, `make check`, …);
- **repeated unsuccessful repairs:** two failed verifications in a row within the task, with edits between them;
- **the scope growing:** the files edited spread beyond the scope the classifier set (a second top-level directory, or more than 8 files for `local`);
- **a new delegated task:** `agent.spawn`, which the router decides on its own (§6);
- **a correction:** the person's next prompt has relation `correction` or `explicit: correct`, or the person types `/relais-flag`.

Only **escalating evidence** raises the tier: a failed verification **after an edit in the task** (a test-first red run or a reproduction does not count), two failed repairs, or a correction. **The scope growing and a new delegated task only re-run the capability table** with the updated scope, which may raise the tier but never lower it, and does not count as an escalation. Background Bash commands are ignored: they return no exit code.

**The rule:**
- On escalating evidence (above) the task's tier goes up **one** step, to escalation at most, starting from the **next** request. That switch is a `turn.step` override mid-turn, which is allowed by evidence. **A correction means a tier strictly above the one that produced the corrected work.** It is not merely a bypass of the cache gate.
- There is no downgrade inside a task. A new task starts from the table again.
- At most two escalations per task. After that the router stays at escalation and records `exhausted`. At escalation a further correction raises the **effort** one step (to the `max_effort` ceiling) and is recorded. It never goes below the current tier.
- The person never needs to type "try again": a failed verification after an edit is enough.

### 4. Cache-aware switching (a defined estimate)

**Switching down at a task start**, main session, only if `saving > rebuild`, where:
- `rebuild` = the current context tokens (the last request's `usage`: input plus cache reads) × the new model's cache-write rate. The TTL is the one S0 measures from the 5-minute/1-hour split. If `rebuild_back` applies it is added: the switch is expected to reverse when the task's predicted tier differs from the next likely task's.
- `saving` = `predicted_task_tokens` × (price(current) − price(new)), split into input, output and cache reads by the task kind's observed mix.
- `predicted_task_tokens` is the median total tokens of completed tasks of the same (kind, difficulty) in the ledger, served by `router-state`. With fewer than 10 such tasks it falls back to a fixed prior per difficulty: 20 k, 60 k, 150 k, 400 k, 800 k.
- An unknown price blocks a switch down.

**Switching up** (recovery or escalation) **always passes:** correctness comes first.

### 5. Evidence, outcomes and learning

**Task outcome**, decided when the task ends:
- *completed-verified*: the last verification in the task passed with no failure after it; or the task included a relais run that ended `accepted`;
- *completed-accepted*: the person's `explicit: accept` ("looks good", "ship it"), with the quote stored;
- *corrected*: any `explicit: correct` or `correction` relation, as defined above;
- *unknown*: none of the above.

**Inferred signals** (no complaint, an abort, a repeated delegation) are stored as `inferred_*` diagnostics. They are never outcomes and never feed activation.

**Missed failures, measured automatically (computed in R2; R1 records what it needs):** a task recorded as completed counts as a missed failure when, within 24 hours and in any session, either (a) a later task whose relation is `correction` (or that the person `/relais-flag`s) edits at least one of the files the completed task edited, or (b) a revert of those files is observed in `tool.call` (`git revert`, `git checkout -- <file>`, `git restore <file>`). Task records carry the hashed paths of the files they edited so the overlap can be computed across sessions. A correction inside the still-open task makes that task `corrected` instead, which gate 2 already counts.
- The **missed-failure rate** per stratum: missed failures over completed tasks, on **cumulative** counts. **Before activation** (gate 4) it is computed over the stratum's ≥ 36 completed **drawn** tasks; **after activation** over the tasks routed by the adjustment, cumulatively. Clopper–Pearson 95% upper bound; 0 misses in 36 is the smallest sample that can pass 0.1. A correction prompt inside an open task B that edits an earlier task A's files counts as A's miss only, never also as B's `corrected`.
- Aggregate agreement cannot hide it.
- After activation the same measure keeps running, with an early trigger: the stratum rolls back automatically at once on any `/relais-flag` or revert miss, or when its misses reach 4 at any n, and otherwise when its cumulative upper bound crosses 0.1 with ≥ 36 completed tasks. Every rollback is recorded with its counts.

**Observations:**
- One per task, plus one per routed request group (diagnostics), keyed `(session, task, turn, agent)`. They carry the decisions, the reassessment events, `would_pass_gate` and the drawn exploration value with its propensity, per-step usage rows (tokens, model; no cost) in `router_usage`, and the outcome.
- **Classifier calls:** S0 checks whether `$.model.complete` calls appear in the transcript. If they do not, `usage import` cannot see them. Each call's usage (tokens, model) is then sent in `router-observe` under its call id. Rust writes it to `router_usage` (source `classifier`), the router's own accounting table (R1 decision log), so gate 3 and the report read one table. They are never left out of gate 3.
- Sent batched, one `relais native router-observe` per prompt, and flushed at `session.end`, including the `clear`/`resume` reasons.
- The insert is idempotent (`UNIQUE` key, `ON CONFLICT` updates). A worse outcome may overwrite a better one, never the reverse.
- Exits: 0 recorded or duplicate; 2 bad payload, dropped and noted; 1 retried at most 5 times.

**Exploration** (envelope only):
- At a task start, with probability ε, the initial tier is one step lower than the table chose: never below research, never on a continuation or a correction.
- Seeded SplitMix64 (seed served per session, test vector shared with Rust); the draw is recorded before the cache gate, with `would_pass_gate`.
- Recovery (§3) still applies, so a too-weak exploratory choice costs a recovery, not a failed task. Recovery cost is counted against the arm that needed it.

**Learning** (Rust, `relais::router`, a pure module):
- It compares drawn and not-drawn tasks among exploration-eligible, gate-passing tasks of the same (kind, difficulty band, scope).
- A learned adjustment (for example "difficulty 3 local verifiable → research") **activates only if all of these hold:**
  1. ≥ 20 drawn tasks in the stratum, with strong outcomes only (completed-verified, completed-accepted or corrected; unknowns excluded and their rate ≤ 0.5);
  2. **success:** the 5% lower credible bound of the drawn arm's success rate ≥ `quality_floor` (0.85), and it is not more than 0.05 below the undrawn arm's;
  3. **savings:** the drawn arm's **total cost per successfully completed task** is lower than the undrawn arm's. The total includes the classifier calls, cache rewrites, subagents and recovery escalations, all priced from `router_usage`. The 90% interval of the difference must exclude zero;
  4. **label quality, automatic** (no hand labels, R1b): only strong outcomes count (verification results, accepted relais runs, explicit acceptance or correction); the stratum's missed-failure rate (above) has a 95% upper bound ≤ 0.1, and the stratum has no `/relais-flag` or revert miss; an activated stratum rolls back per the early trigger above.
- `relais router evaluate [--json]` shows every stratum and why it passes or fails. Activations and rollbacks are recorded in `router_activations`.

**Spend headline** (`relais report`, "session routing"): the **total cost per completed task** in routed sessions vs randomized held-out sessions.
- The hold-out is 10% of sessions, seeded by session id; held-out sessions run in shadow.
- Both figures are API-equivalent estimates, with counts and intervals, computed on session means.
- Per-turn figures are diagnostics only.
- Target volume: ≥ 30 held-out sessions and about 150 completed tasks per arm. S0 projects the dates.
- **Interval method:** a cluster bootstrap over sessions, used for both the spend headline and the savings gate. Tasks are clustered in sessions, and the hold-out is drawn per session.
- **Learning volume, stated plainly:** strata are kind (5) × difficulty band (3) × scope (3) = 45. A stratum needs about 200 eligible tasks to collect 20 drawn ones at ε 0.1, and gate 4's 36 completed drawn tasks need at least 360 (about 420 at 85% success, up to about 850 with 50% unknowns). With per-task costs ranging from 20k to 800k tokens, 20 drawn tasks separate only large savings (about 40% or more).
- So **R2 starts with only the 3–5 busiest strata**, by S0's tasks per stratum per week, and the others merge into their difficulty band. S0 records the projected weeks per stratum. Learned adjustments are expected to be few and slow; initial routing and recovery deliver the value meanwhile.

### 6. Subagents

- **relais's own supervised workers** (`pending` dispatch, or `next.origin.plugin === $.plugin.name`) keep their rung's approved model. They are never routed. More strictly: **a subagent's requests are routed only if the router's own `agent.spawn` decision created its `store.subtasks` entry.** Any other `agentId` passes untouched, which closes the gap between the spawn and the `agentDispatch` registration (agents.ts:61-68). A spawn without an `agentId` gets recovery once it is keyed at its first `turn.step` (S0).
- **The person's constraints are kept:**
  - a model the person named in the prompt (`user_model`);
  - a model pinned by an agent definition the person maintains (`.claude/agents/*.md` frontmatter, or an entry in the user scope). S0 checks whether `agent.spawn` shows where its `model` came from. If it does not, `router-state` serves the pins: relais reads the user-scope and project agent definitions in Rust. The plugin then needs no `$.fs`. If S0 shows `$.fs.read` works it may be used instead, spelled in `effects()` and stubbed in the harness;
  - `[session_routing] pinned_agents` in machine.toml.
- **The parent agent's own `model` parameter is a preference, not a constraint:** relais decides within the envelope. The parent's choice is recorded, and counts as a feature.
- **The decision:** the subagent's description and first 1 KB of its prompt go through the same classifier, plus the type; the capability table then applies. Recovery inside a subagent works like the main session (its own task state, keyed by `agentId`). A failed verification there escalates *its* requests.
- Models known not to spawn on this harness are excluded (`allowed_models` minus the S0 findings: for example, `[1m]` variants that fail for background agents).

## Packages

Delivery order, at the person's request: prove the switch works, then initial routing with recovery and complete-task measurement, then learning. Each package ends with the repository's checks, falsification of its key test, and the implementation review before the push.

- **S0: spike on the installed Claude Code.** This is the gate. It measures:
  - that the `turn.step` switch takes effect, including mid-turn (`usage.model`);
  - where message ids are;
  - `agent.spawn {model}`, and whether the event shows the source of `model`;
  - whether `tool.call` results expose a Bash exit code;
  - whether `$.model.complete` passes through `turn.step`;
  - classifier latency (p50/p95);
  - the cache-write tokens after a switch, and the 5-minute/1-hour split;
  - prompts and tasks per day in the person's own transcripts;
  - which models fail to spawn;
  - whether `tool.call` carries `agentId` for a subagent's Bash call;
  - whether `$.model.complete` calls appear in the transcript;
  - whether `$.fs.read` is available;
  - tasks per stratum per week, from the person's transcripts;
  - **in a session started with `--dangerously-skip-permissions`**, each written as input → expected → actual:
    - a `turn.step` override to haiku → `usage.model` = haiku;
    - `agent.spawn` returning `{model: haiku}` → the subagent's `usage.model` = haiku;
    - a failing `cargo test` through Bash → `tool.call` result with a non-zero exit;
    - `$.ui.ask` in an interactive bypass session → the dialog is shown, and the answer comes back;
    - the model is told to grant itself the envelope, through Write, Edit or a Bash redirect on machine.toml, or `relais native router-envelope` → the `tool.check` guard refuses each attempt;
  - whether the `agent.spawn` event carries the subagent's `agentId` before its first `turn.step`, and how a subagent's completion is seen (`turn.complete` with its `agentId`, or the agent list). Without an id at spawn time, the subtask is keyed when it first appears in `turn.step`, matched to the spawn by description, and recovery waits until then. If two pending router spawns share a description, the oldest is matched first. If the match is still ambiguous, both are left unkeyed: no routing override and no recovery, recorded as `unkeyed`.

  The results go in the decision log. If the switch does not take effect, stop.
- **R1: initial routing, recovery and measurement (shadow until the envelope is recorded):**
  - **Rust:** `native router-state` (mode, tiers, capability table, rates, priors, seed, hold-out), `native router-observe` (batched, idempotent), ledger v20 (`router_tasks`, `router_decisions`, views), `[session_routing]` with the envelope, the report section (cost per completed task), the ADR, SPEC §30, the upgrade note.
  - **Plugin:** `hooks/router.ts` (pure: the classifier parse, task state, the capability table, reassessment, the cache gate), the classifier at `prompt.submit`, pin-and-recover in `turn.step`, detecting checks in `tool.call`, routing in the existing `agent.spawn` hook, `/relais-routing` (records or removes the envelope) and `native router-envelope`, `/relais-flag` (marks the current task `corrected`, which counts as escalating evidence), the status line, fail-open.
- **R2: learning (envelope only):** seeded exploration, the `relais::router` learner with the four gates, `router evaluate` / `rollback`, automatic activation. It runs only after R1 has collected data.
- **R3: removed (R1b).** No hand-labelled evaluation: the person wants full automation. The classifier's quality is judged by the automatic outcomes and the missed-failure rate (§5), and routing recovers on evidence (§3).

## Verification

- **Rust:**
  - capability table: hard research → escalation, rename with tests → research, unknown scope → implementation;
  - the four activation gates at their bounds;
  - a cheaper arm with more rework does **not** activate: higher cost per completed task;
  - inferred signals never count as outcomes;
  - ingest is idempotent, and a worse outcome overwrites a better one but never the reverse;
  - 8 concurrent writers lose nothing;
  - mode: no envelope means shadow, the envelope means on, and the env can only narrow it;
  - classifier usage sent through `router-observe` is priced once and counted in gate 3;
  - the missed-failure gate: 0 misses in 35 completed fails, 0 in 36 passes; a correction one task later that edits the same file counts as 1 miss, and a correction to an unrelated file does not;
  - a stratum rolls back automatically on one `/relais-flag` or revert miss, at 4 misses, or when its cumulative bound crosses 0.1 after activation;
  - off, then `relais install --claude` → no envelope, mode shadow; the model's Bash `relais install --claude` → refused;
  - a revert of a completed task's file within 24 h counts as a miss; a correction in another session counts; a later correction keeps the task in the denominator (`completed_at_end`).
  - an older machine.toml still parses;
  - seeded draws replay exactly;
  - the cache gate: a large context with a small predicted saving does not switch down, and switching up always passes.
- **Plugin:**
  - a continuation inherits the task's classification (only difficulty may rise);
  - a failed verification escalates one tier from the next request;
  - two failed repairs plus a correction give a strictly stronger model, and no "try again" is needed;
  - three Explore spawns plus a growing scope with green tests give 0 escalations;
  - a red run before any edit gives no escalation;
  - a classifier timeout on "continue" inside an escalated task keeps escalation;
  - after `/clear`, "continue" is classified as `new_task`;
  - a relais worker's first request in the spawn→registration gap reaches the engine with its rung model;
  - two concurrent spawns with the same description: each subagent's failed check escalates only its own requests, or neither is routed (`unkeyed` recorded);
  - a subagent's failed check escalates only that subagent, or the main session only if `agentId` is absent (S0);
  - no downgrade mid-task, at most two escalations, then `exhausted`;
  - an `isOwn` prompt is never classified;
  - a model named in the prompt is honoured, an agent definition's pin is honoured, and the parent's `model` parameter is overridden within policy;
  - relais's own workers are untouched;
  - shadow and hold-out sessions apply nothing;
  - the plugin follows `router-state`'s mode;
  - the classifier timing out means `next(e)`;
  - the engine's `turn.step` receives the routed model.
- **Headless** (`claude -p --dangerously-skip-permissions`, scratch config and state, with the envelope recorded by the CLI):
  - "rename `foo` to `bar`, tests exist" runs at the research tier;
  - "why does this test fail intermittently under load" runs at escalation;
  - an edit whose test fails twice is escalated mid-turn (`usage.model` changes, recorded as a reassessment);
  - an `Explore` subagent asked for an easy lookup runs on the research model even when the parent asked for opus;
  - a relais run's worker keeps its rung model;
  - observations land, and the report shows cost per completed task for routed vs held-out sessions.
- **Live, two weeks with the envelope**, in the person's normal `--dangerously-skip-permissions` sessions (R1 on, R2 off for the first week):
  - the report shows the cost per completed task, routed vs held-out, with intervals;
  - zero relais runs failed by substitution;
  - the prompt-to-first-request p95 is within 2 s of routing off;
  - every person flag shows up as a `corrected` task.

## Decision log

### S0: spike results (2026-10-07, Claude Code 2.1.292, headless `--dangerously-skip-permissions`)

The gate **passes**. The probe plugin and its logs are kept outside the repository (scratchpad `s0probe`, `s0a`–`s0e`). Each check: input → expected → actual.

| Check | Expected | Actual |
|---|---|---|
| `turn.step` switch, main session, mid-turn (index 1 → full id `claude-haiku-4-5-20251001`) | answered by haiku | `usage.model` = `claude-haiku-4-5-20251001` on step 1, sonnet on step 0 |
| `turn.step` switch with the alias `haiku` | works | **fails**: "issue with the selected model (haiku)". `turn.step` needs a full id, so `router-state` serves full ids |
| Cache after a mid-turn switch | rewrite | step 1: `cache_read` 0, `cache_creation` 23,830 (the whole context). The gate is needed |
| Cache-write TTL split (person's transcripts, 14 days) | measure | 1 h: 434,284,952 tokens, 5 min: 4,038 → **price rebuilds at the 1 h write rate** |
| `agent.spawn` returning `{model:'haiku'}`, parent asked `opus` for Explore | haiku | the subagent ran on `claude-haiku-5-5` (spawn resolves aliases). The parent's `model` param can be overridden |
| Source of the spawn's `model` | known? | `e.model` is the Agent tool's param as given; undefined lets the agent definition, then the parent, decide. Definition pins are invisible → `router-state` serves them (Rust reads `.claude/agents`) |
| `agentId` at spawn | before the first subagent step | `result.agentId` returned after 25 ms, before the subagent's step 0. No keying gap |
| `tool.call` `agentId` for a subagent's Bash | present | present, the same id as its `turn.step` |
| Subagent completion | detectable | `turn.complete` with its `agentId` |
| Bash failure | exit visible | no exit-code field; `isError: true`, `text` "Exit code 1". `false` is an error, `ls` is not |
| `$.model.complete` passes through `turn.step` | no | no step logged during 30 classifier calls |
| Classifier latency, haiku, default effort (n=20) | measure | p50 1.66 s, p95 1.95 s; **261 output tokens with `maxTokens` 20** (thinking) |
| Classifier latency, haiku, `effort:'low'` (n=10) | measure | **p50 834 ms, p95 1.11 s, 6 output tokens**. The classifier uses `effort:'low'` and a cached `system` block |
| Message ids in hooks | present? | **absent**: `TurnStepResult.usage` and `ModelCompleteResult.usage` carry tokens and model only |
| `tool.check` deny under bypass (Write and Bash `>>` on the guarded path) | refused | both refused (`decided: allow`, the plugin denied); the file was never created |
| `$.fs.read` | available? | it exists, but resolves relative to the session cwd. Not needed (pins come from Rust) |
| Prompts per day (person's transcripts, 14 days) | measure | median 77, min 4, max 183 |
| `$.ui.ask` in an interactive bypass session | dialog shown | **not run here**, because headless cannot show it. Moved to the R1 live check |
| Models that fail to spawn | list | known from the person's notes: `[1m]` variants for background agents. `router-state` excludes them |

**Consequences for R1 (no body change; these follow from the S0 clauses the plan already contains):**
- **Cost without message ids:** the router records each step's own `usage` (tokens and model) and each classifier call's `usage`, keyed `(session, turnId, step index, agentId)`. Routed-session cost comes from these rows alone; they are never added to `usage import`'s totals, so nothing is counted twice. Priced once by Rust from `PriceTable`.
- **The rebuild rate** is the 1-hour cache-write price.
- **Volume, made concrete:** about 77 prompts a day, perhaps about 25 tasks. With a 10% hold-out, 150 completed tasks per arm takes on the order of two months; learned activations in the busiest stratum, likewise months. As the plan states, initial routing and recovery carry the value until then. The hold-out rate is a setting, so the person may raise it to shorten the spend comparison.

### R1 (2026-10-07)

- **How it was built:** the Rust and plugin halves were built in parallel against `docs/router-protocol.md`, then merged.
- **Agreed deviations:**
  - the missed-failure gate uses the exact Clopper–Pearson bound, the only one that gives the plan's "≥ 36";
  - a machine-level `pinned_agents` entry is served as `inherit`;
  - shadow sessions are a third arm of the report;
  - the plugin store path was guarded as a directory (superseded by R1b: there are no store consent records, so that guard is removed);
  - the cache gate uses a fixed token mix and no `rebuild_back` yet (no next-task prediction in R1);
  - `explored` is always false: exploration belongs to R2;
  - classifier usage is booked in `router_usage` (source `classifier`), not as an `orchestration_usage` row as §5 said. S0 found that hooks expose no message id, so the router keeps its own accounting table for every step and classifier call, and never adds it to `usage import`'s totals.
- **Found by the headless check and fixed (`0ff6977`):** "use an Explore agent with opus" moved the *main session* to opus. The classifier now reports who a named model is for (`user_model_for`):
  - a model named for a subagent is kept for the spawn and never applied to the session;
  - a model named inside the spawn's own prompt comes from the parent agent, so it is a preference the table overrides.
- **Found by the headless check, left to the person:** the person's machine.toml prices `claude-haiku-4-5(-20251001)` but not `claude-haiku-5-5`, the `model_ids` default for haiku. With the defaults the research tier is unpriced, so routing never switches down. That is safe, but saves nothing. Either set `[session_routing] model_ids = { haiku = "claude-haiku-4-5-20251001" }` or add a `claude-haiku-5-5` price.

## Verification record (2026-10-07, before push)

All checks ran in a scratch config and state, with headless `claude -p --dangerously-skip-permissions` sessions in a fresh Cargo crate. The envelope and the R3 record came from the CLI with `--source plugin-ask`. The plugin's two consent records were seeded in its store file outside the session, as the plan allows for headless runs, and were deleted afterwards.

| Check | Expected | Actual |
|---|---|---|
| `make lint` | clean | clean (38 modules, no cycles) |
| `cargo test` | green | all suites green (1,184 unit tests, plus the integration suites, including `session_router` 9/9) |
| `claude plugin validate` and `claude plugin test` | pass | validation passed; 132/132 |
| Falsified key tests | red when broken | Rust: table, mode, outcome rank, payload, R3 bound, holdout, ledger-exists, unpriced. Plugin: escalation, red run, mode_effective, store guard, subagent model |
| "Rename foo to bar, tests exist" (routed session) | research tier | all 13 requests on `claude-haiku-4-5-20251001`. Class: edit, difficulty 1, local, low uncertainty, verifiable, confidence 0.95 |
| The same prompt in a held-out session | nothing applied | sonnet throughout; the decision was recorded with `holdout=1` |
| The same prompt, with no price for the research model | no switch down | stayed on sonnet, `reason=cache_gate`, `would_pass_gate=0` |
| "Make foo subtract, run cargo test, fix what fails" | escalates after the failing test, no "try again" | haiku → the `cargo test` failed → the next request on sonnet → fixed → passed. Reassess `failed_verification` research→implementation; task `completed_verified`, 1 escalation |
| Hard investigation (intermittent wrong sums under load) | escalation | opus throughout; class: question, difficulty 5, cross-cutting, high uncertainty (2 of 2 sessions) |
| "Use an Explore agent with opus to list src/" (after the fix) | session on the table's tier, subagent on the person's opus | main on haiku (research); subagent on `claude-opus-5-5`, `reason=user_model` |
| An Explore subagent whose parent chose a model (unit) | the table overrides the parent | `routing.test.ts` / `router.test.ts` pass |
| relais's own run worker keeps its rung model | untouched | covered by plugin tests (pending dispatch, plugin origin); not run headless |
| Observations land, and the report has the section | per plan | `router_decisions`, `router_usage`, `router_reassess` and `router_tasks` are filled. `relais report` "session routing": routed 8 sessions, cost per completed task $0.59 (95% interval $0.23–$1.44); held-out and shadow arms show cost *unknown* while unpriced; unknown rate 53%; classifier 17 calls; 43,625 cache tokens rewritten after switches |
| `$.ui.ask` dialogs (`/relais-routing`, `/relais-r3`) in an interactive bypass session | shown and answered | **not run**: needs the person (the live check) |
| R2 learning, R3 evaluation on real labels | later phases | not in this PR: R2 needs weeks of R1 data, and R3 needs the person's labelled transcripts |

### R1b verification (2026-10-07, before push)

Branch binary, scratch `RELAIS_STATE_DIR`/`RELAIS_CONFIG_DIR`/`HOME`.

| Check | Expected | Actual |
|---|---|---|
| amont pre-commit on `6e72c32` (lint, `cargo test`, plugin validate and tests) | green | 28 checks passed |
| `plugin_install` and `session_router` integration suites | green | 13/13 and 11/11 |
| `relais install --claude` on a fresh machine, then again | envelope recorded once (source `install`) | `an_install_records_the_routing_envelope_once_and_a_preview_does_not` passes |
| `/relais-routing off`, then `relais install --claude` | stays `off` | `an_install_after_routing_was_turned_off_leaves_it_off` passes; CLI: `router-envelope --off` then `router-state` → `mode=off`, reason "turned off (/relais-routing off)" |
| Envelope recorded, no `[pricing]` in machine.toml | `mode=on`, Haiku 5.5 priced | `router-state` → `mode=on`, rates for haiku-5-5, sonnet-5-5, opus-5-5, fable-5-1; headless rename session routed to `claude-haiku-5-5` |
| Tombstone test falsified | red when broken | `a_tombstone_means_off_not_shadow` red with the off branch removed |

## Implementation review

- **Round 1: approve-with-changes** (93k tokens, 120 s). Fixed in `4a6244d`:
  - spawn bookkeeping never throws after `next`, and the error handler never calls `next` twice;
  - an unreadable agents directory is warned about;
  - the command messages are honest after a store failure;
  - a corrupt inferred column is an error;
  - a stdin read failure exits 1;
  - relais workers' usage is no longer booked against the person's task.
- **Delta: approve-with-changes** (41k tokens, 35 s). Kept:
  - deliberate: when only the refresh fails after the store write, the message is pessimistic; the next session's refresh corrects it;
  - deliberate: the unreadable-directory test assumes it does not run as root (CI runners are not root);
  - the classifier-usage location is recorded as an agreed deviation (R1 decision log).

### R1b: full automation (2026-10-07, the person)

- **The person's words:** "you can get Haiku pricing from internet. I don't want to have to label by hand, what the fuck is /relais-r3 and why would I want to run that. I'm looking for full automation no headache".
- **Haiku 5.5 price** taken from platform.claude.com/docs/en/about-claude/pricing (2026-10-07) and written to the person's machine.toml: the over-100k-prompt tier, so savings are not overstated. Input $0.50, output $2.50, cache read $0.05, 5-minute write $0.625, 1-hour write $1.00 per million tokens. (The up-to-100k tier is $0.10, $0.50, $0.01, $0.125 and $0.20.) `model_ids` keeps haiku → `claude-haiku-5-5`.
- **R3 removed entirely:** `/relais-r3`, `relais router r3`, the labels format, and the R3 rows' role in the mode.
- **The plugin's store consent records removed:** the mode is the envelope's.
- **`relais install --claude` records the envelope** (source `install`). On the person's machine it is recorded at the next install. The installed 0.10.1 refuses an unknown `[session_routing]` table, so writing it before the upgrade would break it.
- **Off sticks:** `/relais-routing off` records `[session_routing] off = { at, by, source }` (a tombstone) and removes the envelope. `relais install --claude` records the envelope only when there is neither an envelope nor a tombstone. `tool.check` also refuses the model's Bash `relais install --claude`. Verification: off, then install, leaves the mode `off` (the implementation review asked for a real off: no classifier call).
- **Default prices:** relais's built-in `PriceTable` holds seven current models (Haiku 5.5 at its over-100k rates, Haiku 4.5, Sonnet 5.5, Sonnet 5, Opus 5.5, Opus 5, Fable 5.1), so a machine with no `[pricing]` can still switch down. A `[pricing]` entry wins, and the rates version reads `… + relais built-in 2026-10-07` when a default was used.
- **Outcome as first recorded:** task records keep `completed_at_end` (the outcome when the task ended) beside the current outcome. A later correction raises the current outcome but leaves the task in the missed-failure denominator.
- **Attribution:** a correction is attributed to the most recent completed task, in any session within 24 h, that edited an overlapping file; a revert is a `git revert`/`git checkout -- <file>`/`git restore <file>` of such a file. Only `/relais-flag` and reverts are fully independent of the classifier, so **the measured rate is a lower bound**. Gate 4 states that, and also requires that no `/relais-flag` or revert miss exists in the activated stratum.
- **Superseded by R1b:** S0's `$.ui.ask` and consent-record checks and the R1 verification rows about `/relais-r3` and the plugin's consent records are history; they no longer describe the design. Task records gain hashed edited-file paths (R1 contract addition) for the cross-session missed-failure measure.
- **Gate 4 is automatic:** missed failures are measured from later corrections and reverts, not hand labels.
- **R1b round 1: approve-with-changes.** Fixed in `6e72c32`: off is a real `off` mode; the rates version names the built-ins; provenance kinds are matched explicitly; a holds-until on revert paths; the contract lists the seven built-in prices.
- **R1b Delta: approve** (38k tokens, 22 s), all six findings resolved. Kept deliberate: if `router-state` fails after an off, the plugin falls back to shadow (fail-open, SPEC §23), so a classifier call may run; revisit in R2.


<!-- panel: repos=relais reviewers=backend,language:rust,unix,react body-sha=3b57295144e6 -->
