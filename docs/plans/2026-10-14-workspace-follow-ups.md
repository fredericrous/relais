---
status: active
branch: feat/workspace-follow-ups
repos: [relais]
adrs:
  - relais ADR-0009 (the real index is restored at the end of an in-place run; a baseline cache hit keeps the base's check outcomes) — amends ADR-0008, supersedes its detection-only promise and D13
---

# relais: close the workspace-reuse gaps

## Review panel

👉 **Decide:** none — approve if the worker-index gap is closed by restoring your index at the end of a run (your choice), and the other five gaps are the right set to close.
📍 relais · plan reviewed, no open blocker · next: one PR (restore, retire scan, doctor, held source, cache outcomes, golden), then release v0.22.0. Panel: backend (opus-5-5).
**Changed by review:** the restore takes git's own `index.lock`; `resume --retire` reports a changed index instead of restoring it days later; cached logs are stored as blobs, with a measured size cap; the golden is generated at two commits.
📄 Full reviews: [2026-10-14-workspace-follow-ups.reviews.md](2026-10-14-workspace-follow-ups.reviews.md)
**Verdicts:** backend round 1 approve-with-changes (7 applied), round 2 and the final body approve-with-changes, no blocker. 5 findings carried into implementation (below).
**Carried into implementation (apply as written):** Verification 1 and section 1 still say a killed run's index is restored by `resume --retire`; D22 governs, so it reports `Changed`, keeps `.orig` and leaves the index as it is after the kill, and Verification 8 says the same; the committed golden is produced from the branch-point commit and a PR-branch test must never regenerate it; emit `real_index_restored` / `real_index_changed` events; Verification 8 gains a happier step (two `cache_baseline = true` runs: the first base check's total log MiB recorded in section 5, the second `baseline_cached` with `base-cmd0.log` present and no `baseline_cache_outcomes_skipped`); a raised cap never exceeds 64 MiB (a quarter of the 256 MiB budget) unless the budget rises with it, and the 1,300-entry figure assumes one log per entry.

## Context

v0.20.1 shipped local workspace reuse (ADR-0008, plan `docs/plans/2026-10-13-local-reuses-workspace.md`). It left six gaps, recorded there as D11–D16 and in the release notes. Another session shipped v0.21.0 (task-type promotion, trial ground-truth) meanwhile; nothing on `origin/main`, in any branch or any open PR touches these, so the work is free. The next version is v0.22.0.

The person's decisions (2026-10-14): do the gaps, unless another session already did (checked: no). For the worker index: **restore the real index at the end** (not report-only, not a Bash-rewriting hook).

| # | Gap (today) | This plan |
|---|---|---|
| 1 | A worker's `git add` changes the person's real index; relais only detects it (D11) | Restore the byte copy at the end |
| 2 | `relais resume --retire` with no run id cannot find an interrupted in-place run (D12) | Scan the repo's worktrees for markers |
| 3 | No `doctor` line for the workspace (D15) | Add one |
| 4 | A fork can seed from a worktree an in-place run is editing (the "installing lock" gap) | Refuse to seed from a source with an in-place marker |
| 5 | A baseline cache hit loses the base's check outcomes, so an identical candidate (an inspect run) re-runs the checks (D13) | The entry keeps outcomes and log text |
| 6 | The isolated path is pinned only by the existing suite and a silence test (D14) | A golden generated from the pre-#237 code |

Dropped on purpose: the `installing` lock as such. It guarded a quietness heuristic that authorizes nothing; gap 4 closes the one case where a relais-owned writer can be cloned mid-write, with no lock. Also rejected: rewriting worker Bash through a plugin hook to carry a private `GIT_INDEX_FILE`. It touches every worker command, and the mod API's contract for rewriting a `tool.call` cannot be checked from this repository (its type file is a stub). Restoring gives the person the guarantee they care about (their index, byte for byte) without that risk.

## Design

### 1. Restore the real index (`workspace/inplace.rs`)

