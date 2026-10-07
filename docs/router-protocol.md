# relais session router: plugin ↔ relais wire contract

The relais Claude Code plugin routes the session's own model (plan:
`docs/plans/2026-10-07-session-router.md`). It talks to relais through
four `relais native` commands. This file is the contract both sides build
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
  "mode_reason": "envelope and r3 recorded | no envelope | no r3 pass | env RELAIS_SESSION_ROUTING=shadow | ...",
  "envelope": { "granted_at": "RFC3339", "by": "string", "epsilon_max": 0.1, "source": "plugin-ask | cli (unattributed)" },
  "r3": { "id": "string", "passed": true, "at": "RFC3339", "source": "plugin-ask | cli (unattributed)" },
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
  "adjustments": []
}
```

- **`mode`** is relais's side only: `on` when an envelope is in machine.toml, the latest R3 row has `passed: true`, and the env does not narrow it; otherwise `shadow`; `off` when the env says so. The plugin computes `mode_effective` = the narrower of `mode` and its own two `$.store` records (`envelope_consent`, `r3_consent`). The `r3_consent` record must name the same `r3.id`.
- **`holdout`** is drawn by Rust from `SplitMix64(hash(session id))` against `holdout_rate` (machine `[session_routing]`, default 0.1). In a held-out session the plugin decides and records but never applies.
- **`seed`** is per session: the plugin's exploration draws are `SplitMix64(seed, draw_index)`. They are R2's; R1 sends `explored: false`.
- **`tiers`** come from the repository's `relais.toml` `[models.*]` when present, else machine defaults. Aliases are resolved to full ids by `[session_routing.model_ids]`, defaulting to haiku → `claude-haiku-5-5`, sonnet → `claude-sonnet-5-5`, opus → `claude-opus-5-5`, fable → `claude-fable-5-1`. `turn.step` refuses aliases (S0).
- **`rates`** are micro-USD per million tokens, from `PriceTable`; `cache_write` is the 1-hour rate (S0). A model with no price is `null`, and the plugin then never switches down to it.
- **`pins`**: subagent types whose definition (user `~/.claude/agents/*.md`, project `.claude/agents/*.md`) names a `model:` in its front matter, plus machine `[session_routing] pinned_agents`. The plugin never overrides a pinned type.
- **`checks`**: argv prefixes that count as verification. The repository's `relais.toml` profile commands, plus the built-in list `cargo test`, `cargo nextest`, `npm test`, `pnpm test`, `yarn test`, `bun test`, `pytest`, `uv run pytest`, `go test`, `make check`, `make test`.
- **`adjustments`**: R2's learned rules; empty in R1.

## `relais native router-observe`

stdin: `{"schema": 1, "session": "<id>", "records": [ ... ]}`. Exits: 0 recorded (duplicates count as recorded); 2 bad payload (nothing written; stderr names the record); 1 operational failure (retry). All records of one call are written in one transaction.

Record kinds (field `kind`):

```json
{ "kind": "decision", "task_id": "s", "turn_id": "s|null", "agent_id": "s|null",
  "relation": "new_task|continuation|correction|subagent", "class": { "kind": "question|edit|debug|design|review", "difficulty": 3, "scope": "local|module|cross-cutting|unknown", "uncertainty": "low|medium|high", "verifiable": true, "confidence": 0.8 },
  "tier": "research|implementation|escalation", "model": "full id", "effort": "s|null",
  "reason": "table|recovery|pin|user_model|explicit_kept|timeout_kept|abstained|cache_gate",
  "mode_effective": "on|shadow|off", "holdout": false, "applied": true,
  "explored": false, "propensity": null, "draw": null, "would_pass_gate": true, "at": "RFC3339" }

{ "kind": "usage", "task_id": "s", "turn_id": "s|null", "step": 0, "agent_id": "s|null",
  "source": "step|classifier", "model": "full id",
  "input_tokens": 0, "output_tokens": 0, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0, "at": "RFC3339" }

{ "kind": "reassess", "task_id": "s", "agent_id": "s|null",
  "event": "failed_verification|repair_failed|scope_growth|spawn|correction|flag",
  "tier_from": "…", "tier_to": "…", "effort_to": "s|null", "escalating": true, "at": "RFC3339" }

{ "kind": "task", "task_id": "s", "agent_id": "s|null", "started_at": "RFC3339", "ended_at": "RFC3339|null",
  "class": { … }, "outcome": "completed_verified|completed_accepted|corrected|unknown",
  "inferred": ["aborted", "no_complaint", "respawned"], "escalations": 0, "exhausted": false, "turns": 1,
  "explicit_quote": "s|null" }
```

Idempotency keys:
- decision: `(session, task_id, turn_id, agent_id)`;
- usage: `(session, turn_id, step, agent_id, source)`;
- reassess: `(session, task_id, agent_id, event, at)`;
- task: `(session, task_id, agent_id)`.

A `task` record may be sent again with a later outcome. Its rank is `corrected` > `completed_verified` = `completed_accepted` > `unknown`. A higher-ranked outcome overwrites a lower one; a lower one never overwrites. `unknown` never overwrites anything.

## `relais native router-envelope --by <name> --epsilon-max <f> [--source plugin-ask]`

Writes `[session_routing] envelope = { granted_at, by, epsilon_max, source }` to machine.toml (locked, atomic, comments kept, as `trust grant` does) and a ledger provenance row. Prints `{"schema":1,"envelope":{…}}`. Exits 0, 2 bad args, 1 failure. Run by the plugin's `/relais-routing` handler after `$.ui.ask`. Typed in a shell it records `source: cli (unattributed)`.

## `relais router r3 --record --id <id> (--passed | --failed) [--source plugin-ask]`

Writes a ledger provenance row only. `relais router r3 --eval <labels.jsonl> [--json]` computes the R3 measures from a hand-labelled file: tier accuracy, continuation detection, `previous`/outcome agreement and the missed-failure rate, with Wilson bounds and the plan's gates. It prints the verdict and an `id`. The plugin's `/relais-r3` runs `--eval` and shows the verdict through `$.ui.ask`; on yes it runs `--record` with `--source plugin-ask` and writes its own `r3_consent` store record naming that `id`.

## R3 labels (`relais router r3 --eval <labels.jsonl>`)

One JSON object per line (blank lines are skipped); unknown fields are
refused, and an `item` may appear once. Exit 2 names the first bad line.

```json
{ "item": "string, unique",
  "human":  { "tier": "research|implementation|escalation|null",
              "relation": "new_task|continuation|correction|null",
              "outcome_correct": true },
  "router": { "tier": "research|implementation|escalation|null",
              "relation": "new_task|continuation|correction|null",
              "outcome": "completed_verified|completed_accepted|corrected|unknown|null" } }
```

A `null` (or absent) field leaves the item out of the measures that need
it. The measures, each with its 95% bounds:

- **tier accuracy**: `router.tier == human.tier`, over items with both. Gate: Wilson lower bound ≥ 0.75.
- **continuation detection**: `router.relation == "continuation"`, over items whose `human.relation` is `continuation`. Reported, no gate.
- **outcome agreement**: over items with `human.outcome_correct` and a router outcome other than `unknown`, the router agrees when it said `completed_*` and the person said correct, or `corrected` and the person said wrong. Gate: Wilson lower bound ≥ 0.75 (100 of 120 passes, 99 does not).
- **missed failures**: over items with `human.outcome_correct: false`, the share the router called `completed_*`. Gate: the exact (Clopper–Pearson, two-sided 95%) upper bound ≤ 0.1, which with 0 misses needs ≥ 36 such items.

The verdict passes when all three gates pass. Its `id` is `r3-` and the
first 16 hex characters of SHA-256 of the file's bytes; `--record --id`
and the plugin's `r3_consent` record name it. `--json` prints
`{schema, id, items, tier_accuracy, continuation_detection,
outcome_agreement, missed_failure, passed}`, each measure as
`{successes, n, rate, lower, upper, gate, passed}`. A failed verdict still
exits 0: the verdict is the output.
