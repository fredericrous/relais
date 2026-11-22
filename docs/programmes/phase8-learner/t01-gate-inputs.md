+++
type = "research"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais"]
+++
# What each activation gate needs that the ledger does not yet hold

## Question

For each of the four activation gates in
`docs/plans/2026-10-07-session-router.md` §5 (≥ 20 drawn with strong
outcomes and unknown rate ≤ 0.5; a success lower bound; savings in total
cost per completed task with a 90% interval excluding zero; label quality
with a missed-failure upper bound ≤ 0.1 and no flag or revert miss), which
inputs can be computed from today's ledger tables and `router::stats`, and
which are missing: a per-stratum cost per completed task on drawn versus
undrawn tasks (the arm figure exists per session, not per stratum), a
two-arm bootstrap of the difference, the missed-failure measure, the
`/relais-flag` and revert signals. Which gates can the two ready review
strata pass today?

## Look at

- `docs/plans/2026-10-07-session-router.md` §5 (lines 168–212) and §6.
- `crates/relais/src/router/explore.rs`, `stats.rs`, `report.rs` (the
  per-session split in `session_routing`), `crates/relais/src/ledger/router.rs`
  (`router_exploration_tasks`, `router_window`, `router_task_token_totals`).
- The ledger schema for `router_tasks`, `router_decisions`, `router_usage`
  (`crates/relais/src/ledger/schema/migrations.rs`).
- `crates/relais/src/learn/typed_gate.rs`: the artifact learner's gate
  evaluation, the likely precedent for `router evaluate`.
- A read-only query on a `.backup` copy of `~/.local/state/relais/ledger.sqlite`
  for the two ready strata: drawn and undrawn tasks, their sessions, and
  whether their usage rows are priced.

## Answer looks like

A table, one row per gate: inputs it needs, which exist (file and
function), which are missing, and the smallest addition that supplies each.
Then, for `sub review d3 module` and `sub review d4-5 cross-cutting`: the
gate-1 and gate-2 figures computed from the copy, and whether gate 3 is
conclusive given the unpriced sessions in their windows.

## Resolution

