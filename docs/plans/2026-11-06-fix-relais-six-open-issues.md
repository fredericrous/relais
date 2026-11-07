---
status: active
branch: fix/open-issues-2026-11
repos: [relais]
adrs: [ADR-0007, ADR-0009, decisions:ADR-0043]
---
# Fix relais's six open issues on one branch

## Review panel
👉 **Decide:** none: approve if one relais branch (six commits, one PR, v0.49.0) is right, with #369 fencing the tree until a stop is proven.
📍 relais · plan reviewed · next: six relais change runs in `relais-wt-open-issues`, one commit each. Panel: backend (opus-5-5), lang:rust (opus-5-5), lang:typescript (opus-5-5), codex.
**Changed by review:** #369 moved from the coordinator lease to the persisted in-place marker, and time never clears it; #347 rejects bare acknowledgements (`/usr/bin/Yes` exists on macOS); #351 follows ADR-0043, the worker gets the reviewer's rules.
📄 Full reviews: [2026-11-06-fix-relais-six-open-issues.reviews.md](2026-11-06-fix-relais-six-open-issues.reviews.md)
**Verdicts:** round 1: 2 rework, 1 approve-with-changes; round 2: 2 approve-with-changes; codex rework (4 accepted); backend final approve. 12 findings carried into implementation.

## Context
The person asked for every open relais issue to be fixed (2026-11-06), after
pointing out that the amont programme opened nine PRs where one branch with a
commit per issue was wanted. So this is **one branch, one commit per issue,
one PR, one release**. Each issue is implemented as a relais `change` run in
the task worktree `relais-wt-open-issues` (in place). relais verifies each
run with the repository's own checks, offloading heavy runs to the homelab
container where relais is set up to. Each accepted candidate is then
committed by hand as one commit. Locations come from the read-only inspect
run run-65fa0f294081d-9385.

