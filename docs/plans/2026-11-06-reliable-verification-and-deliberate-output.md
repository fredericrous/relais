---
status: active
branch: fix/verification-terminal-isolation
repos: [relais, amont-agent, decisions]
adrs: [decisions:ADR-0039, decisions:ADR-0042, decisions:ADR-0043, decisions:ADR-0050, decisions:ADR-0053, decisions:ADR-0061, decisions:ADR-0065, decisions:ADR-0066]
---
# Reliable verification and deliberate output

## Review panel

👉 **Decide:** none — approve if relais prompts may index the rules a tool checks (candidate/exact-only) and the reviewer's process rules, fetched through the aval MCP and measured.
📍 relais, decisions · Phase 3 shipped (amont-agent 3.24–3.26, both rules at deny 11-09) · next: ADR-0066, then the relais Phase 4 PR. Delta panel: backend (opus).
**Changed by review:** reviewer reuses the worker's `aval_rule` instruction with `repo:`; relais journals index ids, `output-audit` counts fetches (no SPEC change); targets from the arithmetic (24%/12%); a stated rollback threshold.
**Verdicts:** backend approve-with-changes → approve-with-changes (a blocker its own patch caused, fixed) → approve. Nothing carried.
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

## Phase 4 — relais prompts carry the rules each agent judges (re-targeted 2026-11-09)

Measured over the Phase 2 window (2026-10-23..11-06): automatic quiet capture of checks would save at most 2.3 MB (check output is 3% of tool output) and relais plugin notifications total 1.0 MB, so the earlier Phase 4 (quiet routing, notification golden tests, receipt projections) is dropped as not worth its review cost. The largest remaining bytes are relais's own prompts: 248 semantic-reviewer prompts at 72 KB (17.8 MB) and 138 worker prompts at 68 KB (8.0 MB, coordinator messages included). A reviewer prompt holds 41 KB of code rules in full and 13.8 KB of statements of 34 process rules (`ci.*`, `work.*`, `toolchain.*`, `guidance.*`, `prose.*`); a worker prompt 41 KB of code rules and 11.5 KB of ranked guidance. Each code rule's body says how it is checked (`**Checked by:**`): judge-only 18 KB, mixed with a judge part about 14 KB, candidate-only about 9 KB, exact-only 0.4 KB. ADR-0043 put code rules in full in every dispatch because on-demand fetch through the `aval rule` CLI failed (0 of 194 workers); the aval MCP tool now exists and is called (since 2026-10-04: 54 `aval_rule` calls from main sessions, 30 from subagents), but no prompt has yet relied on it, so this phase measures whether it is used (item 4). The person chose this re-target, both halves, on 2026-11-09.

