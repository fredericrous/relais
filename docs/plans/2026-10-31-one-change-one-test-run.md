---
status: active
branch: chore/relais-profile-drop-msrv-audit
repos: [relais, amont]
adrs: [decisions:ADR-0009, decisions:ADR-0041, amont:ADR-0013, amont:ADR-0014]
---
# One change, one test run: cut duplicated checks on the laptop

## Review panel

📄 Full reviews: [2026-10-31-one-change-one-test-run.reviews.md](2026-10-31-one-change-one-test-run.reviews.md)
👉 **Decide:** nothing is needed now. Approve if dropping msrv and audit from relais is fine; the cost check decides the formula for A2.
📍 relais, amont · planned, nothing changed yet · next: A1 PR in relais. Panel: backend, rust, architect, po, tui, unix (all opus-5-5); codex not installed.
**Changed by review:** relais and amont already share one slot queue; A2 covers weight, all three spawn sites, unqueued runs and `MAKEFLAGS`; the cost check compares cap-based and held-based width.
**Verdicts:** 6 approve-with-changes in round 1, backend twice approve-with-changes in round 2. 3 medium findings carried into implementation: slots off → set nothing; optional `jobs` with a new function beside `record_wait`; verification values written for both formulas.

## Context

The laptop runs at a load average of 40–160 on 12 cores. For one change to the relais repository, the same tree is tested up to four times on the laptop, then again in CI:

1. **relais baseline.** relais runs its whole profile on the base tree: `make lint`, `make test`, `make msrv`, `make audit` and `make plugin`. It skips this only when the base is cached green (`cache_baseline`).
2. **relais candidate.** The same profile runs on the candidate, once per attempt (`max_attempts = 3`), plus any holdout commands.
3. **No stamp for amont.** relais commits with hooks disabled (`crates/relais/src/repo.rs:431`, `:472`, `:513`, `core.hooksPath` pointed at an empty dir). amont therefore never stamps the tree. relais can read an attestation (`amont attest covered`, `verify/mod.rs:2481`) but never writes one, and amont's `attest` has no way to record one.
4. **Push reruns the suite.** amont's pre-push `cargo-test` finds no stamp and runs `cargo test` again in a cold snapshot under `/var/folders/.../amont-push-*`. amont's `adr-lint-cli-probe` (warn-only, `heavy`) also builds there.
5. **CI runs it all again.** It runs lint, test on ubuntu, macOS and Windows, msrv, plugin and audit. Only one test leg can skip, and only through a signed attestation.

On top of the duplication, the parallelism is oversubscribed:
- amont runs up to `cores/2 = 6` heavy slots. Each holder runs cargo at its default width, `-j12` with 12 test threads, because amont sets no bounds.
- relais does bound its own checks (`verify/heavy.rs:210-226`: `MAKEFLAGS`, `CARGO_BUILD_JOBS`, `RUST_TEST_THREADS`, `NEXTEST_TEST_THREADS` = weight × cores / cap).
- relais and amont already share one slot queue (`amont/docs/host-slots.md:6-8`; relais links `amont-slots`, `heavy.rs:959`). Slots do not stack across tools; the oversubscription is that each amont holder runs at full width while up to `cores/2` slots are held.

The goal is fewer duplicate runs and bounded width now, then one runner: amont runs the gates, and relais, the push and CI reuse its verdict.

Non-goal of Part A: the pre-push `cargo-test` rerun after a relais run stays until Part B tickets 1+2 ship.

## Part A: now (two small changes, each its own PR)

### A1. relais: drop `make msrv` and `make audit` from relais's verification profile
- **Edit:** `relais/relais.toml`. Delete the two `[[verification.profiles.default.commands]]` blocks. Update the comment above the profile: it covers `make check`'s gates minus msrv and audit, which CI runs on every PR (`ci.yaml` jobs `msrv` and `audit`).
- **Why msrv:** it is a full second build of the whole graph on another toolchain, 152 s measured and up to 900 s allowed, run per candidate and per uncached baseline. A worker's change can break MSRV (a newer std API or syntax). `make lint` catches the API case through clippy's `incompatible_msrv` under `-D warnings`; CI's `msrv` job catches syntax before merge.
- **Why audit:** it is advisory and never fails, so it can say nothing about a candidate. Dropping it costs nothing.
- **Unchanged:** `make check` and CI keep all five gates.
- **Trust:** the policy hash changes. If relais asks for trust again, the run refuses with a message naming `mcp__relais__trust`, the person grants it there, and the A1 PR description gives that step.

