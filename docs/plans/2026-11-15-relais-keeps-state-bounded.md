---
status: done
branch: feat/bounded-state
repos: [relais]
adrs: []
---
# relais keeps its state directory bounded

## Review panel
**Decided 2026-11-15:** Phase 1 (hard-link `candidate-latest.patch`) is dropped; the copy stays. Delta: backend approve-with-changes (57k, 23 s). Receipt rule: the person made a receipt required for `accepted` runs only after the pilot found 298 old runs (1,090 MB) kept forever for lacking one; delta backend resolved (57k, 13 s).
📍 relais · plan reviewed, worktree `relais-wt-bounded-state` made · next: land the plan, then Phase 2. Panel: architect, backend, rust, po, tui, unix (opus), codex.
**Changed by review:** crash-safe prune (`.pruning-<id>`) keeping pinned, live-subtree and worktree runs; doctor watches the purge; Goal measured at ~1.1 GiB.
**Verdicts:** R1 6 approve-with-changes; R2 2 approve, 4 approve-with-changes; codex approve-with-changes; 18 carried; words 1467 → 1941 (×1.32, past ×1.25 only by high fixes and corrected numbers); remove 0, merge 0.

📄 Full reviews: [2026-11-15-relais-keeps-state-bounded.reviews.md](2026-11-15-relais-keeps-state-bounded.reviews.md)

## Picture

```
 today                               after this plan
 -----                               ---------------
 run --> runs/<run>/ (kept forever)  run --> runs/<run>/
         candidate-N.patch                   candidate-N.patch
         candidate-latest (a copy)           candidate-latest (a copy)

 hook --> hook_journal.jsonl         session start --> retire --all
          (grows forever)                  `-daily-> prune runs > 30 d
                                                     (pins kept)
                                     hook --> journal --32 MiB--> .1
                                     doctor --> prune stamp age