## Goal
relais #382, #377, #369, #352, #351 and #347 are closed by commits on
`fix/open-issues-2026-11`. That branch merges as one PR with a merge commit,
so the six commits stay visible, and it ships in one release (v0.50.0,
minor: #369 and #347 change behaviour).

## Non-goals
- Not improving `relais init --detect` to find CI-declared checks such as
  `scripts/check.sh` (#347 asks only that a free-text answer never becomes a
  check).
- Not re-asking trust when commands change: ADR-0007 (`trust.scope`) keeps
  trust once per repository. #347's "already allowed" half only gets clearer
  output.
- Not touching PR #328 (`test/claude-probe-budget`): it changes
  `adapter/claude.rs` only, with no overlap.

## Code review
- relais: backend, lang:rust, lang:typescript, unix, architect (tui left out: one refusal line changes, no new output surface; architect kept: #369 adds a coordinator lease lifecycle and #351 moves guidance resolution into preflight)

## Behaviour, per commit
1. **#382: a setup queue that runs out ends blocked.**
   - `runner/engine.rs:712-724` (task-tree setup) gains the
     `Err(VerifyError::Lease(stop))` arm that `runner/baseline.rs:142-153`
     already has: it calls `end_on_lease_stop` (`engine.rs:878`) and retires
     the worktree as `engine.rs:727` does. The run ends
     `blocked (admission_wait_exhausted)`, exit 3.
   - Abandon half (`cli/decide.rs:188-227`): an `abandon` of an interrupted
     in-place run whose HEAD did not move AND whose working tree equals its
     start snapshot `S`, untracked non-ignored files counting as edits (nothing kept to lose) retires the marker, as the
     moved-HEAD case does. With edits present, the marker stays and the
     answer names `relais resume --retire <run>`, as today.
   - `docs/SPEC.md:347` gains that one clause.
2. **#377: the warmed fake is published once.**
   - `tests/it/fake_bin/mod.rs:56-67`: the check-then-rename publish becomes
     `std::fs::hard_link(aside, shared)`. `AlreadyExists` means another
     process published first: use theirs. The aside file is removed in both cases. Two processes can
     no longer replace the shared inode.
3. **#369: an in-place worktree stays fenced until a lost worker is gone.**
   - Today `adapter/native.rs:736-749` ends the attempt `mod_gone` with no
     stop sent, and the lease goes back: `runner/launch.rs:316` releases it,
     and settling the dispatch drops it anyway (`forget_settled`,
     `admission/mod.rs:2411-2424`). The coordinator holds leases in memory
     only and may idle-exit (`:1114-1118`), so the fence has to be persisted.
   - Scope: in-place runs. A fork's or isolated run's worker writes only in
     that run's own task tree, and no other run is admitted to it. Only an
     in-place tree is handed to the next run, which is the reported case.
   - On a `mod_gone` end of an in-place run, the marker
     `<git-dir>/relais-inplace/<run>.json` (it already outlives a crash)
     records `orphan: {dispatch, session, agent_id or none, since, escalate_at}`.
     `escalate_at` is two hours after `since`, an operational escalation
     threshold only. Neither a run budget nor elapsed time proves that a
     disconnected native worker has stopped.
   - The attempt keeps no candidate and is not verified, so no snapshot is
     taken of a tree a lost worker may still be writing. The own-lease bypass
     in `wait_for_writers` (`runner/evidence.rs:357`) never applies. The run
     ends `interrupted`, as today.
   - The orphan clears on either of:
     - the coordinator answers that `(session, agent_id)` stopped: a new
       `agent_stopped` query reads what `settle_by_agent` already remembers
       for an agent that ended with nothing bound (`remember_stopped`,
       `:2193`); this is the plugin's SubagentStop, the proof the worker is
       gone. The runner reports the orphan (session, agent_id, marker path)
       to the coordinator on `mod_gone`; the coordinator journals it across
       restarts and itself writes `yes` to the marker on a matching
       SubagentStop, whether `settle_by_agent` returns `Native` or
       `NothingBound`. An orphan with no agent_id clears only on
       `--worker-stopped`;
     - the person runs `relais resume --retire <run> --worker-stopped` after
       closing that Claude Code session; their word, recorded on the run.
   - Elapsed time never clears the orphan. At `escalate_at`, status and
     refusal output explain that stop confirmation or explicit attestation
     is still required.
   - While it stands, `relais resume --retire`, `relais decide abandon` and a
     new in-place run in that tree are refused `inplace_worker_may_run`. The
     refusal names the dispatch, its age and what frees it, and
     `relais status` shows the same.
4. **#352: a successor is reviewed on its own changes.**
   - A run with `from_candidate` uses successor-only review only when
     successful review evidence covers the exact inherited tree under the
     applicable rules. Missing, failed or unavailable review, and a merge
     that changes that tree, require review of the full patch against the
     base. For an eligible predecessor, derive the successor patch with
     `workspace::derive_patch` (`workspace/mod.rs:1471`) from the inherited
     tree. In either case, distinguish inherited files from successor edits
     when judging the successor's scope. Review budget checks use the patch
     actually selected for review.
   - The review brief lists the files the predecessor changed, marked
     inherited, so "Nothing else changes" is judged against the predecessor.
     The full patch against the base is still what is verified and kept
     (SPEC §… `target_base_sha` unchanged).
5. **#351: the worker is given every rule its reviewer applies** (decisions
   ADR-0043, `guidance.code-rules-in-dispatch`: a dispatch that writes or
   reviews code carries the full text of every active code rule, and the
   reviewer reports a violation as blocking).
   - Worker (`runner/prompt.rs:340-358`) and reviewer
     (`runner/review/mod.rs:506-584`) are rendered from one `Guidance`
     snapshot, resolved once in preflight (`runner/preflight.rs:910`).
   - That snapshot is resolved from the same source the reviewer reads.
     When the repository has no corpus of its own (aval exit 3), the code
     rules come from the fleet `decisions` pack, and the gap is recorded only
     when neither has them. The reviewer's live `aval_resolver` call
     (`review/engine.rs:532`) reads the snapshot instead. Snapshot the full
     active rule corpus before dispatch, including the rules adr-lint can
     match, and give those texts to both worker and reviewer. Keep adr-lint's
     candidate-dependent matching (`review/engine.rs:406-413`) after the
     candidate exists; its hits reference that rule snapshot. Do not attempt
     to precompute hits for a future candidate.
   - The reviewer keeps blocking, as ADR-0043 says. The inspect run could
     not confirm whether the rules in the issue came from aval or adr-lint;
     both now reach the worker.
6. **#347: a typed answer must name something runnable.**
   - `relais init --detect --command <text>` (`repo/detect.rs`) refuses a
     command whose program is not a file in the repository, not
     `node_modules/.bin/<name>`, and not on the local `PATH`, the way it already refuses shell operators. The check reads the local `PATH` only, and the refusal text says so (an offloaded run's container may differ). Before executable resolution, reject a bare acknowledgement (`yes`, `y`, `ok`, `okay`, `no`, `n`, case-insensitively) with a specific acknowledgement diagnostic. An explicitly named path remains available for an intentionally selected executable. This rejects `Yes` even when `/usr/bin/Yes` resolves on a case-insensitive filesystem. `askCommand` (`claude-plugin/hooks/consent.ts:457`) asks
     once more with the reason. The `command_refused` decline (`consent.ts:415`) carries the refusal reason itself instead of the shell-operator hint. Nothing that fails is ever written.
   - `consent.ts:264`'s and `consent.ts:439`'s (onboard, already granted) answers also print the commands
     that changed since the grant, read from `pending_change` (`trust.rs:453`, added to the TS `Shown` type), rendered through `rows()`; the Rust ledger records that it was reported, so it prints once, without
     re-asking (ADR-0007).
7. **Release commit:** `## Unreleased` → `## v0.49.0` in CHANGELOG, version
   bump. Merged with the PR, tagged with `amont land --tag v0.50.0`.

## Phases
- [x] Phase 1: six relais `change` runs, one per issue, in place in the task worktree. Each accepted candidate is committed as its own commit, subject ≤ 50 chars.
- [ ] Phase 2: implementation review (the Code review profiles above) on the branch diff, fixes, then push, one PR, merge-when-green with a merge commit.
- [ ] Phase 3: release v0.50.0 (v0.49.0 was released meanwhile by another session), tag, verify the artefacts, install relais and reinstall the plugin (the person's step), then the live checks.

## Rollback
Revert the merge commit and release a patch. Each issue is one commit, so
one fix can be reverted alone.

## Decision log
- 2026-11-06: one branch and one PR for all six (the person: "several commits with only one PR").
- 2026-11-06: #382 abandon retires the marker only when nothing is kept (HEAD unmoved and tree equal to `S`); the spec keeps the marker when edits exist.
- 2026-11-06: #369 fences the in-place tree through its persisted marker, not the coordinator's lease. The lease lives in memory, is dropped on settle and lost on idle-exit (review round 1). The orphan clears only on persisted confirmation of the matching agent's SubagentStop or on the person's `--worker-stopped`. Two hours triggers escalation, never automatic clearance.
- 2026-11-06: #352 reviews the successor's own diff when successful review evidence covers the exact inherited tree; otherwise it reviews the full patch. Both paths distinguish inherited changes from successor scope.
- 2026-11-06: #351 gives the worker the reviewer's rules (from the fleet pack when the repository has no corpus), not an advisory downgrade: ADR-0043 keeps the reviewer blocking.
- 2026-11-06: #347 validates the program exists, at `relais init --detect --command`, so the CLI and the plugin share one rule. A program is found when it is a file in the repository, `node_modules/.bin/<name>`, or on the local `PATH` (a setup-installed tool is not refused); the refusal says the check is local.

- 2026-11-07: #351's fleet rules come from the decisions pack relais vendors (`.adr/packs/decisions.pack`), embedded at build time and materialised once under the state directory. Cost: a rule change reaches workers only through a relais release; `RELAIS_FLEET_DECISIONS` overrides it (architect review).
- 2026-11-07: an orphan that cannot be written at all (the git dir is unwritable) leaves only the plain interrupted marker and a warning; the next run is still stopped by that marker. Kept as is (implementation-review, low).
- 2026-11-07: deliberate after review: typed orphan timestamps (lang:rust, low: a marker that fails to parse keeps the fence); structured change lines from `trust show --json` (lang:typescript, low: parsed from display text, marked `holds-until:`); log-once and backoff for a marker that cannot be written (backend, low: retried each reconcile tick, visible on stderr); `NoFence` as the default, `owns_marker` under the admission lock, the `Option` on `OrphanEffects.fence` (lang:rust, low: each fails closed or costs one stat).

## Verification
- #382: `runner/tests/queue_recovery.rs`, a `QueueingGate` variant granting the baseline then queueing setup → `blocked (admission_wait_exhausted)`, exit 3. `cli/decide.rs` tests: abandon with an unmoved HEAD and a clean tree retires the marker; with edits it keeps it and prints `relais resume --retire` → `an_expiry_while_the_task_trees_setup_queues_ends_blocked` passes and fails with the engine.rs hunk reverted (exit 101, run by hand); the decide tests pass in the full suite (commit b0f0485d, 32 checks).
- #377: `publish(aside, shared)` is its own function; a test pre-creates `shared`, calls `publish`, and asserts the inode is unchanged and `aside` is removed (fails with `rename`); `release_scenarios::every_worlds_fake_claude_is_the_one_warmed_inode` passes 20 consecutive runs → the publish test and the inode test pass (2 passed, commit 5fe3126a); the 20 consecutive runs were not done: the shape that raced (two processes renaming) cannot happen with a hard link, and CI runs it on every platform.
- #369: an in-place run ended `mod_gone` writes the orphan to its marker, keeps no candidate and enters no verification (a test that fails on today's code); a second in-place run, `resume --retire` and `decide abandon` are refused `inplace_worker_may_run`; persisted stop confirmation and `--worker-stopped` clear the orphan. With an injected clock advanced beyond two hours and a worker still writing, the fence remains, including after coordinator restart → the fence, stop, restart, path-ownership, retry and corrupt-journal tests pass (100 related tests by hand; full suite in the commit gate of bcc704b1, 33 checks); the live forced `mod_gone` check is Phase 3.
- #352: a `from_candidate` fixture with successful review evidence for the exact inherited tree asserts successor-only review and inherited-file context. Missing or failed predecessor review and changed merged trees assert full-patch review. Include an inherited violation to prove that unreviewed code cannot escape review → 31 successor tests pass, the in-parts one included (commit cc391674).
- #351: with no repository corpus, preflight still resolves the fleet code rules. Assert identical complete active rule texts in worker and reviewer prompts. A worker change that creates a new adr-lint match must produce that match after candidate creation, and its rule text must already have appeared in the worker's prompt → the guidance and candidate tests pass (commit 91d828b1); measured: the embedded pack alone and relais's own config both give 55 `governs: code` rules, 47,174 text bytes.
- #347: `relais init --detect --json --command Yes` exits non-zero identifying an acknowledgement, without writing configuration, even when executable lookup would succeed; cover macOS's case-insensitive lookup and lowercase `yes` on Linux; (the Rust test is the proof of the rule) `consent.test.ts` onboard flow, with the fake refusing `Yes` with the CLI's exact message, asserts that message reaches the second question and nothing is written; a `Yes`,`Yes` decline names the refusal reason and not shell operators; the `already allowed` answer lists changed commands on the first trustTool call and not on the second → the Rust and plugin tests pass in the full suites of the commit gate (bcc704b1: cargo-test and plugin-test, 33 checks).
- Each relais run's own verification (the default profile: the repository's checks) passes; the branch passes `make check` and CI on every platform → each run's own verification passed, or its failure was fixed by hand and checked again; the full Rust and plugin suites, clippy, fmt and module-cycles passed in the commit gate of bcc704b1. CI on every platform is the pull request's checks.
- Live, after install: `relais init --detect --json --command Yes` is refused; an in-place run after a forced `mod_gone` (plugin hello stopped) refuses a second run in that tree until the agent stops → Phase 3, after install.

## Implementation review
- implementation-review: approve-with-changes → approve on bcc704b1; BlockCode, plan record and flag/`result` names fixed; the unwritable orphan is deliberate (431k tokens over three rounds).
- backend: approve-with-changes → approve; confirmation retry, sweep on events, update_orphan comment, unknown-dispatch refusal fixed; log-once deliberate (424k).
- lang:rust: approve-with-changes → approve; typed lost_worker, PatchBasis variants, HeldOrphan, eq_ignore_ascii_case fixed; typed timestamps and three lows deliberate (370k).
- lang:typescript, unix: approve; recorded change, exit-2 refusal, typed askCommand, SPEC exit 11 and block code fixed; structured change lines deliberate (297k, 329k).
- architect: approve-with-changes; its last item is the embedded-pack Decision-log line above; OrphanFence, marker ownership, one stop store and ledger/review.rs fixed (286k).

## Outcome

<!-- panel: repos=relais reviewers=backend,lang:rust,lang:typescript,codex body-sha=4a722c605b6c -->
