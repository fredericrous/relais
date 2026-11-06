---
status: active
branch: fix/verification-terminal-isolation
repos: [relais, amont-agent]
adrs: [decisions:ADR-0039, decisions:ADR-0042, decisions:ADR-0050, decisions:ADR-0053]
---
# Reliable verification and deliberate output

## Review panel

Approved 2026-11-06; proposal-time body wording is preserved to retain the review binding.
📍 relais, amont-agent · active · next: current-source reproduction. Panel: backend (Opus), TypeScript (Opus).
**Changed by review:** bounded metadata; terminal denial state; golden notification tests.
**Verdicts:** two approve-with-changes; high findings resolved; seven implementation carries in the sidecar.
📄 Full reviews: [review evidence and implementation carries](2026-11-06-reliable-verification-and-deliberate-output.reviews.md).

## Context

The person prioritized amont checks stopping under relais and reducing unnecessary Claude context/quota use. They then challenged the exclusion of automatic output mediation, requested consent before loading successful CI output, and proposed an agent self-review opportunity before interrupting them.

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

1. **Consent:** verify actual native `Ask` behavior under the current host/permission mode. An unapproved successful-job output read must not execute. Test approve, deny, cancellation and non-interactive/unavailable consent. Separately prove a non-executing, explicitly machine-generated self-review feedback path that is distinguishable from a person's permission refusal. It must never teach retrying a human denial or rephrasing to bypass a guard. If that distinction is unsupported, report self-review unsupported and use native Ask directly; if Ask cannot be enforced, stop the consent slice. Never substitute automatic Allow or widen permissions.
2. **Mediation:** verify whether the real pre-tool interface supports invocation updates and how authorization evaluates the original and transformed commands. Test allowed and denied commands; never attach an Allow decision merely to make rewriting work. If safety cannot be demonstrated, do not enable automatic rewriting; retain explicit quiet recipes without claiming transparent interception.
3. Start with actual Bash and Read hook paths. Inventory CLI/API, wrappers, saved artifacts and MCP coverage. Unknown tools are explicit gaps, not claimed protection. No guarantee for arbitrary MCP results without a verified pre-execution adapter.
4. Produce a reproducible aggregate of the last 14 days: exact UTC window, session/call counts, deduplication, byte definition, category/coverage gaps. Include direct output, CI/log/receipt reads, agent launches and reported token/cache/model/duration data where available. Separate bytes, reported usage and inferred context duplication; none is automatically billed tokens or quota percentage.
5. Keep measurement in existing Rust tooling with already-declared JSON facilities and std counting; add synthetic tests. Print aggregates and bounded redacted examples only. Do not copy transcripts/secrets into git. Rank further optimization slices by observed avoidable volume; the explicitly requested CI-consent policy is not vetoed by an arbitrary byte-share cutoff.

Critical files: amont-agent's existing decision/payload/hook protocol, quiet integration fixtures, transcript-reading/measurement facilities and tests; `docs/quiet.md`. Exact contract paths are narrowed after inspecting these existing entry points, before launching each worker.

## Phase 3 — Self-review first, then consent for successful CI output

This is one targeted pre-tool policy, not a blanket block on CI information.

| Retrieval | Policy |
|---|---|
| Status, conclusion, SHA and compact list of all job outcomes | Allow |
| Failed-job diagnostics | Allow with existing bounded-output guidance |
| Successful-job logs or verbose successful-step payloads | Self-review challenge, then native consent on retry |
| Whole-run output including successful jobs | Same challenge/consent; suggest failed-job-only retrieval |
| Verbose CI retrieval with unknown outcome | Compact metadata lookup first; if still unknown, challenge/consent |

