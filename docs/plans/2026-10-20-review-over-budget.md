---
status: active
branch: fix/review-over-budget
repos: [relais]
adrs: [ADR-0010 (relais), ADR-0040, ADR-0041]
---
# Plan: relais, a review that does not fit asks the orchestrator

## Review panel

👉 **Decide:** none. Approve if fixing the transport (prompt by path, no prompt text to the coordinator) plus a `review_over_budget` question with `review-parts` is worth one PR after the split's PR C, released as v0.28.0 with it.
📍 relais · reviewed, nothing built · next: after PR C merges, check reviewer file access and the coordinator's use of the prompt. Panel: backend (opus-5-5), codex (gpt).
**Changed by review:** the coordinator does read the prompt back (`ClaimOutcome::Spawn`, `Continue`); a successor column and manifest table with their migration; every part must fit, at most 8; the approval receipt is sealed from a saved verification snapshot, idempotently; part evidence immutable.
📄 Full reviews: [2026-10-20-review-over-budget.reviews.md](2026-10-20-review-over-budget.reviews.md)
**Verdicts:** backend approve-with-changes three times and approve once; codex approve-with-changes. **5 carried into implementation**, applied in the PR:
- Changes step 5's one migration adds the successor column and the parts manifest table (keyed by run, candidate hash and part index);
- the reseal model is `reseal_receipt_with_signoff`, defined in `cli/evidence.rs`, called from `cli/decide.rs`;
- Verification adds the version mismatch: a coordinator one version older refuses the prompt-less `Spawn`, relais restarts it, and the dispatch succeeds;
- Design 1 states the constraints budget in bytes, and the test asserts the exact count that fits;
- the per-part reviewer timeout is stated, with the worst case 8 × it.

## Context

Fourteen relais runs on 2026-10-19/20 ended `needs_review (review_unavailable)` with "the reviewer ended without a terminal result". Each reviewer dispatch ended `exit 1` about 0.26 s after it started, with no agent and no usage, so no review happened. Every one of those runs had a large diff (900–4,100 changed lines). Small diffs were reviewed normally.

The root cause is confirmed:
- `adapter/native.rs` `launch_native` registers each native dispatch with the coordinator (`register_native`, a `NativeAsk::Spawn { prompt, .. }` JSON line on the unix socket).
- The coordinator reads at most `MAX_REQUEST_BYTES = 64 * 1024` per request (`coordinator/mod.rs:62`, read near `:1280`).
- The reviewer prompt embeds the candidate's patch, up to `REVIEW_PATCH_BUDGET_BYTES` (192 KiB, `read_patch`). Above roughly 60 KB, registration is refused before anything is spawned.
- A 70 KB request sent to `~/.local/state/relais/relais.sock` on 2026-10-20 was answered `{"kind":"error","detail":"request exceeds 65536 bytes"}`.
- The refusal text is kept in `LaunchResult.failure_detail`, but the runner replaces it with the generic "ended without a terminal result" (`dispatch_reviewer`, near `runner/mod.rs:5744`). So the cause was invisible.

A second, smaller symptom has the same root. With the patch occupying the budget, the reviewer prompt kept 0 of the 84 constraint statements ("reviewer constraints partial (0 of 84 …)").

Where the prompt actually goes after registration:
- The coordinator keeps the prompt and sends it back in `ClaimOutcome::Spawn` (`admission/native.rs:436-453`). No client in the repository reads that field. `NativeAsk::Continue` carries `message: spec.prompt` (`adapter/native.rs:204-207`), which is under the same cap.
- The plugin receives the prompt as one `Request::Spawn` line on relais's stdout (16 MiB limit, `claude-plugin/hooks/lines.ts:5`) and spawns the subagent through the host API (`hooks/agents.ts:46-70`). The prompt never enters the orchestrator's context.
- The reviewer agents can read files (`tools: Read, Grep, Glob, LSP`).
- The patch already exists as `candidate-<n>.patch` in the run's artifacts.

The person asked (2026-10-20): "could you fix the budget issue with the 14 runs ended review_unavailable. worker should ask orchestrator if it should continue and orchestrator to decide". For a patch too large to review at once, they chose these answers: review in parts, accept on checks alone, revise, reject.

Intended outcome:
- A review that fits is always launched and is given its constraints.
- A launch failure says what failed.
- A patch too large for one review stops the run with a named question, and the orchestrator answers it. That answer can be a review in parts.

## Design

1. **The prompt names the patch; it does not carry it.** The reviewer prompt gives the patch's path and size, the changed files and the diff stat, and tells the reviewer to read the patch with `Read` in windows (`offset`/`limit`, 2,000 lines per call). A 192 KiB patch is about 49k tokens at 4 B/token, so it fits the reviewer's context. The 192 KiB budget then measures the patch file, the amount one reviewer reads, not prompt bytes. Constraints and guidance get their own budget again, so they are no longer starved by the patch.
   - Before relying on this, the plan's first step confirms that a reviewer subagent can `Read` a file in the run's artifacts directory, which lies outside the worktree it runs in.
   - If it cannot, the patch is copied into the worktree's ignored relais area (`.relais/review/<run>/`) and that path is given instead, removed when the review ends.
