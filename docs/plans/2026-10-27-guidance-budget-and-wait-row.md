---
status: active
branch: fix/guidance-budget-and-wait-row
repos: [relais]
adrs: []
---

# relais keeps the best guidance that fits, and the pane shows a waiting run

## Review panel

👉 **Decide:** none — approve if trimming the ranked guidance by score, instead of dropping it whole, and a wait row that is never cut are the fix.
📍 relais · plan reviewed · next: branch fix/guidance-budget-and-wait-row, routed through relais. Panel: backend (opus-5-5), codex (gpt).
**Changed by review:** score kept without `Eq`; code rules skipped only where shown in full; undecided keys kept; wait row handles runs with no step.
📄 Full reviews: [2026-10-27-guidance-budget-and-wait-row.reviews.md](2026-10-27-guidance-budget-and-wait-row.reviews.md)
**Verdicts:** backend approve-with-changes ×2 (2 high fixed); codex approve-with-changes; 6 findings carried into implementation.

## Context

The person saw two runs side by side in the relais pane (2026-10-27): a replay and the Phase 1 run of `2026-10-27-answered-outcome`. Two things were wrong.

- **Only one run showed the activity row.** The second run was not running: it waited 30 minutes for a heavy local slot and ended `blocked (admission_wait_exhausted)`. Its events said so every 30 s (`waiting_for_slot`, `heavy_local`, `position 1`), and `timeline.ts:382-390` turns that into `m.wait`. The pane showed none of it:
  - `activityRow` (`pane.ts:238-248`) is drawn only under a running check;
  - the wait text is a dim tail row (`pane.ts:161`), and dim rows go in the last trim stage (`pane.ts:318`);
  - when the frames do not fit, every live frame but the kept one shrinks to its head row (`minimalFrame`, `pane.ts:326-329`), with no wait row.
  So a queued run looked dead.
- **Both runs said `guidance: relevant missing (dropped over budget: guidance measured 17880/19146 bytes against a 16384-byte budget)`.** `fit_budget` (`context/guidance.rs:315-346`) drops the whole ranking when the index plus the ranking exceed `GUIDANCE_BUDGET_BYTES` (16 KiB). `aval relevant --top 20` returns 20 keys and 20 rules, each list sorted by `score`, descending (checked live), about 10 KB and 9 KB. So almost every run gets no ranked guidance at all, and the planner and reviewer lose it too, since they read the trimmed manifest (`context/mod.rs:1005`). Two more defects:
  - `parse_relevant_output` (`guidance.rs:489-518`) discards `score`, so there is no basis to keep the best entries;
  - the worker and planner render the ranking with no skip list (`runner/prompt.rs:77`, `runner/scheduler.rs:1310`), so a ranked rule that is also a `governs: code` rule is printed twice: once in full under "code rules in force", once in the ranking (3 rules, 1.3 KB, on the Phase 1 contract).

**Intended outcome:** a run keeps the best-ranked entries of each list that fit (D3), and its receipt says how many it kept (`relevant partial (kept K of N by rank, M bytes over)`; N counts the entries rendered after the code-rule skip; today 17880 − 1300 (3 code rules) ≈ 16580 bytes, about 200 over, so about one entry is cut). A rule already given in full is not ranked again. A waiting run shows what it waits on, with a spinner, wherever its frame is drawn; a shrunk frame above the kept one can still be cut behind `↑ N rows above` (`pane.ts:405-408`).

## 1. Ranked guidance trimmed by rank (Rust)

