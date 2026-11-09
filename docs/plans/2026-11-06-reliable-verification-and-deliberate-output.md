---
status: active
branch: fix/verification-terminal-isolation
repos: [relais, amont-agent, decisions]
adrs: [decisions:ADR-0039, decisions:ADR-0042, decisions:ADR-0043, decisions:ADR-0050, decisions:ADR-0053, decisions:ADR-0061]
---
# Reliable verification and deliberate output

## Review panel

👉 **Decide:** none — approve if Phase 3 should ship the reviewer-exempt Read rule and slim profile briefs under a new ADR replacing ADR-0043 for profile seats.
📍 relais, amont-agent, decisions · Phase 3 rule committed (unpushed) · next: the ADR in decisions, then the amont-agent brief change. Delta panel: backend (opus).
**Changed by review:** reach corrected by replay (272 calls / 5.5 MB with reviewers exempt); profile index is ids and titles only; RULES hash binds through implementation-review; ADR lands before the code.
**Verdicts:** backend approve-with-changes → approve-with-changes (round 2, mediums carried). Carried: read item 6's `&amp;lt;id&amp;gt;` and `&amp;lt;&amp;lt;&amp;lt;RULES sha=` as `<id>` and `<<<RULES sha=` (a `plan-apply` escaping bug, to fix); the sizes are about 58 KB per profile brief and 370 KB per round, not 55 and 360.
📄 Full reviews: [review evidence and implementation carries](2026-11-06-reliable-verification-and-deliberate-output.reviews.md).

## Context

The person prioritized amont checks stopping under relais and reducing unnecessary Claude context/quota use. They then challenged the exclusion of automatic output mediation, requested consent before loading successful CI output, and proposed an agent self-review opportunity before interrupting them. On 2026-11-09, shown the Phase 2 baseline, they chose no prompts at all: Phase 3 became autonomous limits on the largest outputs, with feedback to the agent only.

Accepted inspection `run-65f95f0243fa2-c037` reproduced SIGTTIN in the real amont install journey: `offer_agents_md -> trust::confirm -> read(/dev/tty)`. Three background-PTY runs stopped; two no-controlling-terminal controls passed. It used an older binary: current amont must be rebuilt and reproduced before a fix. The historical cancelled run's signal remains unknown.

Quiet capture already exists. The missing pieces are safe automatic routing, targeted CI-output policy, and compact producer results—not another excerpt implementation. The earlier output report failed review; its session byte breakdown is not an accepted measurement. Tool-result bytes also exclude important review-agent/repeated-context costs.

## Ownership and non-goals

- Relais owns verification launch context and its public status/notification/receipt interfaces. Amont-agent owns agent tool-call policy, one-off quiet capture and any safe invocation transformation. Amont retains declared-check capture and host-slot ownership.
- Reuse existing implementations and published contracts; no required sibling dependency or private sibling-ledger reads. Work without amont installed uses amont-agent's own quiet wrapper.
- Automatic mediation is now a candidate solution, **not a non-goal**. Prove host behavior before depending on it. PostToolUse cannot retract context already delivered.
- No paid summarizer, speculative generic output framework, new runtime dependency, hand-written JSON parser, new implementation language, blanket source-read restriction, lint suppression, skipped check, longer timeout or manual trust/permission change.
- No changes to required review panels/models or permission allowlists. Measure review overhead without silently disabling safeguards.
- Preserve the paused suppression rollout and its accepted worktrees/commits. This plan does not resume fleet adoption, runner publication or deployment; its original measurement date stays 2026-11-17.

## Execution method

After approval, refresh boards/remotes and coordinate with other sessions. Use task worktrees from `origin/main` without tracking main; refresh existing packs/generate guidance, and commit the approved plan/pointer before code. Route bounded implementation/measurement tasks through relais with narrow scopes and executable acceptance; do not edit a leased worktree. No implementation or new paid capability experiment during planning.

Land through reviewed PRs, explicit local checks and explicit CI conclusions. Phase 1 can ship independently; keep later PRs focused without multiplying one-line repair runs. Never reapply the delayed historical completion notifications from this conversation.

## Phase 1 — Fix verification terminal isolation first (relais)