- Today `Lease::keep_original_index` copies `<git-dir>/index` to `<run>.index.orig` at start, and `finalize` compares `git ls-files -s` with the marker's `index_listing`, reporting `real_index_changed`.
- New: in `finalize`, after `apply_tree` succeeds and the listing differs, `restore_index`: take `<git-dir>/index.lock` with create-new (git's own lock: a concurrent git then fails with "index.lock exists" instead of racing us), copy `.index.orig` into it, fsync, and `rename` it over `<git-dir>/index`; the lock is removed on every error path and a rename error (Windows: the index held open) is reported, never swallowed. Then re-read `ls-files -s` and require it to equal the start listing. Success removes `.index.orig` and reports `real_index_restored`. Anything else (an `index.lock` exists because git is running, the copy is missing, the listing still differs) keeps `.index.orig` and reports `real_index_changed` exactly as today, with its path.
- Only the run's own `finalize` restores, in the same process that ran the worker. `relais resume --retire` of a run that died, possibly days earlier, never overwrites the index: it reports `Changed` and prints the `.orig` path, because staging done after the run died is not the worker's to discard (D22).
- Not restored: the `HeadMoved` outcome (the index belongs to the moved `HEAD`; tree, branch and index are all left alone) and an `Interrupted` run until `resume --retire` runs the same `finalize`.
- The cost is stated, not hidden: staging the person does during a run is lost. The in-place start message already says not to edit the worktree until the outcome arrives; it now says "or stage".
- `Outcome::Applied` grows `index: IndexOutcome { Untouched, Restored, Changed }`, replacing the `real_index_changed: bool`. The runner transition detail, `retire_in_place`'s output and the receipt note use it.

### 2. `resume --retire` with no run id (`main.rs`, `workspace/mod.rs`)

- New `workspace::worktree_paths(repo) -> io::Result<Vec<PathBuf>>`: `git worktree list --porcelain`, the `worktree <path>` lines.
- `retire_all_command` gains a second source of work: for each worktree of the repository it is run from, `inplace::git_dir` then `leftover_markers`. Each marker names a run; the existing per-run steps apply unchanged: `ledger.run_status` (none = "kept, no such run on record"), the orphan/interrupt handling for a non-terminal run, `reconcile_run`, then `retire_in_place`. The state-resolution block of the current loop is extracted into one helper both sources call, so the two cannot drift.
- "no run worktree is retained" is printed only when both sources are empty.
- Scope: the repository `resume --retire` is run from. A marker in another repository is found by naming its run id, as today. Outside a repository the scan is skipped with one line saying so; a `prunable` worktree or an unreadable git dir is one `kept, <path>: <error>` line; a run found by both sources is processed once (dedup by run id).

### 3. `relais doctor` workspace line (`doctor/mod.rs`)

- `workspace_finding(inputs, cow, markers) -> Finding` is pure. `doctor()` gathers the inputs, as it does for its other findings: machine `[workspace] mode`, `RELAIS_WORKSPACE`, `CI`, the repo's `in_place` opt-out, `repo::is_linked_worktree`, the leftover markers of this repository's worktrees, and a CoW probe (`fsclone::probe` of the repo's first regular file into the state directory; "not probed" when the state directory does not exist yet).
- Text: `workspace: <resolved mode> (<why: machine setting | CI | RELAIS_WORKSPACE | linked worktree | primary checkout>); CoW <ok | unavailable: <reason>> between <repo volume> and <state volume>`. `Level::Warn` when a marker is left (naming `relais resume --retire <run>`) or when the mode would seed and CoW is unavailable (naming the fix: put `RELAIS_STATE_DIR` on the repository's volume); `Ok` otherwise. It appears in `--json` through the existing `Finding`, with no schema change.

### 4. A fork does not seed from a held source (`runner/setup_gate.rs`)

- `seed_tree` checks the source's git dir for any in-place marker (running or interrupted) before cloning. Found: event `workspace_seed_skipped: <source> is held by in-place run <id>` and today's full setup. No lock is taken; the check runs again after `reuse::seed`, and a marker found then discards the seeded directories (`workspace_seed_discarded`) and falls back to full setup. Any `*.json` under `relais-inplace/` counts, parseable or not, and a git dir that cannot be read means no seed. The marker exists for the whole life of an in-place run, which is the window in which its tree can be half-written.

### 5. The baseline cache keeps outcomes (`verify/mod.rs`, `runner/mod.rs`)

- Entry v4: `{ "failures": [...], "outcomes": null | [ { "outcome": <CheckOutcome with file names, not paths>, "log": "<sha256>", "junit": "<sha256>" | null } ] }`. `BASELINE_MEANING_VERSION` 3 → 4, so old entries miss. The bytes are stored beside it as `<sha256>.blob` (logs need not be UTF-8). Written to a temp file and renamed.
- Outcomes are stored only when the logs total at most 16 MiB; above that the entry keeps `failures` alone and behaves as today. happier's `yarn verify` logs may be large, and caching must not regress for exactly the repository this is for. Measured 2026-10-14 on 421 base-check logs from past runs: largest 0.2 MiB, p95 0.16 MiB, none from happier (its baseline never completed, so no `yarn verify` log exists yet). The 16 MiB cap is 80× the largest observed, and 256 MiB holds about 1,300 entries at that size (256 ÷ 0.2). The pilot records happier's first `yarn verify` log size here, and the cap is raised to twice it if it would not fit, never above 64 MiB (a quarter of the 256 MiB budget) unless the budget rises with it; the 1,300-entry figure assumes one log per entry. `put` removes the oldest entries and blobs past 256 MiB. A degraded entry emits `baseline_cache_outcomes_skipped: <n> MiB`.
- A hit re-materializes each log (and JUnit copy) into the new run's `logs/` as `base-cmd<N>.log`, requires `sha256` of the bytes to equal the stored `log_sha256` (any mismatch or missing piece = miss), rewrites `log_path` to the new file, and yields `(failures, Some(checks))`. The runner then records those logs as `CheckLog` evidence for the new run (the hit currently records none), which also fixes the JUnit-evidence skip on reuse (`runner/mod.rs:~4176`). Setup is not cached, so no `SetupLog`.
- `Baseline.cached` stays true; the pane text is unchanged (`baseline_summary` tests `cached` first). The `holds-until` comment at the hit site goes.

### 6. Golden for the isolated path (`crates/relais/tests/golden/`)

- A test runs fixed scenarios with `Workspace::Isolated` and a deterministic backend: an accepted run with one setup step, and a run blocked by a failing base setup. It writes a normalized document: the ordered event kinds with their `what`/`reason`/step names, the ledger transitions (state, reason), the files under `logs/`, the layout of `worktrees/` and `verify/` while the run is live, and the receipt's checks and flags. Ids, timestamps, shas and absolute paths are replaced by placeholders.
- The golden is generated in a scratch worktree at #237's merge commit's first parent (`git rev-parse <merge>^1`, recorded here), by a test body that names no `Workspace` type (a one-line shim on main sets `Isolated`), and again at the branch point; their diff must be empty or each line attributed to a v0.21.0 commit, and the branch-point document is the one committed, through an env var that writes the document instead of comparing. It is committed; the test on `main` compares byte for byte. A difference means the isolated path moved.
- Not in the golden: baseline-cache files (the v3/v4 keys change in every mode, stated in the CHANGELOG).

## Changes

- `crates/relais/src/workspace/inplace.rs` (restore, `IndexOutcome`, `held_marker`), `workspace/mod.rs` (`worktree_paths`), `runner/setup_gate.rs` (held-source skip), `runner/mod.rs` (index outcome in transitions and the start message; baseline hit with outcomes; `record_logs` on a hit; remove the holds-until), `verify/mod.rs` (cache v4), `main.rs` (`retire_all_command`, shared state helper, output lines), `doctor/mod.rs` (finding).
- `crates/relais/tests/golden/isolated-run.json` and its test; unit and runner tests below.
- `docs/SPEC.md` §8 (index restored; detection no longer the promise), §10 and §18 (a hit keeps outcomes), `.adr/records/0009-*.md` plus a key in `.adr.yaml`, `aval heads --write`, CHANGELOG, `docs/plans/2026-10-13-local-reuses-workspace.md` D11/D13 get a "superseded by ADR-0009" line.

## Verification

1. **Index.** Runner test (extends `a_worker_that_stages_into_the_real_index_*`): the worker stages a file, the run is accepted; afterwards `<git-dir>/index` is byte-identical to its bytes at start and no `.index.orig` remains. With `index.lock` present at the end: the index is not touched, `.orig` is kept, `Changed` is reported. `HeadMoved`: index untouched. A killed run through `resume --retire` (D22): the index is left as the kill left it, `Changed` is reported and `.orig` is kept.
2. **Retire.** CLI/unit tests: two linked worktrees, a marker in one; `resume --retire` with no id restores it, leaves the other alone, prints one line per run; a marker whose run is not on record is kept with that message; no markers anywhere prints the existing "no run worktree is retained" line. The directory survives, with and without an id.
3. **Doctor.** Pure-finding tests for each reason and level, in text and JSON; `RELAIS_WORKSPACE=isolated` and `CI=1` readings; a left marker warns with the retire command.
4. **Held source.** A fork run with an in-place marker in the source seeds nothing and says why; without one it seeds as before.
5. **Cache.** Two runs with `cache_baseline = true`: the second has `baseline_cached`, no `verify-1`, its `base-cmd0.log` under its own artifacts directory with the stored sha, and `CheckLog` evidence rows of its own (JUnit too); a corrupted or truncated entry is a miss; over 16 MiB it degrades to failures-only; `an_inspect_answer_is_kept_*` still passes.
6. **Golden.** Generated at #237's first parent and at the branch point, diff attributed, and the branch-point document asserted on the new code, byte for byte, for both scenarios; the test fails if an event, a transition, a log name or the layout changes.
7. `make check` equivalent (fmt, clippy `-D warnings`, module cycles, ADR corpus, full test, plugin) green; CI green on all seven jobs.
8. Pilot with the branch binary on scratch `RELAIS_STATE_DIR`/`RELAIS_CONFIG_DIR`: in a scratch linked worktree, kill an in-place run mid-way, then `relais doctor` (warns), `relais resume --retire` (no id) retires the run, reports `Changed` with the `.orig` path when the index differs (D22), `relais doctor` (ok). Then, in happier, two `cache_baseline = true` runs: the first base check's total log size in MiB is recorded in section 5, the second is `baseline_cached` with `base-cmd0.log` present and no `baseline_cache_outcomes_skipped`.

## Delivery

One PR, reviewable commits: plan and ADR-0009, then index restore, retire scan, doctor, held source, cache outcomes, golden. After merge, release v0.22.0 through `tag-release` (the plan's outcome is a released fix) and verify the published assets and the Homebrew tap. The pre-commit and pre-push gates time out under machine load; the commits are made at load below 35, a push is preceded by `amont rehearse --wait`, and no gate is skipped without the person's say-so.

Rollback: remove the `[workspace]` setting if set; cache entries of v3 are ignored by v4 and v4 entries by v3 (a miss, never a wrong answer); there is no ledger change.

## Not in scope

- Concurrent runs in one worktree; a `relais apply` command; sharing installs by hardlink.
- Rewriting worker Bash with a private index (rejected above).
- The macOS flake in amont's `a_newer_commit_cancels_the_running_rehearsal` (another repository).

## Decision log

- D17 (person, 2026-10-14) The worker index gap is closed by restoring the real index at the end. Supersedes D11's "detection only".
- D18 `resume --retire` with no id scans the worktrees of the repository it is run from, not every repository.
- D19 The `installing` lock is dropped; a fork refuses a source that carries an in-place marker instead.
- D20 The cache keeps outcomes only up to 16 MiB of logs; above that it behaves as today. Supersedes D13.
- D21 The golden is generated twice, by a test body that names no `Workspace` type: at `aee42a0` (#237's first parent, checked) and at the branch point. Ten commits separate them (#240, #242, #243, #246 touch the runner), so their diff must be empty or attributed line by line; the branch-point document is the one committed and compared byte for byte afterwards.
- D22 Restoring the index happens only at the end of the run that did the work; `resume --retire` reports `Changed` and keeps the `.orig` copy (backend review, accepted).

<!-- panel: repos=relais reviewers=backend body-sha=b90d707e6ab5 -->