```

A run's artifacts and every hook firing used to stay forever, with the
latest patch stored twice. After this plan, the session-start job prunes ended runs over 30 days once a day, the
journal rotates at 32 MiB, and `relais doctor` shows when it last ran.

## Context

`~/.local/state/relais` measured 3.8 GiB on 2026-11-15 and only grows: nothing deletes on its own.

| Part | Size | Cause |
|---|---|---|
| `runs/` (1,483 dirs, median 376 KB) | 1.8 GiB | `relais resume --prune-runs` exists but is manual only; 336 dirs > 30 d = 1.1 GiB |
| one run (Oct 4) | 700 MB | `candidate-latest.patch` is a byte copy of `candidate-N.patch` (`runner/attempt.rs:873-874`) |
| `hook_journal.jsonl` | 332 MB | append-only, never rotated, nothing reads it back (`hook/respond.rs:408`) |
| ledger backups | ~330 MB | relais writes only `bak-v27`/`bak-v30`; ~260 MB are hand-made |

Measured again before the panel: the ~1 GiB the table does not cover was live verify checkouts (`verify/<run>/`, APFS clones, removed when their run ends); at 12:04 one live run's read 7.1 GiB by `du` and the directory 9.7 GiB. They are out of scope here. The 30-day prune frees 1.1 GiB (336 dirs); 1,126 younger runs keep 0.7 GiB.

Goal: steady state about 1.1 GiB (runs < 30 d 0.7 + journal <= 64 MiB + ledger 0.13 + ballast 0.06 + the rest 0.1), plus hand-made backups until the person deletes them, with no work added to a run's hot path. Speed is a hard constraint: every added step is either one `stat`/`fstat`, or runs in the detached session-start job.

## Phases

### 1. Hard link for `candidate-latest.patch` (`runner/attempt.rs`): dropped
- The person dropped it on 2026-11-15: it saves 40.5 MB in runs < 30 d. The copy stays, and Phase 2 prunes it with its run.

### 2. Automatic, throttled prune of ended runs (`prune.rs`, `cli/resume.rs`)
- **Shared judge.** Move the CLI closure (`resume.rs:840-897`) into `prune::RunJudge`, which loads `ledger.pinned_candidates()` and the baseline-cache texts once. It replaces today's 1.6M file reads. Checks, in order:
  1. ended and not waiting on a person (as today);
  2. has a receipt, required for `accepted` runs only: relais stores receipts for `accepted` runs alone (measured 2026-11-15: 200 of 318 accepted, 0 of ~800 other ended runs since Oct 15), so any other ended run that waits on no one is judged without one (the person's decision, 2026-11-15);
  3. **new:** no pinned candidate names the run (live `kind: verify`, `from_candidate`, pending successor intents; `ledger/runs.rs:888`). This also fixes the manual `--prune-runs`;
  4. **new:** the whole deletion subtree is safe: keep the root if any descendant package run is live, waits on a person or is pinned, or if any checkout remains beneath it (legacy `worktree`, `packages/worktrees`, `packages/verify`); unreadable descendants keep it. Same for manual and automatic pruning, re-checked before the rename; an integration test keeps a modified child checkout under an old eligible parent;
  5. not cited by baseline-cache (as today);
  6. dangling evidence rows counted (as today).

  `apply_run_prune` re-checks the single run's pin immediately before each delete (one indexed read), so a pin created mid-prune keeps its source.

  An unreadable pin or cache keeps every run. `--prune-runs` output does not change except for the new keep reasons and a keep tally by reason in its last line (e.g. `kept 538: 12 pinned, 3 worktree, 523 younger than 30d`).
- **Automatic path.** `prune::auto_prune` is called from `retire_all_command`, which the plugin already spawns detached at every session start (`claude-plugin/hooks/learning.ts:56`, output to `resume.log`).
  - The body moves into `retire_all()`, so the early "no run worktree is retained" return (`resume.rs:410-417`), and a retire that fails after the ledger opened, still reach the prune; its summary follows the "no run worktree" line.
  - Throttle: skip if `runs-prune.stamp` is newer than 24 h (a future mtime counts as stale); otherwise take `runs-prune.lock` (`procs::LockFile::try_acquire`, skip if held), re-check, write the stamp before deleting, then prune with the 30-day age.
  - Deletion is crash-safe: rename `runs/<id>` to `runs/.pruning-<id>`, then `remove_dir_all`; each prune first finishes leftover `.pruning-*` dirs.
  - Output on stdout: the manual line per deletion, `prune  <run>  <days>d  <size>  <n> evidence rows left dangling` (resume.rs:922, no child `dangling` lines), then one `auto-prune <ISO time>: pruned N of M older than 30d (SIZE)` summary. Kept runs are not listed; a throttle skip prints nothing and exits 0.
  - A prune error goes to stderr as `relais: prune <run>: <err>`; the other runs still go, the stamp stays, and `retire --all` exits OperationalFailure, as a failed retirement does (resume.rs:413).
  - Precedence, stated once: `--no-prune` flag, then `RELAIS_AUTO_PRUNE_DAYS` (age; `0` turns it off), then the default 30. An invalid env value is a usage error on stderr and skips the prune. `--help` says `--retire --all` prunes. Not a policy key, so the authority hash is unchanged.
- **Not the coordinator thread:** `sweep_tick` shares its 15-s loop with `group_tick` (admission tokens); a first prune would stall it.
- **Pruned-source refusal:** `cross_check`, when the whole `runs/<source>/` directory is gone, blocks `candidate_missing` ("its run directory was pruned") instead of the misleading `candidate_digest_mismatch`.

### 3. Size-rotated hook journal (`hook/respond.rs`), and `resume.log`
- `append_journal`: unchanged single `O_APPEND write_all`, then `file.metadata().len()` (one `fstat`).
- Past `JOURNAL_MAX = 32 MiB`, `rotate_journal`:
  - `try_acquire(<journal>.lock)`, created 0600; skip if held;
  - re-check the path's size under the lock; skip if already small;
  - `rename` to `hook_journal.jsonl.1`, keeping one generation.
  - Nothing is ever truncated: a writer still holding the old fd lands its whole line in `.1`. Errors are ignored (best effort, SPEC §23); on a failed rename (Windows, open file) the next append retries, and the rotation tests are unix-only.
- `resume_logged`: the same check and rename for `resume.log` at 1 MiB, done once before `open()` and never while the child holds the fd, mirroring `coordinator::daemon_log` (`coordinator/mod.rs:1297`).
- Put the rotation (lock + re-check + rename) in one `fsutil::rotate_if_over(path, max)` helper used by the journal, `resume.log` and `coordinator::daemon_log`, so `hook` does not import `procs` directly (the lock reaches it through `fsutil`'s re-export of `procs::LockFile`, with no cycle); run `scripts/check-module-cycles.py` before starting this phase.

### 4. Ledger backups: a doctor note, not a deleter (`cli/doctor.rs`)
- relais writes only the fixed `bak-v27`/`bak-v30`: a keep-two cap would delete nothing.
- Also add a purge-health finding: warn when `runs-prune.stamp` is missing or older than 3 days while ended runs older than the prune age exist, and print the state-directory size and the effective `RELAIS_AUTO_PRUNE_DAYS` (`reliability.steady-state`: the purge is monitored).
- Add a finding beside `holdouts_finding`. It lists every `ledger*` file other than `ledger.sqlite{,-wal,-shm}` with size and age, marks relais's own as "safe to delete once you will not roll back" and the rest as "not written by relais", and gives the total. Layout: sorted by size, at most 10 lines then `... and N more (TOTAL)`, each line under 80 columns with the full path last; only relais's own files get a copyable `rm <path>`; no colour. It deletes nothing.

### Docs
- SPEC §8 (:343) pruned-source refusal; §12 (:534) judge checks, auto-prune, its controls, `resume.log` rotation, doctor notes; §23 (:911) journal rotation.
- CHANGELOG `## Unreleased`: Changed, Fixed (`--prune-runs` keeps pinned and worktree-holding runs), Added (doctor notes).