1. Resolve job identity/outcome **without fetching the verbose output**. Use existing bounded process helpers for at most one compact metadata query per evaluation, only after matching a CI-retrieval command, with a 1.5-second deadline (timeout stays unknown). Measure unmatched-call hook p95 against the baseline and require no regression beyond baseline noise; cache only narrowly identified outcomes. Auth/network/parse failure stays unknown. Start with verified GitHub `gh` CLI/API patterns; enumerate unsupported Forgejo/raw HTTP/MCP paths explicitly rather than claiming universal coverage.
2. **Fresh episode:** refuse execution once with a short, factual self-review message: the job succeeded, the compact result is available, do you actually need its full output? Suggest the compact alternative. This is feedback to the existing agent, **not a separate LLM call**.
3. **Retry after challenge:** if the agent still requests that retrieval, emit native `Ask`, identifying the job/attempt, outcome, scope and the agent's supplied purpose when available. The agent can explain the reason in its next message/tool description; no second model judges whether the reason is good. The person decides.
4. **Consent boundary:** a retry is not approval. Native approval authorizes the single requested tool call; no persistent allowlist, self-approved token or blanket CI waiver. Denied, cancelled or unavailable consent does not run the retrieval and closes the episode until expiry: matching requests, including equivalent forms, do not prompt again. A recorded human denial is labeled as such; unavailable consent is labeled unavailable, never falsely attributed to the person. Expiry never grants consent or authorizes an agent to retry a human refusal; a new explicit human request can begin a new episode. There is at most one machine self-review challenge before Ask for an episode—not an endless deny/retry loop.
5. Keep bounded session state keyed by repository/host, run/job identity, attempt and retrieval scope—not exact shell spelling. Changing `gh` to a known equivalent API path or wrapping in quiet/redirection must not reset or bypass the challenge. Whole-run permission is not inferred from one job's permission. Eviction/expiry returns to the conservative initial state, never Allow.
6. Gate **before fetching/downloading or opening known saved successful-CI output**. Quiet capture is not consent. Saved-log coverage requires known provenance; ordinary source files must not become CI logs by filename resemblance. No private sibling-path or product-specific filename classifier.
7. Keep consent output concise and journal only safe identities/decisions, not tokens, full URLs with secrets or logs. Existing pipeline/queue rules keep their own purpose; compose decisions deterministically without losing their protections or emitting duplicate warnings. Document precedence and test combined cases.

Critical files: `amont-agent/crates/amont-agent-guard/src/decision.rs`, existing session-state/payload/rule infrastructure, a focused CI-output rule, registry, `src/hook.rs`, existing rule corpus/`tests/hook.rs`, owner docs. No new CLI is required just to carry a justification.

## Phase 4 — Capture automatically and reduce producer noise

1. If Phase 2 proves safe invocation updates, automatically route a **small explicit set of non-interactive checks** through existing quiet capture. Prefer existing declared `amont run` when applicable; otherwise reuse `amont-agent quiet`. Preserve host-slot scheduling through its published interface; scheduling alone is not capture.
2. Do not rewrite mutations, pushes, `amont land --json`, interactive/stdin-consuming calls, opaque shell parses or already captured output. Unsupported syntax stays unchanged with a documented gap. Never use quiet routing to bypass Phase 3's CI consent policy.
3. Preserve one-line success, exact exit/signal/cancel behavior, actionable failure excerpts and full raw-log retrieval/permissions. Keep CI full-failure behavior unchanged. Do not fork quiet's excerpt engine or indiscriminately tail failures.
4. Tighten **relais-owned** completion/status formatting at existing public adapters: compact current outcome, actionable failure/decision, changed-file count and receipt/evidence path. Keep full criteria/reports/logs behind published detail paths rather than repeating complete contracts inside notifications. Add golden-text regression tests in `outcome.test.ts` for accepted, failed, blocked, cancelled and answered outcomes, using oversized-summary fixtures: fixture-specific byte ceilings must fail before compaction and pass after it; assert the receipt path when present, DECIDE_STEP when awaiting, FEEDBACK_STEP for accepted non-trials, and applicable NEXT_STEP[code] guidance. Ceilings are test assertions, never runtime truncation that could cut an instruction. Preserve decision, trust and feedback instructions when applicable, delivery/dedup semantics, and truthful current-state information. Do not suppress an unresolved decision as stale.
5. Publish compact receipt projections in `relais/docs/SPEC.md`, including candidate identity, criteria, check exits and **all** gaps/bypasses/downgrades. Never interpret absent/null/pending as green. Do not teach amont-agent relais's field schema. Publish the recipe before another tool points at it; until then use the bounded generic fallback.
6. Source-owned short recipes cover explicit quiet fallback, compact CI queries, targeted log retrieval and narrow LSP navigation. No new long always-on prompt, blanket 8 KiB rule or hand-edited installed skill. Extend generic artifact guards only if a measured, independently scoped need remains; that extension is not implicitly authorized here.

