# relais session router: plugin ↔ relais wire contract

The relais Claude Code plugin routes the session's own model (plan:
`docs/plans/2026-10-07-session-router.md`). It talks to relais through
five `relais native` commands. This file is the contract both sides build
against. A change to it is a change to both sides, in one commit.

All JSON is UTF-8, one object per invocation. Every object carries
`"schema": 1`. Unknown fields are refused by Rust (`deny_unknown_fields`)
and ignored by the plugin.

## `relais native router-state --session <id>`

Run in the session's cwd. It prints one object on stdout; diagnostics go
to stderr. Exits: 0 ok; 2 bad args; 1 operational failure. On any non-zero
exit, or stdout that does not parse, the plugin runs in `shadow`.

```json
{
  "schema": 1,
  "mode": "on | shadow | off",
  "mode_reason": "envelope recorded | no envelope | env RELAIS_SESSION_ROUTING=shadow | ...",
  "envelope": { "granted_at": "RFC3339", "by": "string", "epsilon_max": 0.1, "source": "install | plugin-ask | cli (unattributed)" },
  "holdout": false,
  "seed": "16 hex chars",
  "epsilon": 0.1,
  "tiers": {
    "research":       { "model": "full model id", "effort": "low | medium | high | xhigh | max | null" },
    "implementation": { "model": "full model id", "effort": null },
    "escalation":     { "model": "full model id", "effort": null }
  },
  "capability_table": {
    "version": 1,
    "rules": [
      { "when": { "difficulty_min": 4 }, "tier": "escalation" },
      { "when": { "uncertainty": ["high"], "scope": ["module", "cross-cutting", "unknown"] }, "tier": "escalation" },
      { "when": { "difficulty_max": 2, "uncertainty": ["low"], "verifiable_or_question": true }, "tier": "research" },
      { "when": {}, "tier": "implementation" }
    ]
  },
  "rates": {
    "<full model id>": { "input": 0, "output": 0, "cache_read": 0, "cache_write": 0 }
  },
  "priors": {
    "task_tokens_by_difficulty": [20000, 60000, 150000, 400000, 800000],
    "task_tokens": { "<kind>:<difficulty>": 0 }
  },
  "pins": { "<subagentType>": "<model alias or id>" },
  "excluded_models": ["full ids that do not spawn here"],
  "checks": [["cargo", "test"], ["make", "check"]],
  "check_kinds": ["test", "test"],
  "adjustments": []
}
```

- **`mode`** is `on` whenever an envelope is in machine.toml; `off` when routing was turned off (`[session_routing] off`, the tombstone `/relais-routing off` records), so no classifier call is made; `shadow` with neither; `RELAIS_SESSION_ROUTING=off|shadow` can only narrow it (any other value narrows to `shadow`). The plugin's `mode_effective` is this `mode`, and `shadow` when router-state did not answer.
- **`holdout`** is drawn by Rust from `SplitMix64(hash(session id))` against `holdout_rate` (machine `[session_routing]`, default 0.1). In a held-out session the plugin decides and records but never applies.
- **`seed`** is per session (16 hex digits). With `epsilon` it keys the exploration draw (below). **`epsilon`** is the exploration rate in force: never above the envelope's `epsilon_max`, 0 without an envelope, and 0 once the stop rule has fired.
- **`tiers`** come from the repository's `relais.toml` `[models.*]` when present, else machine defaults. Aliases are resolved to full ids by `[session_routing.model_ids]`, defaulting to haiku → `claude-haiku-5-5`, sonnet → `claude-sonnet-5-5`, opus → `claude-opus-5-5`, fable → `claude-fable-5-1`. `turn.step` refuses aliases (S0).
- **`rates`** are micro-USD per million tokens, from machine.toml `[pricing]`, else relais's built-in list (Haiku 5.5 and 4.5, Sonnet 5.5 and 5, Opus 5.5 and 5, Fable 5.1; platform.claude.com pricing, 2026-10-07. Where the page gives two values, the one that overstates no saving is kept: Haiku 5.5's over-100k tier, and Sonnet 5.5's 0.05x cache read. A machine entry wins per model); `cache_write` is the 1-hour rate (S0). A model with no price is `null`, and the plugin then never switches down to it.
- **`pins`**: subagent types whose definition (user `~/.claude/agents/*.md`, project `.claude/agents/*.md`) names a `model:` in its front matter, plus machine `[session_routing] pinned_agents`. The plugin never overrides a pinned type.
- **`checks`**: argv prefixes that count as verification. The repository's `relais.toml` profile commands, plus the built-in list: `test` kind: `cargo test`, `cargo nextest`, `npm test`, `pnpm test`, `yarn test`, `bun test`, `pnpm run test`, `npm run test`, `pytest`, `uv run pytest`, `go test`, `make check`, `make test`; `lint` kind: `cargo clippy`, `pnpm run lint`, `npm run lint`, `go vet`, `golangci-lint run`, `ruff check`, `mypy`, `pyright`, `make lint`; `build` kind: `cargo check`, `cargo build`, `pnpm run build`, `pnpm run typecheck`, `npm run build`, `npm run typecheck`, `tsc`, `go build`, `make build`.
- **`check_kinds`**: the kind of each entry of `checks`, same order and length: `test`, `lint` or `build`. A repository command that is not on the built-in list is `test`. A relais that does not serve it leaves the plugin to read the kind off the argv. The plugin adds the kind `gate` for an amont stamp (below).
- **`adjustments`**: R2's learned rules; empty in R1.