## Non-goals
- No compressed or content-addressed artifact store (a later programme if wanted).
- relais never deletes a ledger backup, journal line it did not rotate, or any file it did not write.
- No new policy key; the authority hash is unchanged.

## Code review
- relais: backend, lang:rust, unix, tui, architect (tui kept: `resume.log` lines, prune keep reasons and the doctor note are new output; po left out: a plan seat, not a code-review profile)

## Reuse
 `prune::{plan_run_prune, apply_run_prune, json_files_citing}`, `Ledger::pinned_candidates`, `procs::LockFile`, `fsutil::write_atomic`, and the `coordinator::daemon_log` rotation pattern.

## Tests (each seen red before the fix where it guards a bug)

**Unit tests**
- prune:
  - a pinned source run is kept;
  - a legacy-worktree run is kept;
  - the cache is read once;
  - auto-prune is throttled by its stamp;
  - a held lock skips;
  - the stamp is written before deleting;
  - `RELAIS_AUTO_PRUNE_DAYS=0` does nothing.
- respond:
  - the existing concurrency test (:1675) stays green;
  - rotation past the cap goes to `.1` with both files 0600;
  - 8 threads across exactly one rotation lose no line and every line is JSON;
  - a held rotation lock skips.
- doctor: the backups note lists relais and hand-made backups with size and deletes nothing.

**Integration tests (`release_scenarios.rs`, World harness)**
- `retire --all` prunes a 31-day-old ended run once per 24 h (backdate with `File::set_modified`, as at :3755): pruned, a second run kept by the throttle, then pruned after the stamp is 25 h old.
- A `candidate_inputs` row from a `running` run keeps its source through `--retire --all`, and `--prune-runs 0` says `keep … pinned`.
- `--retire --all --log` writes `prune <id>` lines to `resume.log` and no `keep` lines.
- A verify of a pruned source blocks `candidate_missing`, not `candidate_digest_mismatch`.
- An old `blocked` and an old `cancelled` run with no receipt are pruned; an old `accepted` run with no receipt is kept.

## Verification
1. Work in a worktree from `origin/main` (`worktree-task`). Implementation goes through relais (`relais:relais`).
2. `cargo clippy --all-targets -- -D warnings`, `cargo test`, `python3 scripts/check-module-cycles.py`, then `greengate list --json --stage pre-push`; push with `greengate land`.
3. On this machine after release:
   - `relais resume --prune-runs` preview: the pinned and worktree keep reasons appear;
   - one session start (or `relais resume --retire --all --log …`): `resume.log` shows the prune lines, `runs-prune.stamp` exists, `du -sh ~/.local/state/relais` drops by about 1.1 GiB toward the Goal (live verify checkouts excluded);
   - a second start the same day does nothing (no new `resume.log` line, stamp mtime unchanged);
   - after hooks fire, `ls -l hook_journal.jsonl*` shows the live file <= 32 MiB plus `.1`, both `-rw-------`;
   - one week later, `du -sk` per row is at or below the Goal; roll back if a `candidate_missing` hits a run the ledger still pins, or `relais doctor` reports the stamp older than 3 days.