2. **The coordinator does not receive prompt text.** `NativeAsk::Spawn` registers the prompt's length and sha256 in place of the text, if the coordinator uses the text for nothing else. That is checked first in `RegisterNative` and `claim_native`, including `ClaimOutcome::Spawn`. `NativeAsk::Continue`'s `message` gets the same treatment.
   - If something does read it, the request cap is raised for that one request kind, to a bound derived from the prompt budget plus a margin, with a comment saying why.
   - In either case, an over-cap request is refused with a precise error. The protocol stays versioned: `serde(default)` lets a new coordinator read an old client's request. An old coordinator refuses a `Spawn` that has no `prompt`, so the coordinator restarts on a version mismatch, and a test covers that.
3. **A launch failure says why.** `dispatch_reviewer` (and the worker path, if it shares the generic text) puts `failure_detail` into the `review_unavailable` detail and into `events.jsonl`. For example: "the reviewer could not be registered: request exceeds 65536 bytes".
4. **A patch too large for one review asks the orchestrator.** When the candidate patch exceeds `REVIEW_PATCH_BUDGET_BYTES`, relais does not dispatch the reviewer at all. The run ends `needs_decision (review_over_budget)`. The detail names the patch size, the budget, the number of files and how many parts a review in parts would take. `relais decide` answers:
   - `review-parts` (new answer, the person's choice): the run's candidate is reviewed in parts. The patch is split at file boundaries into consecutive parts that each fit the budget, with no oversized-file exception. If any single file's patch exceeds the budget, `review-parts` is not offered and a direct answer is refused with a named explanation; approve, revise and reject remain available. The decision handler validates this restriction before recording a successor intent. There are at most 8 parts, so the review takes at most 8 reviewer timeouts. Above 8 parts, `review-parts` is not offered. Each part goes to its own reviewer dispatch, sequentially, with the same prompt and the part's index ("part 2 of 5"). Every part must pass, and any finding from any part counts as it would in a single review. Persist a candidate-bound parts manifest with each part's index, patch path and hash, dispatch id, result path and hash, and status. Each part uses distinct immutable patch and result artifacts; sequential reviews must not reuse `review.txt`. Persist completed results before dispatching the next part. The manifest is a ledger table keyed by run, candidate hash and part index, added in step 5's migration. Part artifacts are `review-parts/<index>.patch` and `review-parts/<index>.txt`. A later `review-parts` answer creates a new successor that reviews every part again; results are not reused across runs. The receipt lists every part and its verdict; a failed or interrupted run retains the manifest, completed evidence, and explicit unavailable or not-run statuses for unfinished parts.
     - Mechanically, this answer records a successor intent: `kind: verify` of the same candidate, `review: parts`, reusing the successor-intent machinery (`ledger/successor.rs`, `runner/successor.rs`). So it re-verifies and then reviews, and no new resume path is needed. The re-verification costs one profile run; that is accepted, and the reason is written in the Decision log.
   - `approve`: accept on checks alone, retaining the existing verification-gap guards. Before a new run stops for `review_over_budget` or `review_unavailable`, persist the verification snapshot needed to seal a receipt, bound to the candidate, base, contract and policy hashes. The approval handler explicitly creates and seals the receipt from that evidence; ordinary decision resolution currently creates no receipt. Record `review: not_run (over_budget)` for an over-budget stop, or an unavailable review with its actual failure detail for `review_unavailable`, including patch size and any attempted dispatch. Publication order: write the receipt artifact to a content-addressed path, then record the receipt row and resolve the decision in one ledger transaction. A retry that finds the decision resolved with the same receipt hash succeeds and writes nothing. An artifact with no row is rewritten with the same bytes. The existing reseal path (`reseal_receipt_with_signoff`, `cli/decide.rs:357`) is the model. Missing evidence produces a named error rather than fabricated verification. This does not retroactively fabricate receipts for the 14 already answered runs.
   - `revise`: as today, a successor contract, to split the task.
   - `reject`: as today.
   - `review-parts` and `approve` also answer a run that is `needs_review (review_unavailable)`, for example a reviewer whose launch failed. On a patch within the budget, `review-parts` is a single part, which retries the review.
5. **Interface.** `review-parts` is a new value of `relais decide`'s answer, of the MCP `decide` tool and of the plugin's decide path. Under `cli.compat.interface-is-public`, a new answer value is a minor version. Nothing is removed or renamed, and `review_over_budget` is a new `Reason`. SPEC §9 (decisions) and §10 (review) say all of this, and the CHANGELOG has a line.

**Sequencing.** These files are being moved by the module split's PR C (`runner/mod.rs` into phase modules, plan `2026-10-19-finish-admission-then-split.md`). This work starts after PR C merges, on the new layout, as one PR. It then rides the split's release, which becomes v0.28.0 (minor, because of the new answer) instead of a patch release.

## Changes

1. After PR C merges: create a branch from `origin/main`. Land this plan as its first commit.
2. Check the reviewer's file access from the artifacts directory, either with a live reviewer dispatch whose prompt names a file there, or by reading Claude Code's subagent permission rules. Then check whether the coordinator reads the prompt text. Record both answers in the Decision log; they choose between the alternatives in Design 1 and 2.
3. Change the transport and the prompt (Design 1–3). This touches `runner/review/*` (after PR C), `adapter/native.rs`, `coordinator/mod.rs` and `protocol` types if `NativeAsk` changes. It goes through relais in one bounded run, with the module-cycle and move checks not applicable (it is not a move). The installed relais predates this fix, so a diff above about 60 KB ends `review_unavailable`. Keep the diff below 60 KB, or have the orchestrator review `candidate-<n>.patch` itself before answering `approve`.
4. Add over-budget detection and `review_over_budget` (Design 4, without parts): in `runner/machine.rs` (the new observation, then decide), the review phase, `lifecycle.rs` `Reason`, the receipt's `review` field for `not_run (over_budget)` and for an unavailable review with its detail, the verification snapshot saved before either stop (a ledger table and its migration), and the `approve` handler in `cli/decide.rs` that seals a receipt from that snapshot.
5. Add `review-parts`: the decide answer (`cli/decide.rs`, the clap enum, the MCP tool schema in `claude-plugin/`), a successor intent with `review: parts` (a new ledger column and its migration; an older binary refuses such an intent with a named error), the parts review in the review module, and the receipt listing the parts.
6. Write SPEC §9 and §10 and the CHANGELOG. Then run the implementation review, merge, and release v0.28.0 with the split.

## Verification

- **The launch no longer fails at the cap.** A test registers a native dispatch whose prompt is larger than 64 KiB: it is accepted, or registers by length and sha256, and the plugin's spawn line still carries the whole prompt. A test sends a deliberately over-cap request to the coordinator and checks that the error names the cap.
- **Failure text surfaces.** A reviewer dispatch whose registration is refused yields `review_unavailable` with the refusal text in the run's detail and in `events.jsonl`. A test checks that the old generic sentence appears only when no detail exists.
- **Constraints fit again.** On a run whose patch is about 150 KB (below the 192 KiB review budget) (constructed in a test), the reviewer prompt holds all constraint statements that fit their own budget, and no "0 of N" message appears.
- **Over budget asks.** A candidate patch larger than the budget never dispatches a reviewer. The run ends `needs_decision (review_over_budget)`, and its detail names the size, the budget and the part count.
- **Each answer:**
  - `approve`: exercise both over-budget and unavailable-review decisions. After approval, reopen the ledger and receipt artifact and verify matching candidate identity, verification evidence, review reason and receipt hash. Inject receipt-publication failure, retry the answer, and verify consistent recovery without duplicate decisions or fabricated evidence; existing verification gaps still prevent approval.
  - `reject`: rejected.
  - `revise`: a successor with the given contract.
  - `review-parts`: a `kind: verify` successor. A test with a mocked reviewer checks the parts are cut at file boundaries, each within the budget without exception, dispatched in order, all-pass required. An oversized single-file fixture checks that `review-parts` is unavailable and that a direct CLI answer is refused without creating an intent or dispatch. One failing part makes the review fail with that part's findings. Tests reopen every recorded result artifact and verify its ledger hash after multiple parts, then repeat with a later dispatch failure to prove earlier evidence survives. Successful receipts list every part; unsuccessful runs retain the manifest and explicit statuses for all parts.
- **Live, on this machine, after v0.28.0 is installed:**
  - A relais run whose patch is about 100 KB (larger than the old cap, smaller than the budget) is reviewed. Before the fix, the same run was the `review_unavailable` case.
  - A run that is over budget asks, and answering `review-parts` completes the review in parts.
  - The 14 `review_unavailable` runs from 2026-10-19/20 were each answered `approve` by the orchestrator after their checks and a move check (plan `2026-10-19-finish-admission-then-split.md`); `relais report` shows none of them still waiting.
- **Observability:** `relais report` counts `review_over_budget` runs and reviewer launch failures by cause. Input: a run that is over budget. Expected: the count goes up by 1.
- **Gates:** `make check` exits 0, and CI is green on all three platforms.

<!-- panel: repos=relais reviewers=backend,codex body-sha=a19380c861ef -->