### A2. amont: bound the width of every heavy-slot holder
- **Edit:** one helper beside amont's `HELD_ENV` export, called at all three spawn sites: `hooks/common.rs` `status_within` and `run_observed`, and `slot_cmd.rs` `spawn`. It bounds every heavy check and every `slot run` child, including one that runs unqueued after a slot-wait timeout (`host_slots.rs:289`, `slot_cmd.rs:192`), because that case happens at peak load. It sets `MAKEFLAGS=-j{n}`, `CARGO_BUILD_JOBS={n}`, `RUST_TEST_THREADS={n}` and `NEXTEST_TEST_THREADS={n}` with `n = max(1, weight × cores / cap)`: `weight` is 1 for hook checks and `--weight` for `slot run`, and `cap` is `Sizing::cap()` (it also covers `hostSlots N`).
  - A key that `cmd.get_envs()` or the parent's environment already sets to a non-empty value is kept, so a check's own value and a relais parent's bounds both win.
  - An empty value counts as unset.
  - When `MAKEFLAGS` is set without `-j` or `--jobserver-auth`, ` -j{n}` is appended to it.
- **Shared code:** move `parallelism` and `child_environment` from `relais/crates/relais/src/verify/heavy.rs` into the `amont-slots` crate. amont calls them, and a relais follow-up switches to the crate's functions, so both tools use one formula.
- **Visible width:** the holder record (`holders/slot-N.who`) and `waits.jsonl` gain `jobs`, and `amont slots` shows it per holder (text and `--json`), so a slower gate says why.
- **Test:** an integration test in `tests/it/slot_run.rs` runs `amont slot run -- env` and checks:
  - the four keys are at `weight × cores / cap`, for weight 1 and weight 3;
  - a preset key comes through unchanged, and an empty one is replaced;
  - `MAKEFLAGS=--no-print-directory` gains ` -j{n}`;
  - a heavy hook check that times out unqueued still gets the keys.
- **Cost check before release:** time a cold pre-push `cargo-test` on relais at the default width and at `-j2` (a lone holder on an idle host gets 2), and record both. Also time it at the held-based width, `n = max(1, weight × cores / slots held after the grant)`: a lone holder gets 12, and six staggered arrivals sum to about 12 × H6 ≈ 29 jobs, against 72 today. Pick the formula from the three numbers before release. If cap-based `-j2` is more than 25% slower or exceeds half the check ceiling, ship held-based, and note in the relais follow-up that relais's cap-based `parallelism` now differs.
- **Docs:** `amont/docs/host-slots.md` and `configuration.md` (the slot-count section) gain one paragraph on width, stating the precedence once: the check's own env, then the parent's env, then amont's computed width.
- **Release:** an amont minor release with the `tag-release` skill, then `amont install` on the laptop.
- **Who does it:** the work runs in a worktree from `origin/main` (`worktree-task`) and is routed through relais. relais's own verify of amont is the check, which is bounded already.

## Part B: plan "amont is the only runner" (a programme map, not code yet)

This spans relais, amont, attest and CI, and its decisions cannot all be stated now, so it gets a programme map (`relais:programme`), charted in `decisions`. Proposed tickets:

1. **amont records a verdict any runner can earn.** Add `amont attest record` (or `amont run --stamp`), which runs the named gates on a tree and stores the stamp the pre-push and the CI `attest` action already honour. Open decision: who signs a locally earned stamp so that CI trusts it.
2. **relais verifies through amont.** For a repository with `integrations.amont = "required"`, relais's profile names amont gates instead of argv. It runs them through amont in the candidate tree, which stamps it. A green relais run then means the push skips `cargo-test`, and one CI test leg skips.
3. **No relais baseline rerun of a proven base.** relais asks `amont attest covered` on the base tree first. A base that main's CI or a stamp already proved is not rerun.
4. **Retries rerun only what failed.** A retry after a repair reruns the failed checks plus anything whose inputs the repair touched, not the whole profile.
5. **relais CI follows ADR-0041 (`ci.platform-schedule`, active).** A PR proves the primary platform (macOS) only. Linux and Windows run on main after the merge, or when the PR touches platform code (`ipc`, `procs`). Today relais runs all three on every PR.