Critical files: existing amont-agent hook/payload/decision and shell-analysis/quiet integration tests; `docs/quiet.md`. Relais's `claude-plugin/hooks/runs.ts`, relevant public status adapter if needed, `claude-plugin/tests/outcome.test.ts`/`status.test.ts`, `docs/SPEC.md` and changelog. Use the already-existing TypeScript plugin; no new language is introduced.

## Phase 5 — Verify, land, activate and observe

- Replay the fixed baseline window through the proposed CI classifier. Review all flagged cases up to 50, otherwise a reproducible sample of 50 plus every distinct form. Record actual coverage and false interventions; zero sampled cases is not proof of accuracy. Run the complete negative corpus.
- Exercise the real host against owned fixtures: self-review abandonment, retry -> Ask, denied consent -> no fetch/read, approved call -> one retrieval, two jobs, rerun attempts, equivalent command forms, redirected/quiet-wrapped downloads, known saved logs, unknown metadata, stale/expired state and unsupported MCP paths.
- Test automatic routing using deterministic noisy pass/fail fixtures: record whole-episode bytes, including guard feedback/retries; pass <=200 B excluding the existing delayed notice; raw log intact; exit 3, signals, missing executable, cancellation and grandchild cleanup retained. Required diagnostics may exceed a cosmetic byte budget.
- Test compact successful/failed/blocked/cancelled/answered relais notifications. Preserve current action/state and every verification gap, avoid repeated full-contract text, and keep complete evidence retrievable. Do not change accepted/failed semantics to reduce bytes.
- Run project default checks and declared implementation reviews, then PR/CI/release/install through each owner workflow. No tool downgrades, review bypasses or manual trust edits. No retry of dismissed aval trust. Person-only plugin refresh remains a reported boundary.
- **Conditional local activation:** consent policy activates only after the real Ask/no-execution tests and negative corpus pass. Automatic routing activates only for proven safe forms. Approval of this plan does not itself approve reading any successful CI log. Any false intervention or authorization regression disables only the affected new behavior, preserving prior protections; fix before re-enabling.
- Observe seven days after actual activation; record the exact due date then. Count self-review challenges with/without retries (no mind-reading claim), consent prompts/outcomes, false interventions, routed calls, whole-episode bytes and available agent usage. Include challenge/metadata/retry overhead. Target >=50% fewer avoidable bytes for optimized fixture episodes with no lost evidence; poor real-world benefit holds expansion or disables the affected optimization, **not** the explicitly requested consent policy merely for low traffic.
- Report before/after workload and data limitations. No quota-saving percentage without actual evidence. Retain mandatory reviews while using concise shared briefs and no redundant exploration, repair runs or raw successful output dumps.

## Code review

- relais: backend, lang:rust, lang:typescript, unix, tui
- amont-agent: backend, lang:rust, unix, tui

Omissions: architect at code close — ownership and interfaces are reviewed in this plan; changes stay localized to existing process/policy/formatting seams. No React/UI profiles: native permission prompts, not a new web UI. Language profiles cover only the named repository's languages; amont-agent has no TypeScript implementation work.

## Approval scope and current state

- This is the full updated proposal, **not implemented or approved**. Automatic capture and Ask behavior are explicitly unproven until Phase 2; no promise of arbitrary MCP interception.
- No active background agents or waiting relais runs at the start of this update (`waiting: 0`). Historical notifications were reconciled, not relaunched.
- Approval would authorize the above PR/release/local activation sequence and owned fixtures, not new trust grants, global permissions, unrelated cleanup or resumed suppression fleet publication.

<!-- panel: repos=relais,amont-agent adds=lang:typescript reviewers=backend,lang:typescript body-sha=745e5882e9b0 -->
