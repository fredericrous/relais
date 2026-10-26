---
status: active
branch: perf/own-tree-seed-wait
repos: [relais]
adrs: [ADR-0009]
---
# relais seeds its own tree without the 30 s wait

## Review panel

👉 **Decide:** none. You chose "Trust the end signal". Approve if skipping the wait for the run's own tree, with partial-seed tests gating the release, is the trade you want.
📍 relais · plan revised after a measurement (a 30.1 s still gap mid-build ruled out a short window) · next: Phase 2 through relais. Panel: backend (opus-5-5), codex (gpt-6-astra, on the first draft).
**Changed by review:** the window is picked by call site, not by path; partial-seed tests gate the release; the claims are worded as best-effort; positive `workspace_seeded` and contents evidence.
📄 Full reviews: [2026-10-26-relais-seeds-its-own-tree.reviews.md](2026-10-26-relais-seeds-its-own-tree.reviews.md)
**Verdicts:** backend approve-with-changes in 4 rounds (the last a delta); codex approve-with-changes. 4 delta findings carried into implementation, listed under Full reviews.

## Context

relais#304. Before a fork or in-place run clones a dependency directory
(`target/`, `node_modules`) copy-on-write, `setup_gate::seed_tree`
(`crates/relais/src/runner/setup_gate.rs:45`) calls
`reuse::wait_until_quiet` (`workspace/reuse.rs:342`) with `QUIET_FOR = 30 s`,
`POLL = 5 s` and `GIVE_UP = 60 s` (`setup_gate.rs:24-26`). The wait is a
politeness toward an install that is still running.

Measured on this machine's recorded runs (`~/.local/state/relais/runs`,
2026-10-26):

- **144 runs waited.** 140 of the 141 counted first waited exactly 30 s and
  then seeded. So the tree was already still, just younger than 30 s.
- **Where:** 161 of 178 waiting episodes came after `verification_started`,
  while seeding a verification tree from the run's own task tree. The other
  17 came before any phase: the base seed from the caller's workspace.
- **What was changing:** `target` in 426 of the 444 waiting decisions.
- **Cost:** time from `verification_started` to the first verification
  check had a median of 30.9 s and a p90 of 232 s, over n = 361
  verification starts.
- **Worker kind:** 126 of the 144 waiting runs used native
  (plugin-dispatched) workers.

**A shorter window cannot fix this.** A `cargo build --tests` of relais
(193 s) left the depth-2 view of `target/` still for 13.4 s and then 30.1 s
while the build was still running: long rustc and link steps write deeper
than `tree_state` looks (`reuse.rs:304`). No timestamp window under about
30 s can tell a busy build from a finished one.

**What relais already knows.** At the two call sites that seed from the
run's own tree (`verification.rs:568`, `holdouts.rs:140`), every worker
attempt has ended. `attempt.rs:539` runs `wait_for_writers` before
`snapshot_tree`, and `launch.rs:310` releases the write lease once the
process has ended. relais's own setup steps have ended too.

## Goal

Seeding a verification or holdout tree from the run's own tree no longer
waits, because the run knows its writers have ended. Seeding from the
caller's workspace keeps the 30 s politeness.

## Non-goals

- The caller-workspace seeds (`baseline.rs:85`, `engine.rs:677`, both from
  `repo_dir`) keep `QUIET_FOR = 30 s`.
- No change to which setup steps run. Seeding stays an optimisation
  (ADR-0009, `workspace.local-reuse`).
- No new process tracking.

## Decisions

- **The person's choice (2026-10-26): trust the end signal.**
  - `seed_tree` takes a `SeedWait`:
    - `QuietFor(Quiet)`: today's behaviour, `CALLER` = 30 s / 5 s / 60 s;
    - `WritersEnded`: skip `wait_until_quiet`.
  - `setup_tree` takes the `SeedWait` from its caller, never by comparing
    paths (in place, `task_tree()` == `repo_dir`, `workspace_ops.rs:287`):
    - `verification.rs:568` and `holdouts.rs:140` pass `WritersEnded`;
    - `baseline.rs:85` and `engine.rs:677` pass `QuietFor(CALLER)`.
  - `source_is_free` and the post-clone re-check (`setup_gate.rs:56`, `:94`)
    stay for both. Another in-place relais run is still respected.
  - A `WritersEnded` seed records a decision `workspace_seed_unwaited` with
    the reason "the run's own writers ended", so Phase 3 can count it.