### Waste audit (ticket 0, done first, read-only, routed through relais as a research task)
Measure, don't guess. For one week of ledger data plus `amont slots` samples, report:
- **Load attribution:** which share of CPU at peak is relais checks, amont hooks, vitest and Playwright, the 38 `npm exec` (MCP servers per Claude session), rust-analyzer and Chrome. Read `ps` samples during peaks.
- **Cold builds:** whether relais verify dirs (`~/.local/state/relais/verify/run-*/verify-1/target`, 67 dirs, 2.3 GB) start seeded or cold, and whether amont's push snapshot builds cold every time.
- **Unmeasured costs:** how often baselines are uncached, and how many attempts and holdouts each run uses.
- **cache_baseline coverage:** repos with `cache_baseline` off and a deterministic profile, deduped (worktrees excluded).
- **Duplicate gates:** checks run by both relais's profile and amont's hooks (`plugin-test`, `module-cycles`, `cargo test`).
- **Cleanup:** stale verify dirs and push snapshots that are never removed.

## Verification
- **A1:**
  - `relais plan` on a trivial contract in relais lists three checks (`make lint`, `make test`, `make plugin`), not five.
  - A real relais run shows no `make msrv` or `make audit` in its log dir.
  - CI's msrv and audit jobs still run on the PR.
- **A2:**
  - On 12 cores with cap 6, `amont slot run --weight 2 -- env` shows `CARGO_BUILD_JOBS=4`. With `CARGO_BUILD_JOBS=9` preset, it shows 9.
  - After `amont install`, `ps eww` on the child cargo of a pre-push `cargo-test` shows `CARGO_BUILD_JOBS=2` (12 cores / 6 slots, weight 1), and `amont slots` shows `jobs 2`.
  - Measure one week before A1+A2 and one week after:
    - relais candidate wall time p50 from the ledger;
    - `waits.jsonl` `waited_ms` p50/p90;
    - median and p90 `load1`.
  - Report the numbers. Success means they fall. Roll back A2 if a lone-holder, idle-host pre-push `cargo-test` gets more than 25% slower than its pre-A2 time.
- **B:** the waste audit report is attached to ticket 0 and ranks tickets 1–5. Tickets 1+2 ship first and alone. They are accepted when, after a green relais run, the push log shows `cargo-test` skipped by a stamp and the CI `attest` step skips one test leg.

## Verification record

- **A1** (`chore/relais-profile-drop-msrv-audit`, 6ecb0b4e):
  - Profile commands → expected lint, test, plugin → actual `[make lint] [make test] [make plugin]`, read from relais.toml with `tomllib`.
  - `relais plan --task <noop contract> --json` → policy accepted → exit 0, policy_hash `df6b1095…`.
    - The plan said `relais plan` would list the checks. Its JSON carries only route, holdouts and hashes, so the TOML read above stands in.
  - "A real relais run shows no msrv/audit in its log dir" → replaced by the TOML read above, since no relais run was made (see the Decision log). The first relais run on relais after the merge is the live check.
  - "CI's msrv and audit jobs still run on the PR" → both jobs trigger on `pull_request` with no path filter (`ci.yaml:14`, `:124`, `:180`). Their conclusions are read on the PR by `merge-when-green`.
  - The commit's `pre-commit-cargo-test` was deferred to CI (no host slot freed: a pre-push `cargo-test` from another session had held one for 17 min). That is the waste this plan targets. It is the repository's declared `set onBusy defer` (amont.conf), not a skip: no `hook.skip`, no `--no-verify`.

## Decision log

- 2026-10-31: A1 was edited directly, not through relais: a relais run would have run the very checks the change removes.

## Implementation review

- A1: approve after a Delta. Round 1 was approve-with-changes (51k tokens, 38 s); the Delta approved (55k tokens, 19 s).
- Fixed: the record now has all three A1 checks, and the audit comment now says only its findings are advisory.
- Phase A1 is done. Next: A2 in amont, then the Part B programme map.

<!-- panel: repos=relais,amont reviewers=architect,backend,lang:rust,po,tui,unix body-sha=1fbe1abf744f -->
