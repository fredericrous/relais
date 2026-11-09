# Review evidence: reliable verification and deliberate output


Previous draft only (`b8b2b879ca6d`): round 1 architect/backend/product all approve-with-changes (34,059/37,514/26,972 tokens; 45.223/81.213/37.676 s). Final backend approve-with-changes (47,396 tokens, 27.447 s); final architect/product approve (42,640/31,408 tokens; 19.114/14.598 s). All used the requested Opus override, no fallback. These are not reviews of the current updated body.

Their incorporated findings were: close verification stdin; verification-only setsid/group identity; owned PTY/no silent skip; fixed measurement window/sample; single-owner guard precedence; owner-held receipt schema; measured rollout/rollback. Four medium implementation carries were timeout-based operational observation, existing Rust aggregation/JSON facilities, generic artifact provenance rather than sibling filenames, and publishing receipt guidance before referencing it. The updated body includes these explicitly and drops the old blanket artifact-extension proposal.

Machine recording previously rejected a malformed disposition; no successful ledger write is claimed. Current plan-only restrictions defer outside-plan writes. Prior review transcripts retain the full evidence; optional Codex was not run (not part of this requested-change delta).

Current requested-change delta reviewed body `181afa4efb88`: TypeScript approve-with-changes (30,271 reported tokens, 16.677 s); backend approve-with-changes (59,551, 42.491 s). Backend's denial-semantics clarification was approve-with-changes (62,108, 21.719 s). Final backend binding on `745e5882e9b0` is approve-with-changes (63,819, 10.149 s), all three backend high findings resolved, no new blocker. All seats used the requested Opus override/retained context; no fallback or integration-agent fan-out. Reported subagent usage is not a claim about billed, uncached or quota tokens.

Accepted and incorporated high findings: explicit 1.5-second metadata deadline and unchanged unmatched-call performance; prove non-executing machine self-review distinct from human refusal; terminal refusal state with no repeat prompts and no consent from expiry; oversized-summary golden tests preserving mandatory action/evidence text with test-only byte ceilings. Backend rebound after these high corrections. The approval gate then required only TypeScript's final binding: it confirmed its high finding resolved and no new blockers (30,862 reported tokens, 5.043 s, zero tools). No whole panel was repeated.

Seven accepted findings **carried into implementation** (no additional body edits):
1. Backend: reuse per-session storage, atomic write-then-rename under a lock, explicit tested fresh/challenged/Ask/approved/declined transitions. Concurrent calls must not lose state, bypass consent or duplicate the initial challenge; approved consent is single-call, not inherited by queued peers.
2. Backend: count the existing agent's continuation tokens after self-review. There is no separately invoked reflection model, but the ordinary continuation is not free.
3. Backend: retain the input-to-expected verification checklist below in implementation contracts.
4. TypeScript: compact facts once in a shared helper across terminal and answered formatters; test key/dedup semantics and no duplicate answered re-notification.
5. TypeScript: accepted with null receipt renders no Receipt line and is not described as verified; absence is not evidence.
6. TypeScript: formatting hook adds no policy; runtime truncation must never cut mandatory instructions. Byte ceilings belong only in golden tests.
7. Final backend: only host-authenticated evidence of a new human prompt/request may reset a recorded human-denial episode. An agent's assertion that the person asked again is never evidence. If provenance is unavailable, keep it closed. Test agent retry without new human input: no Ask, no fetch.

Implementation verification checklist (carried finding 3):

| Input | Expected |
|---|---|
| Current-amont real install journey under owned PTY | SIGTTIN before; 3/3 green after; stdin EOF and owned session/group |
| Fresh successful-job output request | Distinct machine self-review, no verbose fetch; native Ask fallback only if self-review unsupported |
| Retry, human denies | No fetch and no subsequent Ask; agent assertion of new consent cannot reset it |
| Retry, human approves | One requested retrieval; no permission inherited by another call/job/attempt |
| 20 parallel equivalent requests | One initial challenge; no unapproved fetch or lost terminal-denial state |
| 1,000 unmatched Bash calls | No hook p95 regression beyond baseline noise |
| Metadata timeout/auth/parse failure | At most 1.5 seconds for query; outcome remains unknown |
| Noisy pass/fail fixture through automatic capture | Passing output <=200 B excluding delayed notice; raw log and required diagnostics intact; exit/signal/127 preserved |
| Large-summary terminal/answered fixtures | Golden byte bounds plus all applicable action/evidence strings; dedup retained; null receipt not verified |
| No available consent channel | No successful-job output retrieval; report the boundary, never auto-approve |

## Delta 2026-11-09: Phase 3 re-targeted (no prompts)

The person, shown the Phase 2 baseline: "Re-target, no prompts". Body 10e3bac11bf2. backend (opus), Delta: approve-with-changes, 5 findings (1 high: reach not shown; answered by measuring per agent, 941 first + 765 sibling-first + 5 same-agent of the 1,711 large subagent whole reads; 4 medium applied by plan-apply: `first_telling` record, no `head`, 0-false-intervention bar, consent wording), 44,439 tokens, 60.7 s. Round 2: approve, 50,428 tokens, 15.9 s. Carried into implementation: prefix the once-per-key with the rule id; the "1,637 whole reads repeated" sentence is superseded by the per-agent breakdown (770 repeats).

## Delta 2026-11-09 (2): reviewer exemption and profile briefs

The pre-activation replay showed the Read rule reaching 272 calls / 5.5 MB once reviewer agents are exempt (332 / 15.1 MB of would-refuse reads were review briefs). The person chose: ship the rule with the reviewer exemption, and slim profile briefs by replacing ADR-0043 for profile seats. Body 8d8066826a6c. backend (opus), Delta: approve-with-changes, 6 findings (2 high: brief-size arithmetic assumed an id-only index; profile seats still carry the full-rules hash), applied; 41,270 tokens, 74.9 s. Round 2: approve-with-changes, 44,655 tokens, 14.9 s. Carried: `plan-apply` HTML-escaped `<` and `>` in item 6 (read `&amp;lt;id&amp;gt;` as `<id>`); sizes are about 58 KB per profile brief and 370 KB per round.

## Phase 4 re-target delta (2026-11-09, body a7be636bc98a)

The person chose to re-target Phase 4 at relais's own prompts (reviewer 72 KB × 248, worker 68 KB × 138 over the baseline window) after quiet capture (≤ 2.3 MB) and notifications (1.0 MB) measured small, both halves, with the MCP fetch measured.

- backend (opus), round 1: approve-with-changes, 47k tokens, 76 s. High: the reviewer's `aval_rule` instruction must name the corpus (reuse `constraint_index_block`); item 4 had no owner (relais journals index ids, `output-audit` counts fetches, no SPEC change). Medium: classify while the body is present, fixture with source sha; targets from the arithmetic (24%/12%); a rollback threshold; the 19 hand-kept reviewer agent files. Low: TypeScript seat reason; ADR-0065 in `adrs:`. All accepted.
- backend, round 2: approve-with-changes, 53k, 115 s. Its own round-1 patch put prose on the `## Code review` line (unbinds profiles): moved to Omissions; Phase 5 rollback wording aligned to item 1.
- backend, round 2 final bind: approve, 56k, 153 s. Nothing carried.