1. Rebuild current amont's integration binary in an owned scratch checkout/build directory, not the live target. Repeat the focused real install-journey PTY experiment, with a 240-second cap after compilation; record revision, binary identity, stopping signal and descendant. If current HEAD does not reproduce, stop the proposed patch and report the discrepancy.
2. Add a red regression at the real `verify::run_attempt` seam. An owned helper establishes a controlling PTY using `setsid`/`TIOCSCTTY` and a background verification child; the old path must reproduce SIGTTIN on `/dev/tty`. Missing PTY support fails, never silently skips. Do not substitute a Python/sleep stand-in for the real-current-amont proof.
3. At **`run_attempt` only**, including setup using that launcher, set verification stdin to `Stdio::null()`. On Unix use `pre_exec` and existing `libc::setsid`, **not** `process_group(0)`. Assert `pgid == pid`; preserve leases, CPU sampling and group cleanup. A new session without closed stdin can still read inherited terminal input.
4. Leave ordinary `own_process_group` callers, workers/coordinator and generic `amont list`/`amont attest` probes unchanged. Windows process management remains unchanged; verification stdin is explicitly non-interactive. Launch errors remain visible failures. Amont's intentional interactive `/dev/tty` confirmation stays unchanged.
5. Verify real journey green three times; stdin EOF; exact exit/signal results; timeout/cancellation killing grandchildren; capture/log hashes; no survivors. Run the default relais profile and declared implementation reviews, release/install normally, and restart the coordinator only when idle. Branch binaries use scratch `RELAIS_STATE_DIR` only.
6. With the released fix active, run amont's **unchanged** default `make check` through native relais once. No profile, trust or timeout workaround. Record its terminal outcome, not raw passing output.
7. For seven days count verification attempts/timeouts from published receipts. Diagnose timeouts with bounded logs and contemporaneous owned-process state when available; otherwise label cause unknown. Record `pgid/session/tty` for the released launch path. No nonexistent stopped-signal counter or claim that zero attempts proves success.

Critical files: `relais/crates/relais/src/verify/mod.rs` (sole production seam: `run_attempt`), `procs.rs` (narrow helper if needed), owned PTY fixtures/tests, `docs/SPEC.md`, changelog/plan. `verify/mod.rs:3593` is a heartbeat test, not another production launcher.

## Phase 2 — Prove host capabilities and establish the baseline

Use a small owned-fixture experiment before broad implementation. Reuse existing hook/quiet tests and protocol handling; no extra summarizer or nested agent fan-out.

1. **Consent (done 2026-11-08, real-host fixture; kept as the record of what the host does, no longer a Phase 3 dependency):** verify actual native `Ask` behavior under the current host/permission mode. An unapproved successful-job output read must not execute. Test approve, deny, cancellation and non-interactive/unavailable consent. Separately prove a non-executing, explicitly machine-generated self-review feedback path that is distinguishable from a person's permission refusal. It must never teach retrying a human denial or rephrasing to bypass a guard. If that distinction is unsupported, report self-review unsupported and use native Ask directly; if Ask cannot be enforced, stop the consent slice. Never substitute automatic Allow or widen permissions.
2. **Mediation:** verify whether the real pre-tool interface supports invocation updates and how authorization evaluates the original and transformed commands. Test allowed and denied commands; never attach an Allow decision merely to make rewriting work. If safety cannot be demonstrated, do not enable automatic rewriting; retain explicit quiet recipes without claiming transparent interception.
3. Start with actual Bash and Read hook paths. Inventory CLI/API, wrappers, saved artifacts and MCP coverage. Unknown tools are explicit gaps, not claimed protection. No guarantee for arbitrary MCP results without a verified pre-execution adapter.
4. Produce a reproducible aggregate of the last 14 days: exact UTC window, session/call counts, deduplication, byte definition, category/coverage gaps. Include direct output, CI/log/receipt reads, agent launches and reported token/cache/model/duration data where available. Separate bytes, reported usage and inferred context duplication; none is automatically billed tokens or quota percentage.
5. Keep measurement in existing Rust tooling with already-declared JSON facilities and std counting; add synthetic tests. Print aggregates and bounded redacted examples only. Do not copy transcripts/secrets into git. Rank further optimization slices by observed avoidable volume; the person re-targeted Phase 3 on 2026-11-09 from these numbers.