4. `relais doctor` lists the hand-made backups. Deleting them stays the person's call, outside this plan.

### Observed before the push (2026-11-15, final tree `ae2fad8a`)
Input → expected → actual.
- Worktree `relais-wt-bounded-state` on `feat/bounded-state`, rebased onto v0.62.0 → phases through relais → Phase 3, the receipt follow-up and the review fixes accepted by relais with receipts (`run-6604f7ee5a525-963b`, `run-660537c2a21d4-547a`, `run-66054a634686f-141d2`, every criterion met, 8 of 8 profile checks); Phases 2 and 4 committed through the local gates after relais's verify blocked on the host-slot queue under load (the person's choice).
- Checks on the final code tree → relais's profile checks (fmt, clippy, four cycle checks, cargo-test, plugin-test) pass; 32 commit gates pass. Earlier: clippy and cargo-test stamped on `14606054e631`, 2,391 + 57 + 334 + 1 tests, 0 failed.
- `relais resume --prune-runs`, final binary, real state, read-only → old ended runs prunable, a tally by label → 299 of 357 prunable (888.0 MiB, 1,665 evidence rows); `kept 58: 13 checkout beneath, 24 no receipt, 21 waits on a person`. The round-1 binary had found 37 of 357, with 298 kept for "no receipt" (relais stores receipts for `accepted` runs alone) → the person's receipt rule.
- `relais resume --retire --all --log` twice on a scratch copy (`RELAIS_STATE_DIR`, 356 cloned old runs, 1,116 MB) → prunes once, then throttled → 299 `prune` lines and `auto-prune …: pruned 299 of 356 older than 30d (888.0 MiB)`, no `keep` line, no `.pruning-*` left, stamp written; the second run added no line and left the stamp's mtime unchanged; the doctor then showed no overdue warning (260.0 MiB).
- Journal rotation on a scratch state: a 33 MiB `hook_journal.jsonl` (0600), then one `relais hook` firing → rotated to `.1` with that firing's line and all 34,193 earlier ones; the next firing opened a fresh one-line journal; every file `-rw-------`.
- `relais doctor`, final binary, real state → purge health and backups → `purge` warns: 235 overdue runs (864.2 MiB), the stamp missing, 19 `accepted` runs (4.7 MiB) without a receipt named as never pruned, a `next:` line; state directory 2.5 GiB without the verify checkouts. `backups`: 15 files, 299.3 MiB, `rm` for `bak-v27`/`bak-v30` only.
- The first PR push, CI `test (windows-latest)` → every check green → failed with os error 33 in three lock tests: the move to `fsutil` had swapped main's Windows lock (one byte past end-of-file) for std `File::try_lock`, a whole-file mandatory lock. Fix `d1d6da78` (relais run `run-66055a0f751e7-edd9`, approved): `LockFile` back in `procs`, its `imp::try_lock_exclusive` byte-identical to `origin/main` on both platforms, `fsutil` re-exporting it; profile checks pass and a new test reads the lock file while it is held. The regression is recorded against `run-6604f7ee5a525-963b`.

## Implementation review
- implementation-review: approve-with-changes, then approve on the Windows fix; 10 round-1 findings fixed, the delta's blocker (verification on the final tree) met above (109k + 160k + 25k).
- Profiles: backend, unix, architect approve and tui, lang:rust approve-with-changes on the delta; on the Windows fix backend, unix, tui approve, lang:rust and architect approve-with-changes, low only.
- deliberate: follow-up, cosmetic — tui: backups header path over 80 columns, `checkout beneath` label, long purge lines; unix: say an existing lock file keeps its mode.
- deliberate: follow-up, internal — rust: `Kept(_) => 0` arm, `PruneFailure::source`, typed states in `KeepReason`, one env reader, `ERROR_LOCK_VIOLATION` for main's literal 33, the Unix-only rotation-lock test; architect: `human_bytes` module, one public path for `LockFile`.
- deliberate: follow-up — backend: a failed delete names `runs/<id>`, not `.pruning-<id>`; implementation-review: `tree_bytes_except` is a lower bound.

## Release
Merge on green with `merge-when-green`, cut the next minor with `tag-release` (new automatic behaviour), and update the plugin so its session start runs the new binary.

<!-- panel: repos=relais reviewers=architect,backend,lang:rust,po,tui,unix,codex body-sha=165becc63b32 -->
