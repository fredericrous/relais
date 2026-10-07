# Reviews: relais session router

## Full reviews (reference)

### Round 1

- **plan-review-backend: rework** (36k tokens, 51 s).
  - [blocking] Cost double-counted with `usage import`.
  - [blocking] Auto-activation against SPEC §25.
  - [high] The counterfactual is a saving by construction.
  - [high] Weak labels.
  - [high] No volume numbers.
  - [medium] Cache TTLs and the return trip.
  - [medium] Idempotency of observations.
  - [low] An unobservable check; the 40-prompt confidence interval.
- **plan-review-language (rust): approve-with-changes** (49k tokens, 50 s).
  - [high] Draws are not seeded and record no propensity.
  - [high] Pooled cells are biased.
  - [medium] `quality_floor` shared with the task router.
  - [medium] The `deny_unknown_fields` upgrade order.
  - [medium] Prices computed in two places.
  - [medium] The pure-module declaration.
  - [low] The append-only migration and nullable cost.
  - [low] The promotion volume.
- **plan-review-unix: approve-with-changes** (38k tokens, 48 s).
  - [high] The pump retries forever, with no idempotency.
  - [high] Off-switch precedence; the plugin cannot read the env.
  - [medium] Output contract and exit codes.
  - [medium] Rows and cells written separately.
  - [low] Concurrent writers.
- **plan-review-react: approve-with-changes** (45k tokens, 54 s).
  - [high] The classifier promise vs `turn.step` handoff.
  - [high] `ownPrompts` ordering.
  - [high] The `agent.spawn` gate on state that does not exist yet.
  - [medium] The status line overwritten by flush.
  - [medium] The harness lacks the new hooks.
  - [medium] One spawn per turn.
  - [low] A timer leak.

### Round 2

- **plan-review-backend: approve-with-changes** (34k tokens, 44 s).
  - Decision 1 restated against SPEC L501.
  - [blocker] Selection bias from filtering on `applied` → `would_pass_gate`.
  - Hold-out and volume arithmetic.
  - `session.end` flush.
  - A flag command; the R3 lower bound.
- **Bind 1: approve-with-changes** (27k tokens, 27 s): p_g in the volume; hold-out clustering; late flags; the clear/resume hook; the agreement sample.
- **Bind 2: approve-with-changes** (26k tokens, 23 s): the Wilson bound needs 0.83 (100/120); the no-downgrade test.
- **Bind 3: approve** (24k tokens, 16 s). Low carried: the ingest-test wording "label updated unless already `inadequate`".

### The person's delta (2026-10-07, after ExitPlanMode was rejected)

- **The person's words, summarised:** "switching models is the mechanism; completing work correctly at lower total cost is the goal". They asked for:
  - capability routing;
  - automatic recovery;
  - classifier context for follow-ups;
  - authoritative subagent routing;
  - outcomes as evidence;
  - cost per completed task;
  - a revised delivery order.
- **plan-review-backend: approve-with-changes** (38k tokens, 61 s). Fixed:
  - spawn and scope growth no longer escalate;
  - a timeout keeps the tier;
  - the confidence field;
  - redo at escalation, and red runs before an edit;
  - "independence" renamed, with ≥ 36 wrong tasks and re-audit;
  - [blocking] classifier cost written to `orchestration_usage`;
  - learning volume and the cluster bootstrap;
  - the envelope plus an R3 pass.
- **plan-review-react: approve-with-changes** (59k tokens, 79 s). Fixed:
  - only router-created subtasks are routed;
  - the `tool.call` `agentId` S0 check;
  - the `/relais-routing` command;
  - pins served by Rust;
  - `/clear` and subtask cleanup;
  - background Bash ignored and notifications not classified;
  - `/relais-flag` as escalating evidence.
- **Backend binds:**
  - approve-with-changes (32k tokens): wording, R1 heading, one cost home, agentId at spawn, low-confidence rule;
  - approve-with-changes (28k tokens): unkeyed spawns;
  - approve (27k tokens). Low carried: match same-description spawns on description plus a prompt hash.

### Bypass-mode delta (2026-10-07)

- **The person's words:** "is this plan made to be run with --dangerously-skip-permissions!? because I always use this option".
- **Backend: approve-with-changes** (36k tokens). Fixed:
  - [high] the model can self-grant the envelope or R3 through Bash, so consent is honoured only from the plugin store's record, with guards and the stated limit;
  - fail-closed S0 checks;
  - seeded headless runs;
  - observable S0 items.
- **Bind: approve-with-changes** (27k tokens). Fixed:
  - store-file writes added to the guard and to S0;
  - the R3 record written by `/relais-r3` only;
  - the source labelled `cli (unattributed)`.
- **Bind: approve-with-changes** (36k tokens). Fixed: `mode_effective` computed in the plugin, the fail-closed checks observe it, and the tests moved to the plugin side.
- **Bind: approve** (26k tokens). Lows carried to implementation: the "otherwise" wording; tying the store record to the latest R3 run.