1. **A new decision first** (decisions repo, before the relais PR): it amends ADR-0043's `guidance.code-rules-in-dispatch` for relais dispatch as ADR-0065 did for profile seats. A code rule with a judge part in its `Checked by` line, or with no readable `Checked by` line, stays in full for writers and reviewers; a rule checked only by `candidate` or `exact` is one index line (id and statement) for the worker, and for the reviewer is given in full only when a candidate in this diff names it. Process rules are an id-and-statement index for the reviewer too, fetched with `mcp__aval__aval_rule`. Two or more later-found violations of an indexed rule in the seven days, against the baseline window's count, return that slice to full text.
2. **Reviewer** (`runner/review/mod.rs` `reviewer_guidance`, `engine.rs`): code rules split by checker as above, the candidate ids of this diff passed in from the candidates list (`PartCandidates`, already fetched before the prompt is built; when candidates are unavailable every candidate-only rule stays in full); process rules rendered as `- <id>: <statement cut at 120 chars>` under the instruction `constraint_index_block` already gives workers (`aval_rule` with `repo: "<guidance.repo_name>"`, else `aval rule <id>`), reused, not re-worded. The reviewer agent definitions (the 19 hand-kept `claude-plugin/agents/relais-reviewer-*.md`, `tools_of(AgentKind::Reviewer)` in `tests/it/plugin_agents.rs`, and the advisory `relais-review` in `install/mod.rs`) gain `mcp__aval__aval_rule`. Expected: 72 − (13.8 − 34 × ~155 B ≈ 5.3) − (9.4 − ~1) ≈ 55 KB per review (about 24%).
3. **Worker** (`runner/prompt.rs` `worker_code_rules_block`): judge rules in full, candidate- and exact-only rules as an index line each with the instruction to fetch with `aval_rule` (workers already have the tool). The ranked-guidance trim is the separate active plan `2026-10-27-guidance-budget-and-wait-row` and is not touched. Expected: 68 − (9.4 − ~1) ≈ 60 KB per worker prompt (about 12%).
4. **Measure it**: the relais journal (not the receipt, so no SPEC contract change) records, per run, the rule ids given as index lines; `amont-agent output-audit` counts the `aval_rule` calls of relais `agent_type`s from transcripts; the parse of `Checked by` is a pure function run while the rule body is present (a manifest read back from disk has none: today's gap stands), unit-tested over a fixture of every real `Checked by` line copied with its source sha; unknown wording defaults to full text.

Non-goals: no change to the binding key set, the RULES hash, the candidate checker, the guidance budget plan, or what a reviewer blocks on.

## Phase 5 — Verify, land, activate and observe

- Replay the fixed baseline window through the Phase 3 matchers: count would-refuse calls by rule and context (main, subagent), review a reproducible sample of 50 plus every exempt-path near miss, and record false interventions. Run the rules' negative corpus.
- Hook tests: first whole Read over 8 KiB refused with size and window advice; the identical retry allowed and journalled; a different file refused once on its own; a ranged Read and an exempt plan path never refused; a subagent's call keyed apart from the main session's; expired or missing state refuses once again. The same for `whole-file-dump`.
- relais prompts (Phase 4): a fixture corpus with judge-only, mixed, candidate-only, exact-only and unparseable rules → the reviewer prompt shows the judge rules in full, a candidate-only rule in full only when a listed candidate names it, the rest and the process rules as index lines; the worker prompt the same without the candidate case; every real rule file's `Checked by` line parses; prompt sizes recorded before and after on one real run.
- Profile brief: `review-brief --profile backend` on a fixture corpus → its own judge rules in full, no `# Code rules (in full)`, an id index, the RULES line present, the size recorded; the same for `--interdiff`; the implementation-review brief byte-identical to before.
- Project default checks, declared implementation reviews (deltas via `review-brief --interdiff`), PR/CI/release/install through each owner workflow. No tool downgrades, review bypasses or manual trust edits.
- **Activation:** graduating the two rules to refuse-once is authorised by this plan once the replay sample shows 0 false interventions; any false intervention returns only that rule to `advise` until fixed.
- **Observe seven days** after activation (date recorded then): run `amont-agent output-audit` over those seven days and compare with the baseline's per-day rates. Targets: subagent whole-Read bytes over 8 KiB outside reviewer agents down by half (reach 5.5 MB in the baseline), and review-brief bytes per round down by about half (measured on the next five review rounds against the 137 KB per seat above), with no rise in failed tasks, repair rounds or reviewer-missed rule violations found later. For Phase 4, over the seven days after its release: relais reviewer prompt bytes down by about 24% and worker prompt bytes by about 12% against the 72 KB and 68 KB above; the `aval_rule` calls per run with an indexed rule; and review findings or later fixes citing an indexed rule, compared with the baseline window. The Phase 4 item 1 threshold (two or more later-found violations of an indexed rule) returns that slice to full text. Report `allowed-after-feedback` counts (feedback ignored) honestly; no quota percentage without measured evidence.

## Code review

- relais: backend, lang:rust, lang:typescript, unix, tui
- amont-agent: backend, lang:rust, unix, tui
- decisions: none — ADR text only, reviewed by this panel and aval's lint

Omissions: architect at code close — ownership and interfaces are reviewed in this plan; changes stay localized to existing process/policy/formatting seams. No React/UI profiles: hook feedback to the agent only, no prompt and no web UI. Language profiles cover only the named repository's languages; amont-agent has no TypeScript implementation work. relais keeps lang:typescript because Phase 1's diff was reviewed with it; Phase 4 has no TypeScript.

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

<!-- panel: repos=relais,amont-agent,decisions adds=lang:typescript reviewers=backend body-sha=a7be636bc98a -->