### Exploration

At a main-session `new_task` start, when the mode is `on`, the session is not held out and `epsilon > 0`, the plugin draws once and records it. The draw is defined identically in Rust (`crate::rng::exploration_draw`) and TypeScript (`explorationDraw`):

1. `u` is the first 8 bytes, big-endian, of SHA-256 over the UTF-8 bytes of `seed`, the session id and the task id, each preceded by its byte length as a 4-byte big-endian integer.
2. `x` is the first output of SplitMix64 seeded with `u`.
3. `draw = (x >> 11) * 2^-53`, a uniform in `[0, 1)`.

A shared test vector, non-ASCII ids included, and a 10,000-draw fingerprint are checked in both languages.

Below `epsilon` (`draw < epsilon`) the initial tier is one step lower than the table chose, never below research; the task keeps that floor (the table may raise the tier, never above one step under its own choice) and recovery still applies. Continuations, corrections, subagents and held-out sessions are never explored. When the table's tier is already research nothing can be lowered: `explored = false`, `propensity = 1`. The decision records `explored`, `propensity` (`epsilon` when lowered, `1 - epsilon` otherwise), `draw` and `would_pass_gate`. The cache gate still applies: a drawn switch down that fails it is recorded `explored = true`, `applied = false`. A decision the draw did not apply to records `explored = false`, `propensity = null`, `draw = null`.

**Stop rule** (Rust, evaluated by `router-state`): once at least 20 drawn tasks (explored and applied) have strong outcomes (`completed_verified`, `completed_accepted`, `corrected`), if their corrected share exceeds that of undrawn tasks (the draw ran and passed them over) in the same strata (kind x difficulty band x scope; bands 1-2, 3, 4-5) by more than 15 percentage points, `router-state` serves `epsilon = 0` and a row in `router_exploration_stops` records the stop with its counts (ledger step v24). The rule is re-read at every `router-state`; a row is written when the counts differ from the latest one. `relais report` prints per stratum the drawn and undrawn tasks with strong outcomes, both corrected shares and the projected date the stratum reaches 20 drawn tasks (`no projection: 0 drawn` without a draw), the top 5 strata then `+N more`, and the same in `--json` under `session_routing.exploration`.

## `relais native router-observe`

stdin: `{"schema": 1, "session": "<id>", "records": [ ... ]}`. Exits: 0 recorded (duplicates count as recorded); 2 bad payload (nothing written; stderr names the record); 1 operational failure (retry). All records of one call are written in one transaction.

Record kinds (field `kind`):

```json
{ "kind": "decision", "task_id": "s", "turn_id": "s|null", "agent_id": "s|null",
  "relation": "new_task|continuation|correction|subagent", "class": { "kind": "question|edit|debug|design|review", "difficulty": 3, "scope": "local|module|cross-cutting|unknown", "uncertainty": "low|medium|high", "verifiable": true, "confidence": 0.8 },
  "tier": "research|implementation|escalation", "model": "full id", "effort": "s|null",
  "reason": "table|recovery|pin|user_model|explicit_kept|timeout_kept|abstained|cache_gate",
  "mode_effective": "on|shadow|off", "holdout": false, "applied": true,
  "explored": false, "propensity": null, "draw": null, "would_pass_gate": true,
  "table_version": 1, "at": "RFC3339" }

{ "kind": "usage", "task_id": "s", "turn_id": "s|null", "step": 0, "agent_id": "s|null",
  "source": "step|classifier", "model": "full id",
  "input_tokens": 0, "output_tokens": 0, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0, "at": "RFC3339" }

{ "kind": "reassess", "task_id": "s", "agent_id": "s|null",
  "event": "failed_verification|repair_failed|scope_growth|spawn|correction|flag|revert",
  "tier_from": "…", "tier_to": "…", "effort_to": "s|null", "escalating": true,
  "files": ["16 hex"], "at": "RFC3339" }

{ "kind": "task", "task_id": "s", "agent_id": "s|null", "started_at": "RFC3339", "ended_at": "RFC3339|null",
  "class": { … }, "outcome": "completed_verified|completed_accepted|corrected|unknown",
  "completed_at_end": "completed_verified|completed_accepted|corrected|unknown|null",
  "inferred": ["aborted", "no_complaint", "respawned"], "files": ["16 hex"],
  "escalations": 0, "exhausted": false, "turns": 1, "explicit_quote": "s|null",
  "checks": [{ "name": "go vet ./...", "kind": "test|lint|build|gate", "passed": true }] }

{ "kind": "task_outcome", "task_id": "s", "agent_id": "s|null",
  "outcome": "completed_verified|completed_accepted|corrected|unknown", "at": "RFC3339" }
```