Critical files: amont-agent's existing decision/payload/hook protocol, quiet integration fixtures, transcript-reading/measurement facilities and tests; `docs/quiet.md`. Exact contract paths are narrowed after inspecting these existing entry points, before launching each worker.

## Phase 3 — Autonomous limits where the bytes are (re-targeted 2026-11-09)

The Phase 2 baseline moved the target. Over [2026-10-23T10:03:38Z, 2026-11-06T10:03:38Z): main-conversation tool output 27.4 MB, subagent output 121.9 MB. CI logs are 0.5% of main output (plus 0.6% failed-job logs). Subagents' whole-file Reads over 8 KiB are 1,711 calls and 40.4 MB, about 27% of all tool output; 1,637 whole reads repeated a file already read whole in the same session. Measured reach, keyed per agent as below: of the 1,711, 941 calls (26.0 MB) are an agent's first whole read of that file, 765 (14.4 MB) a sibling subagent's first read of a file another agent read, and 5 (0.1 MB) the same agent again (left to `file-reread`). Replayed through the rule as built (2026-11-09), its exemptions cut that: 775 calls / 23 MB, of which 332 / 15.1 MB were review briefs that reviewers are told to read whole, false interventions. With reviewer agents exempt (below) the rule reaches 272 calls / 5.5 MB, about 4% of tool output, mostly workers reading source files. The large remaining bucket is the briefs themselves: about 137 KB per seat (code rules in full 50 KB, constraint index 34 KB, diff 33 KB, path decisions 17 KB), read whole by every seat of every round. Item 6 shrinks them. Main shell output is many small results (about 1 KB each), so capping it saves little. The person chose no prompts: every limit below is feedback to the agent, never a question to the person, and CI logs get no special path.