Measured 2026-11-22 on a `.backup` copy of the live ledger (4,576
`router_tasks`, latest 2026-11-22T21:17Z), with the join of
`router_exploration_tasks` (subagent arm) restricted to kind `review`.
Prices were not read (machine.toml is the person's); `relais report
--json` lists only `gpt-6.1-sol` and `qwen3.6:35b-a3b` as unpriced, and
`claude-fable-5-1` usage runs to 2026-11-22, inside its window, so fable
is priced.

### Inputs per gate

| Gate | Needs | Exists (file, function) | Missing | Smallest addition |
|---|---|---|---|---|
| 1. ≥ 20 drawn strong, unknown rate ≤ 0.5 | drawn strong count; drawn weak count | `router/explore.rs` `strata()` → `StratumCounts.drawn`; `MIN_DRAWN` | the unknown-rate denominator: `strata()` `continue`s past every non-strong task, so no weak count is kept; a settled reading of "unknown" (see below) | `drawn_weak` / `undrawn_weak` counters in `StratumCounts`, filled before the `continue` |
| 2. 5% lower credible bound ≥ 0.85, ≤ 0.05 below undrawn | drawn and undrawn success / strong | `StratumCounts.drawn{,_bad}`, `undrawn{,_bad}`; `router/stats.rs` `wilson` (two-sided) | a Beta quantile and a named prior ("credible"): the repo has only Wilson and Clopper–Pearson | `beta_lower(s, n, prior)` in `stats.rs` via the integer binomial identity (uniform) or a bisection on the incomplete beta (Jeffreys); or the plan rewords to a one-sided Wilson bound |
| 3. cost per completed task, drawn < undrawn, 90% interval of the difference excludes 0 | per-task cost by stratum and arm; sessions as clusters; unpriced flags | per-task usage: `router_usage` keyed `(session, task_id, agent_id)` (index `idx_router_usage_task`), 100% of the strata tasks have rows; `router/report.rs` `priced()` / `total()`; `stats.rs` `cluster_bootstrap_ratio` (one ratio) | a per-stratum, per-arm cost aggregation (the report's arms are sessions, not strata); a **paired** two-arm bootstrap of the difference: drawn and undrawn tasks share sessions (46 and 96 in d3), so resampling each arm separately treats them as independent; a rule for "unpriced session" at task granularity | `ExplorationTask` gains `session`, `task_id`, `agent_id`; a `cluster_bootstrap_difference(clusters: &[(num_a, den_a, num_b, den_b)])` resampling sessions once per draw; a ledger read of per-task usage for the strata tasks |
| 4. missed-failure upper bound ≤ 0.1 over ≥ 36 completed drawn; no flag or revert miss | per-task hashed files; later `correction`/`flag` tasks and `revert` events within 24 h; the miss count | `router_tasks.files_json`; `router_reassess` events `correction` (26 rows), `revert` (93 rows), `flag` (0 rows: accepted by `router/wire.rs`, never emitted yet); `stats.rs` `clopper_pearson_upper` | the missed-failure computation itself (no code); any file signal for reviews: 1,601 of 1,603 strata tasks have `files_json = '[]'`, so a file-overlap miss can never fire on them; `router_activations` (no table) | t02 decides how a review's miss is detected (a review edits nothing; its failure is the parent's, see the parent note below) |

### The two ready strata

| Stratum | Drawn (strong / all) | Drawn corrected | Weak drawn | Undrawn (strong / all) | Undrawn corrected |
|---|---|---|---|---|---|
| `sub review d3 module` | 30 / 92 | 3 | 62 (56 own `answered`, 5 `backfill_v45` `answered`, 1 `unknown`) | 323 / 856 | 35 |
| `sub review d4-5 cross-cutting` | 24 / 63 | 1 | 39 (33 own `answered`, 6 backfill `answered`, 0 `unknown`) | 246 / 592 | 3 |

No subagent task in either stratum has more than one decision row (the
`(session, task_id)` join does not fan out here).

**Gate 1, two readings.** Literal `unknown`: rate 1/92 and 0/63, both
pass. Every non-strong outcome (a review's own `answered` is not strong
under policy 2): 62/92 = 0.67 and 39/63 = 0.62, both fail. Which reading
the plan means is a finding for t03 (the adjustment's shape); this ticket
does not settle it.

**Gate 2, fails on both.** Success = completed over strong.

| Stratum | Drawn | 5% bound, uniform | Jeffreys | one-sided Wilson | Undrawn | Drawn − undrawn |
|---|---|---|---|---|---|---|
| d3 module | 27/30 = 0.900 | 0.768 | 0.783 | 0.774 | 288/323 = 0.892 | +0.008 |
| d4-5 cross-cutting | 23/24 = 0.958 | 0.824 | 0.848 | 0.833 | 243/246 = 0.988 | −0.029 |

Every bound is under 0.85; the "not more than 0.05 below undrawn" half
passes on point estimates on both. d4-5 under Jeffreys is 0.002 short: one
more clean drawn task passes it under that prior, five under the
uniform one.

**Gate 3, inconclusive on the plan's session rule, conclusive on a task
rule.** One session (2026-11-05 to 11-06) has `gpt-6.1-sol` usage and holds
1 drawn and 39 undrawn d3 tasks and 11 undrawn d4-5 tasks: under "an arm
containing an unpriced session is inconclusive", d3 is inconclusive in
both arms and d4-5 in its undrawn arm. Read per task, every strata task's
own usage is `claude-haiku-5-5` (step and classifier), `claude-sonnet-5-5`
or `claude-opus-5-5`, all priced, so the gate is computable once the
paired bootstrap exists. No dollar figure is given here: it needs the
machine.toml prices and the missing aggregation. Pricing `gpt-6.1-sol`
(t08) makes the session rule conclusive too.

**Gate 4, fails on sample size whatever the misses.** Completed drawn
tasks are 27 and 23; the Clopper–Pearson 95% upper bound at 0 misses is
0.128 and 0.148, above 0.1 until 36. And with no files on the review
tasks, the file-overlap miss is vacuous for them: a pass would mean
nothing.

**Every strong outcome in both strata is `outcome_source = parent`**: the
reviewer's success is its parent task's verification or acceptance. Gates
2 and 4 on these strata therefore measure the parent, not the review. t02
and t03 take this up.

### Neither stratum can activate today

Gates 2 and 4 fail on both; gate 1 fails on the broad reading; gate 3 is
inconclusive under the session rule and not yet computable under the task
rule. The four missing pieces, by size: the weak counters (gate 1), a
Beta-quantile bound or a reworded gate (2), the per-stratum cost read and
paired bootstrap (3), the missed-failure computation and a review-aware
miss signal plus `router_activations` (4).
