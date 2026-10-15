---
status: active
branch: feat/runs-survive-the-machine (four PRs: fix/probe-timeout-not-unsupported, feat/candidate-identity, feat/resource-admission, feat/verification-evidence)
repos: [relais]
adrs:
  - relais ADR-0010 (runs consume machine resources through explicit budgets and leases; resource exhaustion is evidence, never a verdict) — new; amends SPEC §9, §10, §11, §23
  - SPEC §4 (verify kind, candidate identity), §8 (scope, conflicts), §9 (revise successor intent), §29 (heartbeat, progress) — amended; ADR-0004 unchanged (D6)
---

# relais: runs that survive a loaded machine, and revisions that carry their candidate

## Review panel

👉 **Decide:** none — approve if PR 0 (probe timeout ≠ unsupported, your recommended fix) ahead of the three ordered PRs, with PR 0 shipped in v0.22.0 and PRs 1–3 in the next release (v0.25.0 at this writing), is right; D6 settled by ADR-0004.
📍 relais · plan reviewed after your eight findings, §0 added for the load-335 probe refusal, nothing built · next: PR 0. Panel: backend (opus-5-5), lang:rust (opus-5-5), tui (opus-5-5), unix (opus-5-5).
**Changed by review:** conflict check and task tree share one `merge-tree -z` result (index built per git-update-index rules); usage re-sequenced by the driver, no new endpoint; flock slots all-or-nothing, inherited by the process group.
**Verdicts:** round 1 4× approve-with-changes; your delta, then 3 backend confirmations (11 + 1 + 1 findings applied), final approve, no open blocker.
**Carried into implementation:** the A2 sentence chains two "so" clauses; split it when the plan lands. §0.4: no launch passes `--max-turns` today (`max_turns: None` at `runner/mod.rs:2656`, `:4555`, `adapter/native.rs:949`), so PR 0 first greps for one; with none, the re-probe/`probe_unchecked` guard is not built and the only loss is the manifest `turn_ceiling`, recorded as `unknown (timed out)` rather than `unavailable`.
📄 Full reviews: [2026-10-14-runs-survive-a-loaded-machine.reviews.md](2026-10-14-runs-survive-a-loaded-machine.reviews.md)
**§0 delta (your load-335 report):** backend approve-with-changes, 6 findings applied (typed `ProbeFailure`, timeouts not cached, event consumers listed, measured live check); confirmation approve-with-changes, 2 carried above.

## Context

One task (book Claude Code advisor tokens, 2026-10-13/14) took five relais runs and still did not land. None of the five failed on its code. Observed, with run ids:

| Run | What ended it | Root cause in relais |
|---|---|---|
| run-65dbb83cb9a3a-11d7f | review finding (my contract's premise) | — (legitimate) |
| run-65dbec9a07f25-9f4d | `scope_exceeded` on two one-line, compile-forced edits | the only follow-up is a hand-written successor contract |
| run-65dbf5645a538-11ba0 | two review findings | — (legitimate); successor again had to say "apply /path/candidate-latest.patch" in prose |
| run-65dc62a01e3d1-bf16 | `budget_exhausted` | `No space left on device` while compiling → judged `behavioral_failure` → repair → wall clock |
| run-65dc815630ab3-117d3 | `interrupted` (worker did not stop in its wall time) | a Sonnet worker spent ~45 min re-running `make check` to "apply a patch unchanged" under load average 100–300 |

Plus, reported during a release commit on 2026-10-14: at load average ~335 `claude --version` took more than 30 s; relais treats a probe that does not answer within `PROBE_TIMEOUT` (30 s, `tooling.rs:32`) as an unsupported Claude Code and refuses the run, so **every** `relais run` / `dataset replay` stops under heavy load, and 6 Rust test scenarios failed on startup the same way. The plan below did not cover this; §0 does.

Plus: every baseline was red on unchanged `main` at least once, from a full disk (run 1) or from 3–5 plugin tests timing out at 5000 ms under load (runs 4, 5; a different set each time); `stalled: no events for 36m` showed on healthy runs; every native run displays `¢0.00 est`.

Facts from the code (`crates/relais/src`, at `origin/main` c8d640a):

- **Already on main, unreleased:** #240 tells workers "do not run the full suite yourself" (`runner/mod.rs:5113-5126`); `make plugin` reruns the plugin suite once when every failure is the 5000 ms timeout. **Released, not installed here:** v0.20.0 (ADR-0008) seeds `target/`/`node_modules` copy-on-write (`runner/setup_gate.rs:39`). The installed binary is **0.19.0**, so runs 1–5 had neither.
- Kinds are `Task::{Change, Inspect}` (`contract/mod.rs:263`). Nothing starts a run from an existing candidate; `revise --successor` is stored (`ledger/mod.rs:1106`) and never read. Candidates are `refs/relais/candidates/…` plus derived `runs/<run>/candidate-{n,latest}.patch` (`runner/mod.rs:2932-2945`). Read hints must resolve at `base_ref` (`context/mod.rs:424`). `relais run` is plugin-only (`adapter/native.rs:101`): only the plugin starts runs.
- Baseline red/green is per check label (`verify/mod.rs:1061`, :190); JUnit feeds only named-test criteria. A check red on base and on candidate still spends repairs (`runner/machine.rs:305-357`), which is what lets a contract fix an existing failure. The baseline cache stores failed labels only (`verify/mod.rs:2060`). No check is rerun.
- Nothing recognises ENOSPC / `SQLITE_FULL` / OOM, in check output or in relais's own writes.
- The deadline is `Instant::now() + max_wall_seconds` taken **after** the baseline (`runner/mod.rs:1720`; this repo 2700 s); candidate verification is bounded only by each command's `timeout_seconds`.
- The coordinator is machine-wide (`coordinator/mod.rs:2327`); `HeavyLocal`/`max_heavy_commands` exist but nothing requests them. Checks already run in their own process group (`procs.rs:287`, `:565`). amont queues its own heavy checks on host slots — `flock` on a per-user directory, cores/4 slots, `AMONT_HOST_SLOT=held` for nested runs (amont ADR-0009 `hooks.host-concurrency`).
- `stalled` = `events.jsonl` mtime older than 600 s (`reap.rs:197-215`). `¢0.00 est`: `relais native usage` (a separate process) appends turn costs to `events.jsonl` without `seq`; the pane reads only the driver's stdout (`runs.ts:254`) and sums the zero remainder (`timeline.ts:281-290`). The ledger is right (691951 µ$ for run-65dc648407618-1004b).

**Intended outcome:** a mechanical follow-up costs no worker and is reproducible; resource exhaustion is reported as evidence with a decision, never repaired blindly or read as a red base; heavy work across sessions and tools shares one bounded queue; every wait and every command has an explicit budget; the pane shows real cost and real progress.

## Design

### 0. A version probe that times out is not an unsupported version (PR 0)

Today: `ClaudeBackend::ask` (`adapter/claude.rs:149-170`) maps a `--version`/`--help` that ends `Ended::TimedOut` to `ProbeFailure::Refused { ended: "timed out" }`; `run_refusal` (`main.rs:5319`) passes it to `unsupported_claude_code_because` (`native.rs:71`), whose rule "a version that cannot be read is outside it too: nothing says it works" refuses. Callers: `relais run` (`main.rs:5446-5462`) and `dataset replay` (`main.rs:6005-6018`).

- **0.1 Typed failure.** New `ProbeFailure::TimedOut { args, after: Duration }`, produced in `ask` only for `Ended::TimedOut` (cancelled, signalled, non-zero exit, silent keep their variants). Display is built from `after`: ``"`claude --version` did not answer within {after}s (machine busy?)"``.
- **0.2 Three-way verdict.** `native::claude_code_support(printed, failure) -> Support` with `Support::{Supported, Unsupported(String), Unchecked(String)}`. `Unchecked` only when the failure is `TimedOut`; a version that answers outside the range, an unparsable answer, a non-zero exit, a missing binary stay `Unsupported` (the "nothing says it works" rule keeps its meaning for an answer that is wrong or absent, not for one that is late). `unsupported_claude_code[_because]` become thin wrappers so `doctor` and existing tests keep their shape. `run_refusal`, its two callers and `doctor` pass `Option&lt;&amp;ProbeFailure&gt;` instead of a stringified failure; no matching on Display text.
- **0.3 Warn and continue.** `run_refusal` returns `Ok(None)` on `Unchecked` after printing one stderr line: `relais run: warning: Claude Code version unchecked: <reason>; continuing (supported: >= 2.1.291, < 2.2.0)`, and the run records an `environment_warning {kind: claude_code_version_unchecked, detail}` event so the receipt and pane show it. The hello check (`hello_refusal`) still runs: a live plugin session is itself evidence a supported Claude Code is in front of the user. `dataset replay` gets the same treatment.
- **0.4 What the run loses.** A `TimedOut` failure is not settled: it stands for `TIMEOUT_RETRY` (300 s), so one run start waits it out once, and the next use after that re-probes; answers and other failures stay settled (D12). Within one probe a timed-out `--version` also yields no `--help` capabilities: `runner/mod.rs:2030` gets `None`, which routing already treats as `Fact::Unknown` with its unknown-catalog carve-out (`route/rung.rs:113-131`). No launch passes `--max-turns` (checked: `max_turns: None` at every launch), so no re-probe guard is built; the manifest's `turn_ceiling` reads `unknown (timed out)` (`TurnCeiling::UnknownTimedOut`).
- **0.5 doctor.** `doctor` reports `claude code: version unchecked (timed out after 30s) — runs will warn, not refuse` as a warning, not a failure.
- **0.6 Testable without waiting 30 s.** `ClaudeBackend` gains a `probe_timeout` field (default `PROBE_TIMEOUT`), set by a `#[cfg(test)]`/test-support constructor; the tests use a fake `claude` that sleeps past a 200 ms timeout. Existing tests in `native.rs:231-250` and `main.rs:9129-9150` keep passing unchanged.
- **Not taken:** caching the last answer across processes (option 2 of the report). It still refuses when the cache is cold or invalid, needs an invalidation key on the binary, and the timed-out case is what needs fixing; admission (B2) addresses the load itself.

### A. Candidate identity and revision semantics (PR 1)

**A1. Resolved candidate identity.** The public syntax stays `candidate: { run }` (optionally `{ run, index }`). At contract validation relais resolves it **once** to a `ResolvedCandidate { source_run, index, candidate_commit, patch_sha256, source_base_sha, target_base_sha }` and persists it (ledger table `candidate_inputs`, expand-only step) and on the receipt. Rules: the source run must be terminal (`candidate_source_running` otherwise); same repository identity (`candidate_foreign_repo`); a run with no candidate → `candidate_missing`. The applied content is the **commit object** the ledger recorded for that attempt (`runner/mod.rs:2926-2931`), not the best-effort ref (`:2921-2925`); resolution rewrites `refs/relais/candidates/<run>/<n>` and refuses `candidate_missing` if the object is gone; the `.patch` file is derived and its digest only cross-checked. A pin row protects the referenced ref and files from retire/reap (`workspace::retire`, `reap.rs`) while any non-terminal run, open decision or pending/claimed successor intent references them; an intent's candidate is resolved and pinned inside the decide transaction.

**A2. Application happens in preflight, before the baseline.** `git merge-tree --write-tree --merge-base=<source_base_sha> <target_base_sha> <candidate_commit>` runs in preflight (object-level, no worktree; exit 1 lists conflicted paths; git ≥ 2.40, checked by `doctor`); this is where a conflict is detected, so a conflicting candidate costs no baseline. The task tree is built from that same result, never from a second merge: its result tree is checked out, and on conflict (A4) merge-tree is run with `-z`, only its conflicted-file-info section is read (not the informational messages after it), and for each conflicted path a mode-0 removal (`0 0000000000000000000000000000000000000000<TAB>path`) is fed before its stage lines (`mode oid stage<TAB>path`) to `git update-index -z --index-info`, as git-update-index(1) requires, so no stage-0 entry survives beside the unmerged stages, so detection and application cannot disagree (e.g. on renames).

**A3. `kind: verify`** (`Task::Verify { write_scope, candidate }`): no worker, no repair, no escalation. Conflict at A2 → `blocked (candidate_does_not_apply)` naming the files; nothing else runs. Otherwise the task tree gets the candidate, then the existing post-attempt pipeline as for attempt 1: snapshot, scope check, verification, review. A failed check ends `failed` with evidence. Learning skips verify runs (`learn/dataset.rs`); `report` counts them as their own kind.

**A4. `from_candidate` on `kind: change`** (distinct conflict behaviour). The tree starts with the candidate applied. On conflict the run does **not** block: the tree is left with the 3-way merge result (unmerged index entries + markers), and the worker prompt carries `conflicts: [{path, ours: target_base, theirs: candidate}]` and "resolve these first". `snapshot_candidate` refuses while any unmerged entry or conflict marker remains in a conflicted path (`unresolved_conflict` → treated as a failed attempt: repair, then escalation, as today). Patch and scope check stay relative to `target_base_sha`.

**A5. Successor intent (transactional outbox, idempotent).** Because only the plugin starts runs, `decide revise --widen-scope` and `decide revise --successor-contract <file>` write, in **one ledger transaction**, the decision answer and a `successor_intents` row `{intent_id, decision_id UNIQUE, contract_digest, contract_json, state: pending}`. `decide --json` returns `{successor_intent}` at once. The plugin claims and launches it: `relais run --intent <id>` claims atomically (`pending → claimed(run_id)`); a second claim returns the existing run id (`intent_already_claimed`), never a second run. The plugin is the only reconciler (on start and each status poll): it launches `pending` intents; `native status` and `resume` only list them. A `claimed` intent is relaunched only when its run row is absent or its bound pid is dead (coordinator liveness probe), via compare-and-set on the old run id; never on "no first event yet". Repeating the same decide on an answered decision returns the same intent. `--widen-scope` builds the contract itself: same contract, `kind: verify`, `candidate: {run: <this run>}`, `write_scope` + exactly the out-of-scope paths (listed with `+/-` counts in the decision detail). The successor's receipt carries `predecessor` and the exact scope delta. SPEC §8 holds: a scope violation is never accepted; the widened scope is a new contract, verified and reviewed again.

**A6. Who may answer (D6, settled by ADR-0004).** `--widen-scope` is a `revise` answer: the orchestrator may give it as it may any answer, recorded with its typed actor and the exact scope delta; the decision advisor (ADR-0004 `decisions.advisor`) only advises and never answers. No policy change.

**A7. Plugin and skill.** `register.ts`/`runs.ts`: new contract fields; claim-and-launch of `successor_intent`; reconciliation on start. `skills/relais/SKILL.md`: "to verify an existing candidate use `kind: verify` with `candidate.run`; to continue one, `from_candidate`; never write 'apply this patch' into an objective."

### B. Resource admission, budgets and exhaustion (PR 2)

**B1. Explicit budgets** (Temporal's split: schedule-to-start, start-to-close, schedule-to-close). Per run:

| Budget | Covers | Default / source | On exhaustion |
|---|---|---|---|
| `work_seconds` | active work after the baseline: attempts, candidate verification, review (today's `max_wall_seconds`; `limits.wall_seconds` caps **this one**, as today) | repo `max_wall_seconds` | `budget_exhausted (wall_clock)` |
| `admission_wait_seconds` | cumulative time queued for any lease (`ModelWork` + `HeavyLocal`), baseline included | machine.toml, 1800 | `blocked (admission_wait_exhausted)` |
| `disk_wait_seconds` | cumulative time waiting for the disk floor | machine.toml, 600 | `blocked (disk_low)` |
| `total_seconds` | end-to-end from run start: baseline + work + all waits | new `limits.total_seconds`; default = Σ baseline `timeout_seconds` + `work_seconds` + both wait budgets | `budget_exhausted (total)` |

Waits never consume `work_seconds`; they consume their own budget and `total_seconds`. Every command is started with `timeout = min(own timeout_seconds, remaining of each budget that applies)`: baseline commands and base-tree setup → `total`; task-tree setup, candidate commands and review → `work` and `total`. The deadline handle (`runner/mod.rs:1720`) becomes a `Budgets` value passed to `verify` and the native adapter, driven by the injectable clock (the ledger already has one). A baseline that times out stays `BaselineUnrunnable` (blocked, not red); admission (B2) is what keeps it from timing out under load. **Load-scaled deadlines are deferred** (see Not in scope).

**B2. Heavy admission with weights, child parallelism and survival.**
- Every setup step and check (base and candidate) takes a `HeavyLocal` lease of `weight` tokens (check field, default 1, ≤ `max_heavy_commands`, which defaults to cores/4, min 1). Lease dispatch id `<run>-heavy-<phase>-<label>-<try>` (unique per base/candidate/retry, so C8 never refuses a rerun).
- **Child parallelism:** the admitted command's environment gets `n = max(1, weight × cores / max_heavy_commands)` through `MAKEFLAGS=-j<n>` (make creates the jobserver; cargo and rustc join it), `CARGO_BUILD_JOBS=<n>`, `RUST_TEST_THREADS=<n>`, `NEXTEST_TEST_THREADS=<n>`, unless the profile overrides them per check (`env`). This is a bound on parallelism, not a CPU or memory cap; documented as such.
- **Survival:** the lease is tied to the check's process group. The runner reports `pgid` to the coordinator at spawn; if the runner dies, the coordinator sends SIGTERM to the group, SIGKILL after 10 s, and releases the tokens only once `kill(-pgid, 0)` returns `ESRCH`. Capacity is never returned while descendants run.
- **One host queue with amont:** machine.toml `[resources] shared_host_slots = "<dir>"`; when set, a weight-w lease takes w `flock` slots all-or-nothing (`LOCK_NB`, release all and back off on a partial set), after the coordinator token, with the slot fds inherited (no `CLOEXEC`) by the check's process group so slots outlive the runner exactly as long as descendants run (amont ADR-0009's protocol), so amont's heavy checks and relais's share one queue; the check environment gets `AMONT_HOST_SLOT=held` so an amont nested in a check does not queue again. Unset → relais's coordinator queue only.
- Queued → `waiting_for_slot {class, position, waited_s}` every 30 s. Coordinator down: as `ModelWork` today (`runner/mod.rs:994-1026`) — no gate → unadmitted; gate + error → `blocked (admission_unavailable)`.

**B3. Disk floor with reservation.** `statvfs` **`f_bavail`** (bytes available to this user) on each volume the run writes: state dir, repository, worktrees, `TMPDIR`, identified by `st_dev`. Floor `[resources] min_free_bytes` (default 8 GiB). Check before the baseline and **again after each lease is granted** (closing the window between check and use). Each check carries a disk estimate (bytes the same label grew its tree by last time, recorded per label; 2 GiB default); the coordinator admits a heavy lease only if `f_bavail − Σ estimates of running leases on that volume ≥ floor + estimate`, otherwise it queues under `disk_wait_seconds` only (not also `admission_wait_seconds`; the coordinator names the holding constraint) with `waiting_for_disk {volume, available, floor, reserved}`. `relais doctor`: `disk: 5.2 GiB available on <mount>, floor 8.0 GiB, reserved 2.0 GiB (below floor)`.

**B4. Exhaustion is evidence, not a verdict.**
- **In a check's output** (failed command only, last 64 KiB): ENOSPC signatures (`No space left on device`, `os error 28`, `database or disk is full`), memory signatures (`Cannot allocate memory`, `os error 12`, a SIGKILL that relais did not send, with the OOM note when visible). Combined with a measured `f_bavail` below floor (disk) at failure time.
- **Outcome:** `needs_decision (resource_exhausted {resource, volume, available, floor, peak_rss?})`, candidate preserved, **no automatic repair**. The decision text says plainly that exhaustion is observed, not proof the code is innocent (a candidate can introduce the OOM or the disk blow-up), and offers: a `kind: verify` rerun intent of the same candidate (A5), or `revise`. On base: `blocked (baseline_resource_exhausted)`, never "red on base".
- **In relais's own I/O:** any `io::ErrorKind::StorageFull` or `SQLITE_FULL` in a log write, snapshot, ref update, patch write or ledger write stops new launches and kills running checks. relais keeps a 64 MiB **ballast file** in the state dir (created at start when space allows; after deleting it relais confirms an `f_bavail` rise, since APFS local snapshots can retain its blocks, and otherwise goes straight to the distinct-exit path); on StorageFull it deletes the ballast and persists, in order: the candidate ref (if a snapshot exists), the ledger transition `blocked (relais_storage_full {volume})`, the follow-up intent. If the repository volume is the full one and the ref cannot be written, the outcome says so and the tree is kept for `resume`. If persistence still fails, relais exits with a distinct code and a stderr line naming the volume; `relais resume <run>` reconciles from the last durable ledger state. The ballast is recreated on the next start with space.

### C. Verification evidence and telemetry (PR 3)

**C1. Pre-existing failures, guarded.** The per-test shortcut applies only when **all** hold: (a) both base and candidate produced **complete** JUnit for the check (parsed, testsuite totals consistent, report written after the command started, non-zero exit explained by ≥ 1 failing testcase); (b) candidate failing set ⊆ base failing set; (c) no failing test is named by an acceptance criterion (`find_named_test`, `verify/mod.rs:452-495`) and the contract does not name the check itself as a target. Then: no repair is spent, the run ends `needs_decision (baseline_failure_not_waived)` with both test lists. Anything else — partial/missing report, empty failure set with non-zero exit, a targeted test — follows today's machine, repairs included (this keeps contracts that fix existing failures working). The baseline cache gains the failing-test sets with a report-completeness flag (`BASELINE_MEANING_VERSION` 4); a v3 entry has none, so the shortcut does not apply (explicit fallback to today's rule).

**C2. One retry owner per execution context, every attempt visible.** A check may declare `retry = 1`. Under relais the check environment sets `RELAIS_CHECK=1`; the Makefile's #240 plugin rerun is skipped when it is set (CI, outside relais, keeps it). Each attempt logs separately (`<stem>.log`, `<stem>-retry1.log`; `verify/mod.rs:869` truncates today), and the receipt and pane list every attempt with its exit and log hash; fail-then-pass → pass marked `flaky`. Never retried: a failure classified as exhaustion (B4).

**C3. This repository's profile** (`relais.toml`): `make lint`, `make test`, `make msrv`, `make audit`, `make plugin` as separate checks, timeouts from one idle measurement each (recorded like the 614 s one), `make toolchain` as a setup step; `retry = 1` on `make plugin` only; `weight = 2` on `make test`; `junit` on the test checks where the tool emits it.

**C4. Heartbeat vs progress.** Two timestamps on every wait: `alive_at` (the runner's timer thread, `heartbeat` event through `Events::emit`, seq'd, to `events.jsonl` and stdout, every 60 s, joined when the wait ends) and `progress_at` (last observed progress: bytes appended to the check's log, a worker turn or tool event, or CPU time of the check's process group increasing between samples). `stalled` = `progress_at` older than the threshold, shown with which signal went quiet; a live heartbeat with no progress reads `alive, no progress for 14m`. Command deadlines (B1) apply regardless of heartbeats. `timeline.ts` gains reducer cases for `heartbeat`, `waiting_for_slot`, `waiting_for_disk`, `flaky`, `resource_exhausted`, rendered as text, never by colour alone.

**C5. Cost on the sequenced path, status as reconciliation.** `relais native usage` appends to `events.jsonl` as today; the live driver tails seq-less usage lines past its last offset (rebuilt from the last seq'd line on resume) and re-emits each once through `Events::emit` (seq'd, stdout). No new endpoint: `ipc.rs` is the coordinator's socket only (`coordinator/mod.rs:604`). The plugin reconciles from `relais native status <run>`: requests coalesced (at most one in flight per run), 5 s timeout, each response carries the last `seq` it read as its revision and is applied only if newer than the model's; the cost is replaced, never added; replay and reconnect converge to the ledger total. `usage_of_report` (`adapter/native.rs:317-353`): a model with no token fields → cost unknown, not 0.

## Failure cases

| Case | Handling |
|---|---|
| Candidate conflicts, `kind: verify` | `blocked (candidate_does_not_apply)` in preflight, before the baseline; no check runs. |
| Candidate conflicts, `from_candidate` | Tree left with the 3-way result; worker gets the conflict list; snapshot refused until resolved (`unresolved_conflict` = failed attempt). |
| Source run still running / unknown / other repo | Refused at validation (`candidate_source_running` / `candidate_missing` / `candidate_foreign_repo`). |
| Referenced candidate cleaned up | Pinned while referenced; the release path (`reap::release_candidates`) skips pinned refs and files. Nothing calls it yet: `workspace::retire` and reap do not delete candidate refs today (`holds-until:` a gc command). A digest mismatch at apply → `blocked (candidate_digest_mismatch)`. |
| Crash between decide and launch; launch succeeded but response lost; decide repeated | Intent row is the single source; reconciler launches `pending`; claim is atomic and returns the existing run; repeat decide returns the same intent. |
| Two runs pass the disk check, then both build | Reservation: admission subtracts running leases' estimates; recheck after grant. |
| relais's own write hits ENOSPC | Ballast freed, minimal state persisted in order; distinct exit + `resume` when even that fails. |
| Runner SIGKILLed mid-check | Coordinator kills the process group and releases tokens only when the group is gone. |
| Coordinator down | No gate → unadmitted; gate + error → `blocked (admission_unavailable)`. |
| Partial JUnit from a crashed check | Not complete → no shortcut; today's rule. |
| OOM caused by the candidate | `resource_exhausted` decision, evidence shown, not called innocent; orchestrator or person decides. |
| Signature text in a passing test's output | Only failed commands are scanned. |

## Changes

- PR 0: `adapter/claude.rs` (`ProbeFailure::TimedOut`, injectable `probe_timeout`); `native.rs` (`Support`, `claude_code_support`, wrappers); `main.rs` (`run_refusal` warn-and-continue for `run` and `dataset replay`); the event enum and the pane (`claude-plugin/hooks/timeline.ts`; new `environment_warning` kind, SPEC line); a `report` entry is deferred to PR 3; `doctor/mod.rs` (warning line); SPEC §5/§6 sentence on an unchecked version; CHANGELOG.
- PR 1: `contract/mod.rs` (`Task::Verify`, `CandidateRef`, `from_candidate`, refusals); `runner/mod.rs` (preflight application, verify path, conflict tree, snapshot refusal); `workspace/mod.rs` (`apply_candidate`, pin-aware retire); `reap.rs`; `ledger/mod.rs` (`candidate_inputs`, `successor_intents`, pins — expand-only steps); `main.rs` (`decide revise --widen-scope|--successor-contract`, `run --intent`); `learn/dataset.rs`, report; `claude-plugin/hooks/{register,runs}.ts`, `skills/relais/SKILL.md`; SPEC §4, §8, §9.
- PR 2: `runner/mod.rs` + `verify/mod.rs` + `adapter/native.rs` (`Budgets` replaces the deadline `Instant`; per-command timeouts; leases around commands; storage-full handling); `admission/mod.rs` + `coordinator/mod.rs` (weights, pgid-tied release, disk reservation, `shared_host_slots` flock); `procs.rs` (group liveness probe); `policy/mod.rs` (check `weight`, `env`; machine `[resources]`: `min_free_bytes`, `disk_wait_seconds`, `admission_wait_seconds`, `shared_host_slots`; contract `limits.total_seconds` — all `#[serde(default, skip_serializing_if)]`, new pin test); `lifecycle.rs` `Reason`s (refusals exit 2, blocks exit 3, README table); `main.rs` doctor; `.adr/records/0010-…md`, `.adr.yaml` key `execution.resource-budgets`, `aval heads --write`; SPEC §9, §10, §11, §23.
- PR 3: `verify/mod.rs` + `verify/junit.rs` (completeness, set comparison, retry attempts, cache v4); `runner/machine.rs` (guarded `preexisting` row); `runner/mod.rs` (heartbeat/progress); `runner/mod.rs` (driver re-sequences `native usage` lines); `claude-plugin/hooks/{timeline,pane,runs}.ts`; `relais.toml`, `Makefile` (`RELAIS_CHECK` guard); SPEC §10, §18, §29. CHANGELOG in each.

## Verification

`make check` exits 0 first. Tests use the injectable clock and failpoints (a `RELAIS_FAILPOINT=<name>` hook compiled only under `cfg(test)`/a test feature) rather than sleeps.

0. **Probe under load (PR 0).** Fake `claude` sleeping past a 200 ms probe timeout → `run_refusal` returns `Ok(None)`, stderr carries the warning once, the event log carries `environment_warning`. Fake `claude` printing `2.0.0` or `2.2.0` → still refused; exiting 1, printing nothing or not a version → still refused. Stale hello + timed-out probe → refused by the hello rule. `doctor` with the slow fake shows the warning line, not a failure. Live, before the push: `relais doctor` with `RELAIS_CLAUDE_BIN` pointed at a sleeping wrapper. Live, after the release is installed (`holds-until: the first install of v0.22.0`, the release that carries PR 0; not at PR 0's merge, because `relais run` is refused outside the plugin and a branch binary would migrate the real ledger): a `relais run` started from the plugin with `RELAIS_CLAUDE_BIN` a wrapper sleeping 35 s → exactly one warning line, one `environment_warning` event, dispatch within ~30 s of start, one `--version` process.
1. **Identity (PR 1).** Resolution persisted once: mutating `candidate-latest.patch` after validation → `candidate_digest_mismatch`; source run still running → refused; a release of a run whose candidate is pinned keeps its ref (`release_candidates`, not retire, until a gc command calls it); receipt carries all six identity fields.
2. **Conflicts.** `verify` with a conflicting candidate → blocked in preflight, zero baseline commands run (event log); `from_candidate` conflict → worker prompt lists the paths, snapshot refused while markers remain, accepted after resolution.
3. **Successor intent, crash injection.** Failpoints after the decide transaction, after the claim, and after launch with the response dropped: each recovery yields exactly one successor run (count runs per intent); repeated decide → same intent; concurrent claims (two threads) → one run.
4. **Budgets.** Deterministic clock: admission waits consume `admission_wait_seconds` and `total` but not `work`; exhaustion of each budget yields its named outcome; a candidate command's timeout is clipped to the remaining `work`; `limits.wall_seconds` caps `work` only; `limits.total_seconds` caps end-to-end.
5. **Admission.** `max_heavy_commands = 1`, two runs: commands never overlap (ordering asserted from lease events, not timestamps), child env carries `-j<n>`; SIGKILL the runner mid-check → tokens stay held until the process group is gone, then released (no overlap with the next command); `shared_host_slots` set → an amont-style flock holder delays the relais check; stress variant 20× in CI.
6. **Disk.** `statvfs` stub: below floor → `waiting_for_disk` then `disk_low`; two runs whose estimates do not both fit → the second queues; recheck after grant. StorageFull injected at each relais write site → ballast freed, ledger shows `relais_storage_full`, candidate ref present; injected at the ledger with ballast gone → distinct exit, `resume` reconciles.
7. **Exhaustion classification.** Fixtures per signature on a failing check → `resource_exhausted`, `repairs_used == 0`; on base → `baseline_resource_exhausted`; a relais-sent timeout kill is not memory; passing check with the text → pass.
8. **Pre-existing (PR 3).** Complete reports, subset, no targeted test → `needs_decision`, `repairs_used == 0`; same but a failing test named in acceptance → repair; truncated report / exit≠0 with zero failing cases → today's rule; v3 cache entry → today's rule.
9. **Retry ownership.** Under relais (`RELAIS_CHECK=1`) the plugin check runs at most twice in total (count invocations); both logs exist and both attempts are on the receipt; CI path (`RELAIS_CHECK` unset) keeps the Makefile rerun.
10. **Progress/cost.** A check printing nothing but burning CPU → `alive`, not stalled; a check sleeping without output or CPU past the threshold → `alive, no progress`; heartbeat seq monotonic. Plugin: out-of-order status responses → newer revision wins; replay twice → `¢69.20 est`; a seq-less usage line appears on stdout as one seq'd `cost`, exactly once across a resume.
11. **Live (after release and install, this machine):** `relais --version` = the PR 3 release (v0.25.0 at this writing); a `kind: verify` run of `run-65dc62a01e3d1-bf16`'s candidate (the advisor-token fix) reaches `accepted` or a named outcome with no worker; with `shared_host_slots` set to amont's slot directory and `max_heavy_commands = 1`, while a second relais run or an amont Heavy check holds the slot, the pane shows `waiting_for_slot` and lease events show no overlap; `df` before/after within the ADR-0008 bound; pane cost equals the ledger total.

## Outcome (PR 0)

Branch `fix/probe-timeout-not-unsupported`, salvaged from relais run `run-65dd2b932b305-17a47` (`scope_exceeded`: my contract named `claude-plugin/test/**`, the plugin test lives in `claude-plugin/tests/`).

| Check | Input | Expected | Actual |
|---|---|---|---|
| Typed timeout, retry | fake `claude` sleeping 5 s, 200 ms timeout | `TimedOut`, `Unchecked`; `probe_report` then `probe` start the binary once; with retry 0, twice | as expected (`adapter::claude` tests) |
| Other failures | `2.0.0`, `2.2.0`, unparsable, empty, exit 1, silent, not run | `Unsupported`; exit-1 probe cached (1 start) | as expected |
| `run_refusal` | timed-out probe, stale then fresh hello | stale → hello refusal; fresh → `Ok(None)` | as expected (`relais` bin tests) |
| Runner reading | `harness_reading(None, Some(timeout))` | `UnknownTimedOut` + one `environment_warning`; an answer outranks a timeout | as expected |
| Pane | `environment_warning` event | a text line `warning · claude_code_version_unchecked · …` | as expected (`status.test.ts`) |
| Live `relais doctor` | `RELAIS_CLAUDE_BIN` = 35 s sleeper; `uptime` at 22:46 just before: load 23.61 | warning line, not a failure | `! claude-code  version unchecked (timed out after 30s) — runs will warn, not refuse`, exit 0, 100 s (doctor builds two backends: `--version` started twice, pre-existing) |
| Gate | `git commit` | 30 checks | 30/30 passed |

Deliberate: the live `relais run` check (35 s sleeper → one warning, one event, one `--version`) runs at the first install of v0.22.0 (v0.21.0 was cut from #242–#244 on 2026-10-14, so this plan's release is v0.22.0): `relais run` is refused outside the plugin and a branch binary would migrate the real ledger. The warning is not added to `report`: it is in `events.jsonl`, the pane and the manifest; a report entry is left to PR 3.

## Outcome (PR 1)

Branch `feat/candidate-identity`, built by five relais runs (three salvaged after `scope_exceeded` / `review_unavailable` / `interrupted`, one accepted, one salvaged after `needs_review`) and reviewed twice. Tests named below run in `cargo test`; the gate passed 30/30 at each commit.

| Verification | Input | Expected | Actual (test) |
|---|---|---|---|
| 1 Identity | a clean verify of run X's candidate | the six identity fields persisted once in `candidate_inputs` and on the receipt | `a_clean_verify_dispatches_no_worker_and_persists_the_resolved_candidate` |
| 1 | patch changed after validation | `candidate_digest_mismatch` | `a_patch_changed_after_validation_ends_candidate_digest_mismatch` |
| 1 | source running / no candidate or commit gone / other repo | `candidate_source_running` / `candidate_missing` / `candidate_foreign_repo` | `a_source_that_has_not_ended_is_refused`, `a_source_without_a_candidate_or_without_its_commit_is_missing`, `a_source_from_another_repository_is_refused` |
| 1 | a pinned candidate released | its ref and patch are kept while a non-terminal run or pending intent refers to it | `a_pinned_candidate_survives_release_while_its_consumer_is_running`, `a_pending_intent_pins_its_candidate_and_a_finished_run_releases_it` (release path only; nothing calls it yet, `holds-until` a gc command) |
| 1 | stored row resolved against another base; user `diff.noprefix` | refused in preflight; digest unchanged | `a_stored_row_resolved_against_another_base_is_refused_in_preflight`, `a_user_diff_config_does_not_change_the_candidate_digest` |
| 2 Conflicts | verify, conflicting candidate | `blocked (candidate_does_not_apply)`, no baseline event | `a_conflicting_candidate_is_blocked_before_the_baseline` |
| 2 | `from_candidate`, conflicting | worker prompt lists paths; snapshot refused until resolved | `a_conflicting_from_candidate_hands_the_merge_to_the_worker_and_refuses_the_snapshot_until_resolved` |
| 3 Successor intent | decide repeated; concurrent claims | one intent; one run | `the_answer_and_one_intent_are_written_together_and_repeat_to_the_same_intent`, `two_concurrent_claims_yield_one_run` |
| 3 | crash after decide / after claim / after launch with the response dropped | exactly one successor run per intent | `a_crash_after_the_decide_commit_still_yields_one_successor_run`, `a_crash_after_the_claim_still_yields_one_successor_run`, `a_launch_that_dies_before_its_id_is_returned_leaves_exactly_one_successor_run` (counts run rows independently of the claim: two launched, one abandoned, none for the refused third) |
| 3 | refused launch | claim handed back; not relaunched inside a 60 s, doubling to 10 min, backoff; `native status` lists refusal and `retry_after` (parsed by bun and node) | `a_refused_launch_hands_the_claim_back_with_the_refusal_recorded`, `a_refused_intent_is_not_launchable_inside_its_backoff_and_is_after_it`, `a_run_intent_refused_by_a_preflight_leaves_the_intent_pending_with_the_refusal` (real binary) |
| A5 plugin | status poll relaunches a dead successor; errors typed | one launch per poll while it lives; typed failure on bad status | `claude-plugin/tests/successor.test.ts` |

Deliberate: pins protect the release path only (no command calls it yet). The live step (`kind: verify` of a real candidate) is plan step 11, after the release.

## Implementation review

PR 0: approve, four passes on one reviewer (round 1 50k/77 s; delta 56k/24 s; comment-only 58k/18 s; Windows CI fix 61k/17 s); final tree `87456d3f`, nothing open.
CI: the first push failed on `windows-latest` (the fake-`claude` test helper used `std::os::unix`); the helper and its three tests are now `#[cfg(unix)]`.
Fixed: typed `TurnCeiling::UnknownTimedOut`; pure `harness_reading` with its own test; §0.4 and D12 written down; two inaccurate doc lines; Verification 0 split, load figure sourced.
Deliberate: the live `relais run` check waits for the first install of v0.22.0 (`holds-until` in Verification 0); the `report` entry is deferred to PR 3.
Next: PR 1 (candidate identity), on a new branch from `origin/main` after this merges.

PR 1: approve, after two rounds and four deltas on one reviewer (round 1 on the pre-crash-fix tree, 72k/87 s; round 1 on the rebased tree, 68k/68 s; deltas 82k/40 s, 86k/11 s, Windows CI fix 87k/12 s); final tree `704398c8`, nothing open.
CI: the first push failed on `windows-latest` in two tests (a CRLF checkout compared byte for byte, and a tamper step whose backslashed path `sh -c` misread); both fixed in the tests, no production code changed.
Fixed: crash-injection failpoints (Verification 3); relaunch on every poll with a refusal backoff (60 s doubling to 10 min); typed reconciler failures; a claim handed back on refusal and on errors before the run row; abandoned claims by pid start time; a read error in the conflict-marker check is returned, not read as "no marker".
Deliberate: pins protect the release path only (nothing calls it yet; `holds-until` a gc command). Next: PR 2 (resource admission), on a new branch after this merges; its ADR is ADR-0010, since #248 took 0009.

## Outcome (PR 2)

Branch `feat/resource-admission`, built by five relais runs (B1 salvaged after `scope_exceeded` on compile-forced fixtures; B2 salvaged after the coordinator restarted mid-run under a 0.24.1 upgrade; the B2 fixes salvaged after `review_unavailable`; B3 salvaged after `budget_exhausted`; B4 salvaged after `interrupted`) and two early reviews of B2. The gate passed 30/30 at each commit.

| Verification | Expected | Actual (test) |
|---|---|---|
| 4 Budgets | waits consume their own budget and total, never work; each budget ends under its own name; a candidate command clipped to work; `limits.wall_seconds` caps work, `limits.total_seconds` the run | `runner/tests/budgets.rs`: `a_queue_does_not_spend_the_work_budget`, `each_exhausted_budget_ends_the_run_under_its_own_name`, `the_total_ends_a_queued_run_before_its_wait_budget_does`, `wall_seconds_caps_the_work_and_nothing_else`, `total_seconds_caps_the_run_end_to_end` |
| 5 Admission | with one heavy slot, two runs never overlap (lease-event order); child env carries `-j<n>`; a dead runner's tokens return only when its group is gone; a held flock slot delays the check | `verify/heavy/tests.rs`: `with_one_heavy_slot_two_runs_checks_never_overlap`, `an_admitted_check_carries_parallelism_bounds_and_its_own_env_wins`, `a_check_waits_for_a_held_host_slot_and_runs_holding_it`; `coordinator`: `a_dead_runners_group_is_stopped_and_its_tokens_return_only_when_it_is_gone`, `release_keeps_the_tokens_while_a_descendant_runs_and_the_sweep_returns_them_after`, `a_recycled_leader_pid_releases_the_tokens_without_signalling_anything`, `a_hold_bound_before_a_restart_is_still_held_and_reconciled_after_it`, `a_gone_group_returns_its_tokens_while_its_runner_lives` |
| 6 Disk | below the floor → `waiting_for_disk` then `disk_low`; two runs that do not both fit → the second queues; recheck after the grant; StorageFull at each write site → ballast freed, `relais_storage_full`, candidate ref present; at the ledger with no ballast → distinct exit, `resume` reconciles | `a_disk_below_the_floor_is_waited_out_under_the_disk_budget_and_ends_disk_low`, `a_second_run_that_does_not_fit_above_the_floor_queues_until_the_first_lease_is_released`, `the_floor_is_checked_again_after_the_grant_and_the_command_queues_instead_of_running`; `runner/tests/exhaustion.rs`: `storage_full_at_each_write_site_frees_the_ballast_and_records_it`, `storage_full_at_the_ledger_with_the_ballast_gone_exits_distinctly_and_resume_reconciles`, `a_ballast_that_frees_nothing_goes_straight_to_the_distinct_exit` |
| 7 Exhaustion | each signature on a failing check → `resource_exhausted`, no repair; on base → `baseline_resource_exhausted`; relais's own timeout kill is not memory; a passing check with the text passes | `each_disk_signature_in_a_failed_candidate_check_ends_resource_exhausted`, `each_memory_signature_and_an_unsent_sigkill_end_resource_exhausted`, `a_classified_failure_on_the_base_blocks_the_run_and_never_reddens_the_base`, `a_timeout_relais_sent_is_not_memory`, `a_passing_check_that_prints_a_signature_is_still_a_pass` |

Deliberate: a weight above the number of host slot files is refused when the first heavy lease is taken, not at preflight, so a base setup can run first; an untied lease (its bind failed) is not persisted across a coordinator restart (SPEC §23 says so); the stress variant of item 5 (20× in CI) is not added; the live check with amont's real slot directory is plan step 11, after the release. Found on the way: this machine's disk stood at 4.1 GiB free, under the new 8 GiB floor; the release scenarios now run with no floor (`NO_DISK_FLOOR`), the floor itself tested with a stubbed probe.

## Delivery

**PR 0** (§0, small, independent) merges first, so the remaining work is not itself blocked by a loaded machine. Then three PRs, merged in order PR 1 → PR 2 → PR 3 (B4 needs A5, C2 needs B4; later PRs say `merge-after:`), each mergeable on green CI and an approved implementation review: **PR 1** candidate identity and revision semantics (A); **PR 2** resource admission, budgets and exhaustion (B, ADR-0010); **PR 3** verification evidence and telemetry (C). The plan record landed as PR 0's first commit (ADR-0022). PR 0 shipped in v0.22.0 (#250). One minor release after PR 3 carries the rest (expand-only ledger steps); its number is the next free one, v0.25.0 at this writing, since other work cut v0.21.0–v0.24.0 on 2026-10-14/15. Install on this machine (`relais install --claude --user --write`, `relais coordinator stop`), which also brings v0.20.0's CoW seeding and #240; then live step 11. Rollback: a relais from before schema 35 refuses an upgraded ledger (`SchemaAhead`, `ledger/mod.rs`), so a rollback restores the ledger from before the upgrade; it does not run old binaries against new tables. The pending `run-65dc815630ab3-117d3` decision is recorded `abandon` (superseded by step 11's verify run), and the advisor fix already landed as #246 (2026-10-14).

## Not in scope

- **Load-scaled deadlines** (deferred until the B1 accounting has run in practice; scaling the work deadline cannot rescue a baseline that already timed out, and admission addresses the cause).
- CPU or memory caps per check (cgroups/`setrlimit`); B2 bounds parallelism only.
- Per-test results for checks that emit no JUnit.
- A workflow platform; the patterns (outbox, content-addressed inputs, jobserver tokens, split timeouts) are implemented on the existing ledger and coordinator.

## Decision log

- D1 Candidates are named by run (and optional index), resolved once to a content-addressed commit plus digests, pinned while referenced; never by path.
- D2 Conflicts: `verify` blocks in preflight; `from_candidate` hands the merge to the worker with mandatory resolution before snapshot.
- D3 Successor creation is a transactional outbox row with the decision as idempotency key; only the plugin launches; claims are atomic.
- D4 Budgets are split (work, admission wait, disk wait, total); `limits.wall_seconds` keeps capping work; `limits.total_seconds` is new.
- D5 Exhaustion (disk, memory) is evidence leading to a decision, never a repair and never "red on base"; relais's own StorageFull is handled with a ballast.
- D6 (settled by ADR-0004, person's review 2026-10-14) The orchestrator may answer `revise --widen-scope` like any answer, recorded with actor and exact scope delta; the advisor only advises.
- D7 One retry owner per execution context (`RELAIS_CHECK`); every attempt is visible.
- D8 Pre-existing shortcut only on complete reports, subset failures and no targeted test.
- D9 Defaults: disk floor 8 GiB, disk wait 600 s, admission wait 1800 s, `max_heavy_commands` cores/4 (min 1), ballast 64 MiB, disk estimate 2 GiB until measured.
- D10 Delivery split into three PRs, one release (person's review); PR 0 added ahead of them and shipped on its own in v0.22.0; PRs 1–3 follow in one release.
- D11 A probe that times out is "unchecked", not "unsupported": warn, record, continue; a version that answers outside the range, or no answer at all, still refuses. No cross-process version cache.
- D12 (PR 0 implementation) A timed-out probe stands for `TIMEOUT_RETRY` (300 s) on the backend rather than being re-asked at every use, so a run start waits once, not once per caller; the timeout is read through `Harness::probe_timed_out`, never a process-wide static. `Support`/`claude_code_support` live in `adapter/claude.rs`, since `native` must not depend on `adapter` (module-cycle check).

<!-- panel: repos=relais reviewers=backend,lang:rust,tui,unix body-sha=29618604254a -->