- **Keep the score.** *(Dropped by D3.)* `RawKey`, `RawRule` (`guidance.rs:470-480`), `RankedKey` and `RuleRef` (`:123, :216`) gain `score: Option<f64>`, `#[serde(default)]` so older manifests still parse. `f64` is not `Eq`, and `RuleRef`, `RankedKey`, `Relevant` and `Guidance` derive `Eq` (`guidance.rs:122, :215, :226, :241`): drop `Eq` from the four (keep `PartialEq`) once `findReferences` shows no `Eq`/`Hash` bound on them, else store the score as fixed-point `Option<i64>` (score × 100).
- **One ranked list.** *(Dropped by D3: each list keeps aval's order.)* `Guidance::ranked_entries(skip_rules)` merges active keys with a `choice` and the rules not in `skip_rules`, ordered by `score` descending; an entry without a score sorts after every scored one, keeping aval's order. `relevant_entries` renders that order. The prompt therefore interleaves keys and rules by score instead of printing all keys first.
- **Skip what is already in full.** `relevant_rules` stays whole in the manifest. `fit_budget` measures the ranking with the `code_rules` ids skipped; the worker (`prompt.rs:77`) passes as its skip list the ids `worker_code_rules_block` actually showed, so a code rule cut by the 64 KiB budget or unavailable from a manifest read back from disk (`prompt.rs:343-365`) stays in the ranking. The planner keeps `&[]` (`scheduler.rs:1310`): it has no code-rules block, so the ranking is its only copy. The reviewer already skips `stated` (`review/mod.rs:582`).
- **Trim, don't drop.** *(D3: cut from each list's own tail in proportion to its bytes, not a merged prefix.)* `fit_budget` keeps the longest prefix of the ranked list whose bytes, with the index, fit the budget; it then rebuilds `relevant_keys` and `relevant_rules` from that prefix, keeping every key that renders no entry (not active, or no `choice`): it costs no prompt bytes and the manifest records it (`context/mod.rs:1498`). It records one `Relevant` gap only when something was cut: `GapCause::OverBudget { measured, budget }` (no new variant, as `fit_statements` does at `prompt.rs:291-303`) with the detail `kept K of N by rank, M bytes over` . The index rule is unchanged: dropped only when it alone is over the budget.
- **Wording.** `GuidanceGap::describe` (`guidance.rs:99-109`) says `relevant partial (…)` when K > 0 and `relevant missing (…)` when K = 0, as the `Reviewer` source already says "partial".
- **Docs.** `docs/SPEC.md:307` ("the ranking is dropped first, then the index") becomes the proportional per-list cut of D3, then the index.

## 2. The wait row is never cut (plugin)

- **Spinner for a wait.** `activityRow` (`pane.ts:238-248`) also draws when the active step runs no check and `m.wait` is set: `│ ⠋ waiting for a heavy local slot, position 1 · 21m`. Under a running check it is unchanged (`running · last line Ns ago`).
- **Kept like the activity row.** That row is added to `keep` in `drawFrame` (`pane.ts:296-323`) and survives the cut filter (`:414-419`); the dim tail row at `pane.ts:161` is not drawn when the activity row already carries the wait, so the text is not shown twice. The `│ waiting for output` placeholder (`pane.ts:225`) is not drawn beside the wait row. A run with no active step yet draws the wait row after its head, since `drawFrame` calls `activityRow` only inside the active-step loop (`pane.ts:299-310`).
- **A shrunk frame keeps it too.** `minimalFrame` (`pane.ts:326-329`) returns the head row and, for a live run with `m.wait`, the wait row. The head row is unchanged, so `ONE_LIVE_RUN` (`pane-layout.test.ts:38`) still matches byte for byte.

## Verification

- `make check` → exit 0.
- Rust tests (`guidance.rs`, `context/mod.rs`, `runner/tests/mod.rs`):
  - a ranked list of 20 keys and 20 rules over the budget keeps entries of both lists, each cut from its own tail in proportion to its bytes (D3), and the gap detail names `kept K of N by rank`;
  - a list that fits records no gap; an empty prefix gives `relevant missing`;
  - *(dropped by D3: no `score` field)* entries without `score` keep aval's order;
  - a ranked rule that is also a code rule is rendered once in the worker prompt; with the code-rule text unavailable it is rendered in the ranking; the planner prompt still carries it;
  - *(dropped by D3)* a manifest written without `score` still parses;
  - the existing `over_budget_drops_relevant_first_then_the_index` and `over_the_guidance_budget_the_relevant_block_goes_first` are rewritten to the new rule, not deleted.