- **`files`** are the first 16 hex digits of the SHA-256 of a repo-relative path (relative to the session's cwd, no leading `./`), lowercase. On a `task`: every file the task edited (Edit, Write, MultiEdit, NotebookEdit successes). On a `reassess` with `event: "revert"`: the files a successful Bash `git revert …` (`[]`: its files are not known here), `git checkout [<ref>] -- <paths>` or `git restore <paths>` (not `--staged` alone) put back; a revert is never escalating. Optional, default `[]`. R2 matches reverts and later corrections to the tasks whose files they touch (plan §5, missed failures).
- **`table_version`** is the capability table's `version` the decision was made under. Optional, default `null`.
- **`checks`** are the checks the task ran (its last 20), each with the kind of check it matched (`test`, `lint`, `build`) or `gate` for an amont stamp. `name` is the command, or `amont-gate@<first 7 of the tree>`. Optional, default `[]`; a later record with checks replaces the stored list.
- **`task_outcome`** amends a subagent's task with its parent's outcome. A subagent's task is sent when the subagent completes, before its parent ends; when the parent ends the plugin sends one `task_outcome` per router-created subagent task it spawned (never one for an `unknown` outcome). relais sets that task's `outcome` and `outcome_source = parent` only when the stored outcome is `unknown`, and does nothing when the task has no record yet. A task's own record sets `outcome_source = own` (every task recorded before ledger step v23 is `own`), and a later own record of a higher rank takes the outcome back.
- **`completed_at_end`** is the outcome when the task ended (`null` while it is open). relais keeps the first one it receives; a later record never moves it, so a correction that arrives after the end is measured against it. Optional, default `null`.

Idempotency keys:
- decision: `(session, task_id, turn_id, agent_id)`;
- usage: `(session, turn_id, step, agent_id, source)`;
- reassess: `(session, task_id, agent_id, event, at)`;
- task: `(session, task_id, agent_id)`;
- task_outcome: `(session, task_id, agent_id)` (it updates that task).

A `task` record may be sent again with a later outcome. Its rank is `corrected` > `completed_verified` = `completed_accepted` > `unknown`. A higher-ranked outcome overwrites a lower one; a lower one never overwrites. `unknown` never overwrites anything. `files` and `inferred` only gain; `completed_at_end` is never overwritten once set.

## `relais native router-envelope --by <name> (--epsilon-max <f> | --off) [--source plugin-ask]`

Writes `[session_routing] envelope = { granted_at, by, epsilon_max, source }` to machine.toml (locked, atomic, comments and file mode kept, as `trust grant` does), removes any `off` tombstone, and writes a ledger provenance row. Prints `{"schema":1,"envelope":{…}}`. With `--off` it removes the envelope and records `[session_routing] off = { at, by, source }` instead, so routing stays off: `relais install --claude` records an envelope only when neither an envelope nor `off` is there. It prints `{"schema":1,"envelope":null,"off":{…},"removed":bool}`. Exits 0, 2 bad args, 1 failure. Run by the plugin's `/relais-routing` (after `$.ui.ask`) and `/relais-routing off`. Typed in a shell it records `source: cli (unattributed)`.

`relais install --claude --write` records the envelope itself (`by: "relais install"`, `epsilon_max: 0.1`, `source: install`) when it installs the plugin and machine.toml has neither an envelope nor `off`; a preview writes nothing.

## `relais native gate-stamp --cwd <repo>`

Read-only. Prints what amont's gate stamped on the TREE of `<repo>`'s `HEAD`, as one JSON object on stdout; diagnostics go to stderr. Exits: 0 (a missing stamp included); 2 `--cwd` is not a directory; 1 git failed.

```json
{ "stamped": true, "tree": "<40 hex>",
  "passed": [{ "gate": "s", "epoch": 1760000000 }],
  "failed": [{ "gate": "s", "epoch": 1760000100 }] }
{ "stamped": false }
```

**The amont note (an external contract**, amont `docs/gate-evidence.md`). amont's pre-push hook and `amont run pre-push` write a git note under `refs/notes/amont-gate`, keyed by TREE (not by commit), so the same content pushed from two commits carries one stamp. relais reads `git notes --ref=refs/notes/amont-gate show HEAD^{tree}`. The first line must start with `amont-gate-v1`; any other first line (another version) is no evidence and prints `{"stamped":false}`. The other lines read `run <epoch> <gate> pass|fail` (`epoch` in seconds); a line of any other shape is skipped. No note prints `{"stamped":false}`.

The plugin calls it at a task's end, for each repository (`git rev-parse --show-toplevel` of the directories the task edited files in), asynchronously and within 1 s, and fails open. A `pass` whose epoch is inside the task's window, with no `fail` inside the window after it, is a passing check `amont-gate@<tree7>` of kind `gate`; a `fail` inside the window is a failed check; runs outside the window are ignored. Reading the end state covers pushes run in the background, which return no exit code.
