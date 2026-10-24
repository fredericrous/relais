---
canonical: github:fredericrous/aval/docs/plans/2026-10-23-code-rules-reach-writers-reviewers.md
phases: [3]
status: done
branch: feat/code-rules-full-text
---
# Code rules reach the agent that writes and the agent that reviews

Pointer. The plan and its review panel live in aval (merged in aval#42); the
ADR is decisions ADR-0043 (decisions#86). This repository carries Phase 3:
relais workers and its reviewer get the full text of the `governs: code`
constraints and the applicable decisions, the reviewer blocks on a violation
the diff introduces, and workers get the read-only aval MCP tools.

Carried into implementation from the plan review:
- `fit_statements` measures the rendered text (statement + body) and takes the
  gap source (worker or reviewer); a test feeds a body larger than the budget.
- Only exit 2 and the stale-pack case keep every id in the index; exit 1/3 and
  a timeout lose the index as today. The "aval lacks `--full`" gap is recorded
  only when the retry succeeds.
- Decisions: `aval relevant --top 20`; an empty diff skips the call (zero
  `--path` is exit 2, not an old aval).
- Review is conditional (routing or changed verification inputs): say so, and
  count the unreviewed share over 20 runs.
- Measured on this branch: relais has 55 code constraints, 59 KB as `--full`
  text (the plan estimated 52 and 55 KB).

## Verification record

Before the push (branch tree, `cargo test -p relais` via the pre-commit gate, 1,865 tests):
- Guidance fetch: one `aval rules --level constraint --full --json`; `governs: code`
  rules split out with a body hash in the manifest → tests
  `full_rules_json_splits_the_code_rules_with_a_body_hash`,
  `a_code_rule_stays_out_of_the_index_but_in_the_rules` pass.
- Fallbacks: old aval (exit 2) retried, stale pack keeps every id, gap names the
  fix and never "index missing" → `an_old_aval_fallback_keeps_every_id_in_the_index`,
  `a_stale_pack_fallback_keeps_every_id_in_the_index` pass.
- Worker prompt: code-rule bodies before `[relais task]`, 64 KiB budget, whole
  rules dropped with a run event and a receipt line →
  `the_worker_prompt_carries_code_rule_bodies_before_the_task_line`,
  `a_worker_code_rule_over_budget_is_a_run_event_and_a_receipt_guidance_line` pass.
- Reviewer: full code rules, "acceptance criterion or code rule id", deletions,
  decisions for the changed files (`--top 20`, empty diff skipped), a notice when
  they are missing → `the_review_prompt_asks_for_code_rule_ids_and_new_violations_only`,
  `the_reviewer_gets_the_active_decisions_for_the_changed_files`,
  `a_failing_decisions_lookup_still_reaches_the_review_verdict_and_is_a_receipt_line` pass.
- Workers: the three aval MCP tools on all 19 worker agents (`plugin_agents.rs`);
  the instruction names the main checkout (linked-worktree fixture test).
- On real data: relais's refreshed pack yields 55 code constraints, 59 KB as
  `--full` text (aval 1.10.0).

- How Claude Code resolves `mcp__*` in a plugin subagent's `tools:` → asked of
  the Claude Code docs (https://code.claude.com/docs/en/sub-agents) → subagents
  inherit the parent session's MCP tools narrowed by `tools:`; a `tools:` list
  of only built-ins gets no MCP tools (why workers never saw aval);
  `mcpServers` in frontmatter is ignored for plugin agents. Listing the three
  tools is enough where the session has the aval server.
- `make plugin` → 313/313 → 313 pass, 0 fail (29.5 s) at load average 3.5,
  2026-10-24; a first run at load average 224 failed 11 tests, every one
  "timed out after 5000 ms".

Not observed before the push, each run right after the release (they need this
relais installed; a branch binary on the real state would migrate the ledger):
- a dispatched worker prompt carries the `names.reveal-intent` body, and the
  worker holds the three `mcp__aval__*` tools;
- a seeded `const r = store.router` patch draws a finding citing the rule;
- a patch covered by an explicit overriding decision draws no finding against
  the overridden rule.

## Decision log

- 2026-10-24 — The reviewer's decisions are not resolved a second time: aval
  reports each ranked key's state and choice from `aval resolve` on that exact
  key ("the verdict reported beside each key comes from aval_resolve against
  the exact key"), so the carried choice is the resolved verdict at the
  default scope. relais runs no scoped resolution; `holds-until:` at
  `runner/review/mod.rs` names the follow-up (carry a scoped decision with its
  scope once relais resolves by scope).

- 2026-10-23 — Phase 3 ran as six relais tasks. Two workers hit wall time
  under machine load (load average up to 350) and were salvaged by hand after
  their edits passed the full gate; one candidate failed only `cargo fmt` and
  was formatted by hand.
- 2026-10-23 — Follow-up, not done: the reviewer decisions room counts framing
  bytes as reserved (review finding, low), so it is understated.

## Implementation review

- Round 1 approve-with-changes (89k, 121 s); Delta approve-with-changes (94k,
  20 s); a third pass on the final tree approve (98k, 16 s) — the tree moved
  after the Delta (`c56d52b7`), and the push hook binds the reviewed tree.
- Fixed: old-aval gap names 1.10.0, `TextUnavailable` cause, `holds-until:`
  on default-scope decisions, override check and docs answer recorded,
  `make plugin` 313/313 under load.
- Next: release relais, install it, run the three live checks; then tick
  Phases 1-4 in the canonical plan in aval.