- Plugin tests (`claude-plugin/tests/views/pane-layout.test.ts`):
  - a live run waiting for a slot (a `waiting_for_slot` event, no running check) shows the spinner row with the wait text;
  - two live runs in a 12-row room: the shrunk frame shows its head and its wait row;
  - a running check still shows `running · last line Ns ago`, and the wait text appears once.
  - a `waiting_for_slot` event before any `phase` event: the frame shows the wait row, and no `waiting for output` row beside it.
- Live: start one long run and a second that queues for the heavy slot; the pane shows the second run's wait row with its position and minutes. On the next run of the Phase 1 contract, the receipt note says `relevant partial (kept K of N by rank …)` with K > 0, and `manifest.json` lists the kept keys and rules.

## Not in scope

- Raising `GUIDANCE_BUDGET_BYTES` or `RELEVANT_TOP`.
- The heavy-slot scheduling itself and `admission_wait_seconds`.
- Head-row width limits in the pane.

## Decision log

- D1 (2026-10-27, person) Fix both on a separate branch now: "Oui, branche à part (Recommandé)".
- D2 (2026-10-27) Trim by aval's `score`, both lists merged, since each list is sorted by score and both come from one query; no new `GapCause` variant. Open: one query does not prove one scale (keys 81.26→18.78, rules 51.50→27.09); before merging, cite where aval computes both scores, else trim each list by its own rank in proportion to its bytes.
- D3 (2026-11-17) D2 settled: not one scale. aval scores keys with text, path, mention and co-change terms over the key index (`aval/crates/greentopo/src/relevant.rs:563-568`) and rules with text and path terms over a separate rule index (`:622-623`). So the fallback applies: each list is cut from its own tail, the list keeping the larger share of its bytes losing the next entry, and no `score` field is added (the "keep the score" and "one ranked list" bullets fall). #323 (2026-10-28) had meanwhile made `fit_budget` trim from the end (rules first) with a `relevant partial` gap; this branch replaces that order with the proportional one and adds the code-rule skip and the wait row.
- D4 (2026-11-17) Next: the live check below runs after the release that carries this branch is installed (pane and receipt need the installed plugin and binary); the plan stays `active` until then.

## Verification record (2026-11-17, before the push)

- Part 1 candidate (relais run-660721fe86240-13045, verified by run-66072a0bbb487-11160) → all five criteria met, default profile green → accepted.
- `cargo test -p relais --lib context::guidance` after the review fixes → pass, incl. `over_budget_cuts_each_list_from_its_own_tail_in_proportion`, `a_ranking_that_fits_records_no_gap`, `a_fully_cut_ranking_is_relevant_missing_and_the_index_survives` (was `over_budget_drops_relevant_first_then_the_index`), `a_key_that_renders_nothing_survives_the_cut`, `an_index_over_the_budget_alone_does_not_cut_a_ranking_that_fits`; `runner/tests`: `a_ranked_code_rule_shown_in_full_is_rendered_once_and_the_planner_keeps_it`, `a_ranked_code_rule_without_text_stays_in_the_ranking`; `context/mod.rs`: `over_the_guidance_budget_the_ranking_is_cut_and_the_index_stays` (was `…_the_relevant_block_goes_first`) → pass.
- Plugin tests (commit gate `plugin-test`, 30 checks) → the four pane-layout tests of part 2 pass; `ONE_LIVE_RUN` unchanged → pass.
- `greengate run --stamp pre-commit-clippy` on the rebased tree → pass.

## Implementation review

- Round 1: approve-with-changes (opus, 105k tokens, 87 s). Fixed: index-over-budget cut the ranking (2), plan wording (4), wait-once assertion (5). Finding 1 answered: tests renamed, not deleted. Deliberate: 3, planner overrun by code rules is the plan's design (part 1, "Skip what is already in full").
- Delta: approve-with-changes (opus, 37k tokens, 43 s); 1–5 resolved. Fixed: 7 (plan text after D3). Deliberate: 6, `OverBudget.measured` keeps counting a dropped index, as before this branch; the detail's `M bytes over` is the ranking's.


<!-- panel: repos=relais adds= reviewers=backend,codex body-sha=94c6a5ab6e70 -->