1. **Refuse once, then allow.** Add one shared behaviour to the existing rules: the first matching call in a session is refused before it runs, with factual machine feedback (proven on the real host 2026-11-08: the agent sees `PreToolUse:… hook error: <reason>` and continues); the same call repeated afterwards is allowed and journalled `allowed-after-feedback`. Key: session, agent (main or the subagent's id when the payload carries one), tool, normalised path and file size. Reuse the hook's `first_telling`/`lsp::told` once-per-key record (unrecorded answers first), not the `session_state.rs` read log; expiry or a lost record returns to "refuse once", never to a silent allow on an unseen file. No second model, no prompt.
2. **`read-unbounded-large`**: lower `LARGE` from 16 KiB to 8 KiB, keep its exemptions (plans read whole by reviewers), add one: an agent whose `agent_type` is a reviewer (`implementation-review`, `code-review-*`, `plan-review-*`, `interface-review`, relais reviewers) is handed its brief to read whole. Keep the stat-only check, and graduate it from `advise` to refuse-once. Feedback names the file's size and line count and the cheaper call: a window (`offset`/`limit`) or LSP `documentSymbol`.
3. **`whole-file-dump`**: graduate to refuse-once for a `cat` or address-less `sed` of a whole regular file over 8 KiB; feedback names the Read window.
4. **`file-reread`**: stays `advise`; its rate is measured in Phase 5 before any change.
5. No change to `persisted-output-dump`, CI rules, permissions or Claude Code settings. Lowering `bashOutputMaxChars` is the person's setting and out of scope.
6. **Profile briefs carry their own rules, not all of them** (the person's choice, 2026-11-09). A new decision replaces ADR-0043's `guidance.code-rules-in-dispatch` for code-review profile seats: implementation-review keeps every code rule in full, the decisions on the touched paths and the candidates (it already rules on every rule's candidates, so no rule loses its reader); a profile seat's brief carries the judge rules it owns in full, the decisions on the touched paths, and an id index of the other constraints (one line per id and title, replacing the 34 KB `aval rules --level constraint` text; size measured before release), fetched with `aval_rule`, or `aval rule &lt;id&gt;` where the MCP tool is absent, when its review needs one. The `&lt;&lt;&lt;RULES sha=` line stays the full code-rules hash; the new ADR states that a profile seat binds to it through implementation-review, and `interdiff_brief` and `code_review.rs` change to match. The ADR merges and `aval.pack` is refreshed before the amont-agent PR. `amont-agent review-brief --profile` changes to match. Expected, with the index ids and titles only (about 3 KB): a profile brief from about 137 KB to about 55 KB (137 − 50 code rules − 34 constraint text + 2 judge rules + 3 index); per round of one implementation review and four profiles, about 685 KB to about 360 KB. ADR-0061's per-profile diff slice is unchanged and not built here.

Critical files: amont-agent `crates/amont-agent-guard/src/rules/read_unbounded_large.rs`, `whole_file_dump.rs`, the hook's Read/Bash paths in `src/hook.rs`, `crates/amont-agent-process/src/review_brief.rs`, `tests/hook.rs`, `tests/review_brief.rs`, `docs/`; decisions: a new ADR replacing ADR-0043 for `guidance.code-rules-in-dispatch`, `HEADS.md`, `aval.pack`.

## Phase 4 — Capture automatically and reduce producer noise

1. If Phase 2 proves safe invocation updates, automatically route a **small explicit set of non-interactive checks** through existing quiet capture. Prefer existing declared `amont run` when applicable; otherwise reuse `amont-agent quiet`. Preserve host-slot scheduling through its published interface; scheduling alone is not capture.
2. Do not rewrite mutations, pushes, `amont land --json`, interactive/stdin-consuming calls, opaque shell parses or already captured output. Unsupported syntax stays unchanged with a documented gap. Never use quiet routing to get around a Phase 3 refusal.
3. Preserve one-line success, exact exit/signal/cancel behavior, actionable failure excerpts and full raw-log retrieval/permissions. Keep CI full-failure behavior unchanged. Do not fork quiet's excerpt engine or indiscriminately tail failures.
4. Tighten **relais-owned** completion/status formatting at existing public adapters: compact current outcome, actionable failure/decision, changed-file count and receipt/evidence path. Keep full criteria/reports/logs behind published detail paths rather than repeating complete contracts inside notifications. Add golden-text regression tests in `outcome.test.ts` for accepted, failed, blocked, cancelled and answered outcomes, using oversized-summary fixtures: fixture-specific byte ceilings must fail before compaction and pass after it; assert the receipt path when present, DECIDE_STEP when awaiting, FEEDBACK_STEP for accepted non-trials, and applicable NEXT_STEP[code] guidance. Ceilings are test assertions, never runtime truncation that could cut an instruction. Preserve decision, trust and feedback instructions when applicable, delivery/dedup semantics, and truthful current-state information. Do not suppress an unresolved decision as stale.
5. Publish compact receipt projections in `relais/docs/SPEC.md`, including candidate identity, criteria, check exits and **all** gaps/bypasses/downgrades. Never interpret absent/null/pending as green. Do not teach amont-agent relais's field schema. Publish the recipe before another tool points at it; until then use the bounded generic fallback.
6. Source-owned short recipes cover explicit quiet fallback, compact CI queries, targeted log retrieval and narrow LSP navigation. No new long always-on prompt, blanket 8 KiB rule or hand-edited installed skill. Extend generic artifact guards only if a measured, independently scoped need remains; that extension is not implicitly authorized here.

Critical files: existing amont-agent hook/payload/decision and shell-analysis/quiet integration tests; `docs/quiet.md`. Relais's `claude-plugin/hooks/runs.ts`, relevant public status adapter if needed, `claude-plugin/tests/outcome.test.ts`/`status.test.ts`, `docs/SPEC.md` and changelog. Use the already-existing TypeScript plugin; no new language is introduced.

## Phase 5 — Verify, land, activate and observe

- Replay the fixed baseline window through the Phase 3 matchers: count would-refuse calls by rule and context (main, subagent), review a reproducible sample of 50 plus every exempt-path near miss, and record false interventions. Run the rules' negative corpus.
- Hook tests: first whole Read over 8 KiB refused with size and window advice; the identical retry allowed and journalled; a different file refused once on its own; a ranged Read and an exempt plan path never refused; a subagent's call keyed apart from the main session's; expired or missing state refuses once again. The same for `whole-file-dump`.
- Automatic routing (Phase 4): deterministic noisy pass/fail fixtures; passing output <=200 B excluding the delayed notice; raw log intact; exit codes, signals, missing executable, cancellation and grandchild cleanup retained. The real-host proof (2026-11-08) showed the rewritten command is what is shown and run, so routing only ever makes output quieter, never changes the effect.
- Profile brief: `review-brief --profile backend` on a fixture corpus → its own judge rules in full, no `# Code rules (in full)`, an id index, the RULES line present, the size recorded; the same for `--interdiff`; the implementation-review brief byte-identical to before.
- Compact relais notifications: successful, failed, blocked, cancelled and answered fixtures; current action/state and every verification gap kept; complete evidence retrievable.
- Project default checks, declared implementation reviews (deltas via `review-brief --interdiff`), PR/CI/release/install through each owner workflow. No tool downgrades, review bypasses or manual trust edits.
- **Activation:** graduating the two rules to refuse-once is authorised by this plan once the replay sample shows 0 false interventions; any false intervention returns only that rule to `advise` until fixed.
- **Observe seven days** after activation (date recorded then): run `amont-agent output-audit` over those seven days and compare with the baseline's per-day rates. Targets: subagent whole-Read bytes over 8 KiB outside reviewer agents down by half (reach 5.5 MB in the baseline), and review-brief bytes per round down by about half (measured on the next five review rounds against the 137 KB per seat above), with no rise in failed tasks, repair rounds or reviewer-missed rule violations found later. Report `allowed-after-feedback` counts (feedback ignored) honestly; no quota percentage without measured evidence.

## Code review

- relais: backend, lang:rust, lang:typescript, unix, tui
- amont-agent: backend, lang:rust, unix, tui
- decisions: none — ADR text only, reviewed by this panel and aval's lint

Omissions: architect at code close — ownership and interfaces are reviewed in this plan; changes stay localized to existing process/policy/formatting seams. No React/UI profiles: hook feedback to the agent only, no prompt and no web UI. Language profiles cover only the named repository's languages; amont-agent has no TypeScript implementation work.

## Approval scope and current state

- This is the full updated proposal, **not implemented or approved**. Ask enforcement and safe command rewriting were proven on the real host on 2026-11-08; Phase 3 uses no prompt. No promise of arbitrary MCP interception.
- No active background agents or waiting relais runs at the start of this update (`waiting: 0`). Historical notifications were reconciled, not relaunched.
- Approval would authorize the above PR/release/local activation sequence and owned fixtures, not new trust grants, global permissions, unrelated cleanup or resumed suppression fleet publication.

## Implementation review

Phase 1 (relais#383), tree 63885f34. Round 1 then Delta, all Opus.
- implementation-review: approve-with-changes → approve. Fixed discards, stdout timing line, `&mut Command` params, rc=1 pin. Deliberate: unset real-command case returns green with a stderr "not run" line (78k, 74 s + 20 s).
- backend / lang:rust / unix: approve-with-changes → approve. Fixed pipe-fill, Debug-string match, ptsname race, unreaped helper, sid claim. Deliberate: amont probes unchanged (step 4); check-group kill left to run_observed's own timeout (47k/58k/51k round 1).
- lang:typescript, tui: approve (no TypeScript in the diff; optional "asked for a terminal" hint deferred).

## Phase 1 verification (observed)

| Input | Expected | Actual |
|---|---|---|
| Current amont f9ba814c install journey, old launcher shape under PTY | SIGTTIN | 3/3 SIGTTIN; controls 2/2 pass (relais run-65f97bc0db008-118a6 artifacts) |
| PTY regression, launcher reverted | red | both cases TimedOut (~10 s) |
| PTY regression, fixed launcher | green | 2/2 pass, 0.2 s |
| amont install journey (it binary sha256 6e073eb4…) through fixed launcher under PTY | 3/3 green | 3/3 pass, ~8.5 s each |
| Survivors / scratch | none | no processes; scratch worktree removed, logs kept |
| Default gate | green | 30/30 checks at b055a6e6 and 92aef136 |

Remaining Phase 1 steps: release/install, then amont's unchanged `make check` through native relais once; seven-day receipt observation from that date.

<!-- panel: repos=relais,amont-agent,decisions adds=lang:typescript reviewers=backend body-sha=8d8066826a6c -->