- **What this trusts, stated plainly.** It is best-effort, like the
  timestamp it replaces, which a 30.1 s pause in a build already fools.
  - A headless worker or a relais setup step cannot outlive its exit: the
    process group is killed (`procs.rs:925-935`).
  - A native worker ends at `SubagentStop`. A background Bash child it
    started (`cargo build &`) is not tracked and could still be writing.
  - In place, the person can write their own worktree at any time.
- **Correctness boundary and availability risk.**
  - Seeding does not authorize skipping setup; a skip still needs a proved
    `reuse` stamp.
  - Running setup does not repair every partial seed. A setup failure
    becomes a verification gap with no clean retry
    (`verification.rs:584-598`).
  - So Phase 2 gates the release on partial-seed tests, and Phase 3 watches
    setup gaps. A seed-related regression restores the wait.

## Phases

- [x] Phase 1 — relais: this plan in `docs/plans/` (replaces the first
      draft committed on this branch).
- [x] Phase 2 — relais: `SeedWait`, through relais.
  - Files: `runner/setup_gate.rs`, `runner/workspace_ops.rs`, and the four
    callers `runner/verification.rs`, `runner/holdouts.rs`,
    `runner/baseline.rs`, `runner/engine.rs`.
  - Tests (`runner/tests`):
    - **Own tree.** A task tree whose `node_modules` was written just
      before verification. Expected: no `workspace_seed_waiting`; one
      `workspace_seed_unwaited`; and `workspace_seeded` with the freshly
      written contents present in the verification tree. On a filesystem
      without copy-on-write, `workspace_cow_unavailable` and a full setup
      instead.
    - **In place.** An in-place run's base seed still gets
      `QuietFor(CALLER)`.
    - **Caller seed.** A fresh base source records
      `workspace_seed_waiting`, through `wait_until_quiet`'s injected sleep.
    - **Partial seed.** A deliberately partial `node_modules` with a
      setup step that repairs it: setup completes, as from a clean tree. A
      partial seed whose setup step fails: the run records a setup gap and
      no verification `check_started`.
    - All assertions are on decisions, contents and outcomes, never on wall
      time.
  - Remove `quiet_dependencies_worker` (`runner/tests/mod.rs`) where the
    own-tree seed no longer needs it.
  - No event marked a verification setup gap (none in 373 recorded
    verification starts), so Phase 2 emits a `verification_setup_gap`
    decision. Its baseline count therefore starts at this release.
- [ ] Phase 3 — relais: release, and observe.
  - Cut the next relais release (`tag-release`) and install it.
  - Over the next 30 real runs, against the figures in Context:
    - `workspace_seed_waiting` after `verification_started` (expected 0);
    - `workspace_seed_unwaited` count;
    - median and p90 from `verification_started` to the first verification
      check (before: 30.9 s and 232 s);
    - setup gaps per verification start (the `verification_setup_gap`
      decision).
  - If setup gaps rise and trace to a seed, restore the wait. Otherwise
    close relais#304 with the numbers.

## Verification

Phase 2, observed before the push:
- relais run `run-65ebf8aa7b2fa-b789` passed lint and the full `make test`
  (497 s). Its budget ran out during msrv; `make msrv` run by hand: green,
  233 s.
- `cargo test --lib -- runner::tests::a_fork_ … a_partial_seed`: 8 passed,
  in 4.6 s.
- The partial-seed tests no longer return early without copy-on-write, so
  they gate the release on every runner.
- Without copy-on-write: the four seed tests run with `TMPDIR` on a
  temporary HFS+ volume, where `clonefile` fails "Operation not supported"
  (APFS: ok). 4 passed. Without a clone the gap test is blocked at the base
  setup (`VerificationSetupFailed`), as the implementation review found.

- **Phase 2:** `cargo test --lib runner::tests` passes with the four tests
  above. `make check` green locally and on CI (ubuntu, macOS, Windows).
- **Phase 3:** the counts above, read from the `events.jsonl` of runs
  started after the install, and posted in relais#304.

<!-- panel: repos=relais reviewers=backend,codex body-sha=41f6e915fb81 -->
