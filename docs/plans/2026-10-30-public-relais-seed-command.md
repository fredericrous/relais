---
status: active
branch: feat/dataset-seed
repos: [relais]
adrs: [ADR-0045]
---
# Plan: a public relais seed command

## Review panel

👉 **Decide:** none. Approve if a seed command that writes only a not-yet-existing ledger in an explicit `RELAIS_STATE_DIR` is the right base for the value-checking holdout.
📍 relais · plan reviewed, not started · next: Phase 1 (`relais dataset seed`), then the widen plan's Phase 2b. Panel: backend, lang:rust, unix, tui (opus-5-5), codex (gpt-6-astra).
**Changed by review:** no existing ledger is ever opened or replaced; the staging file is per invocation, published by `hard_link`, and the link is the commit point; each run keeps its own `created_at`, and every waiting state counts as a decision.
📄 Full reviews: [2026-10-30-public-relais-seed-command.reviews.md](2026-10-30-public-relais-seed-command.reviews.md)
**Verdicts:** backend approve on the final body, after 4 binding passes; lang:rust rework then approve-with-changes; unix, tui and codex approve-with-changes. 6 findings carried into implementation.

## Context

The holdout widening plan (decisions `2026-10-29-widen-holdout-acceptance`) needs a second holdout, `report-values`, that checks the counts `relais report --json` reports, not only its shape. A holdout is written apart from the change and may use only relais's public interface (ADR-0045).

The Phase 2a probe found one public route to a relais state with known counts: driving `relais run --protocol` by hand while playing the plugin. As a holdout that route is fragile:
- it nests a full run inside verification;
- it shares the machine's heavy slots, with a deadlock risk;
- it depends on an undocumented `[pricing]` table and a pinned Claude CLI range.

The person chose to add a documented public seeding command first ("Public seed command first").

Today no relais command creates runs from a file. `dataset import-task-types` (cli/mod.rs:601, cli/learn.rs:817) only labels existing tasks. The report reads ordinary ledger rows that tests already write through the ledger API (`insert_run`, `record_transition`, `record_usage`, `record_holdout_outcome`, `record_holdout_exposure`; see tests/it/holdout_report.rs).

## Goal

- `relais dataset seed <file>` writes a described set of runs into an empty, explicitly chosen relais state.
- `relais report --json` then counts those runs exactly as it counts real ones.
- It never writes into a person's ledger.

## Non-goals

- No import of real history and no merge into an existing ledger: the command seeds only an empty state.
- No change to how `relais report` counts. The seed writes the same rows a real run writes, through the same ledger functions.
- No fake worker and no `relais run`: nothing is dispatched, and no slot or coordinator is used.

## Behaviour

**Command:** `relais dataset seed <file|->`, documented in `--help` and SPEC beside the other `dataset` commands.
- **On success:** exit 0. stderr gets one line, `relais dataset seed: seeded <n> runs into <ledger path>` (the stream and prefix of `import-task-types`, cli/learn.rs:851). With `--json`, stdout carries one object, `{seeded, ledger}`, and nothing else.
- **Refused:** exit 2.
- **Unreadable file or ledger I/O error:** exit 1.
- **With `--json`,** a refusal leaves stdout empty and prints its reason as text on stderr.

**Guard.** Every check runs before any ledger is opened: `Ledger::open` creates and migrates the file (ledger/mod.rs:174), so a refusal creates nothing. The command refuses unless all of these hold:
- `RELAIS_STATE_DIR` is set and non-empty;
- the state directory is not the default one. The comparison is on canonicalised paths, the parent canonicalised when the directory does not exist yet. So `/tmp` and `/private/tmp`, a trailing `/`, `..` and a symlink to `~/.local/state/relais` all resolve to the same place, and the default is refused however it is spelled;
- no `ledger.sqlite`, `ledger.sqlite-wal` or `ledger.sqlite-shm` exists in the state directory. Seed writes only a ledger that does not exist yet: an existing one, even an empty one, is refused, so no existing file is ever opened, renamed over or inspected.

Each refusal names the reason and the next command, for example: ``relais dataset seed: refused: RELAIS_STATE_DIR is not set; seed writes only into an explicit, empty state; run `RELAIS_STATE_DIR=$(mktemp -d) relais dataset seed <file>` ``. The existing-ledger refusal names the path and says to use a fresh directory.

**File format.** JSON lines, one run per line. The whole file is validated before anything is written, duplicate run ids included. A bad file is refused (exit 2) in two parts:
- every bad line, as `file:line: reason`, or `<stdin>:line: reason` for `-`;
- then `relais dataset seed: <n> bad line(s); nothing was written`.

This is the contract of `import-task-types` (cli/learn.rs:830-836).

```json
{"run": "r1", "task": "t1", "created_at": "2026-10-01T00:00:00Z", "status": "accepted", "accepted_by": "relais", "cost_micros": 120000,
 "holdouts": [{"name": "report-json", "outcome": "failed", "profile_green": true, "source": "named"}],
 "exposures": [{"marker": "store"}]}
```

- **`status`:** one of the terminal or waiting states (accepted, failed, blocked, needs_decision, needs_review, interrupted, budget_exhausted, cancelled). It is written as the state chain the report's status projection reads, ending in that state.
- **`accepted_by`:** applies to `accepted` only. The run is written as prepared → verifying → accepted. The last transition's reason is `ChecksAndReviewPassed` for `relais`, or `DecisionApproved` for `person`: the reason `accepted_by_person` reads (report/mod.rs:53).
- **Decisions:** `needs_decision`, `needs_review` and `interrupted` each open a decision row through `record_transition` (`awaits_a_person`, lifecycle.rs:520), so each counts in `pending_decisions`.
- **`created_at`:** each run is written through a ledger opened with a fixed clock at its `created_at` (`open_with_clock`, mod.rs:179). Its transitions carry that time, so `--since` windows see the file's dates.
- **`cost_micros`:** written as one usage event.
- **`holdouts` and `exposures`:** written with `record_holdout_outcome` and `record_holdout_exposure`. `outcome` takes the four verdicts plus `skipped`; `source` is `named` or `attached`.
- **Required fields:** `run`, `task` and `status`; all others are optional. The first line may be `{"seed_schema": 1}`, and any other version is refused.

**Atomic.** The existing write functions each commit their own transaction (runs.rs:112, transitions.rs:317), and `record_usage` writes on the bare connection. The seed writes into an exclusively created, invocation-specific temporary database in the same directory. It never deletes another invocation's staging files, including apparent leftovers; automatic stale-file removal is outside this command's scope. A new `Ledger::close(self) -> Result<()>` runs `PRAGMA wal_checkpoint(TRUNCATE)` and `conn.close()`, and the seed asserts that its own temporary database's WAL sidecar is gone. Only after successful close is the file published with `std::fs::hard_link` onto the absent `ledger.sqlite`. Destination-exists failure is a refusal (exit 2); other link I/O failures exit 1. Successful linking is the commit point. Removing the temporary name happens afterward; cleanup failure emits a warning on stderr while preserving exit 0 and the normal success output.

Before publication, a Drop guard attempts cleanup of only this invocation's temporary database and sidecars, after its connections close. After publication, cleanup never removes the destination. Cleanup errors must not mask the primary error or cause another panic.

## Phases

- [x] Phase 1 — relais: the `dataset seed` subcommand, the guard, the parser with whole-file refusal, the writer into a staging database published by `hard_link`, `--json`, SPEC beside `import-task-types` (docs/SPEC.md ~985), `--help`, and the CHANGELOG. Release.
- [ ] Phase 2 — the widen plan's Phase 2b resumes with `report-values` built on `relais dataset seed`. That work lives in that plan, not this one.

## Decision log

- 2026-10-30 — A seed command rather than a protocol-driven run, chosen by the person after the 2a probe: deterministic, no slots, no nested run.
- 2026-10-30 — The guard requires an explicit `RELAIS_STATE_DIR`, no ledger file at all and a non-default directory (review: seeding over an existing empty WAL ledger was the source of every remaining edge, and a holdout always seeds a fresh directory). Seeding fake runs into a real ledger would corrupt every report and the learning loop, so the default target is impossible by construction, not merely discouraged.
- 2026-10-30 — Atomicity comes from a staging database published by `hard_link` onto the absent target, not from new transaction-taking variants of the ledger writers: the target must not exist anyway, and this leaves the ledger's write functions untouched (review: backend's route over lang:rust's).
- 2026-10-30 — Rows go through the existing ledger write functions, so the report's counting is exercised as it is for real runs. A seed-specific shortcut would test the shortcut.

## Verification

- Phase 1:
  - Integration tests run the binary with `RELAIS_STATE_DIR` and `HOME` set to fresh temp dirs. A seed file with one run per status, both `accepted_by` values, named, attached and skipped holdouts, a failed holdout on a green profile, an exposure and a cost gives `relais report --json --no-import --since 2000-01-01`:
    - run counts per status;
    - `accepted_tasks` with the relais/person split;
    - `pending_decisions` covering needs_decision, needs_review and interrupted;
    - `holdouts.holdout_runs`, `holdout_catches`, `holdout_exposures` with `by_marker`;
    - `by_source` named and attached, with `skipped`;
    - total cost.
  - Every value matches the file.
  - Dates: two runs dated in different months. The default `--since` counts only the current month's.
  - Refusals, each a test with exit 2, the reason and next command on stderr, and stdout empty under `--json`. After each one, the state directory's ledger files are exactly as they were:
    - `RELAIS_STATE_DIR` unset;
    - set to the default directory, under a temp `HOME`, through a trailing `/` and through a symlink;
    - an existing ledger, empty or not, and a lone `-wal` or `-shm`;
    - two bad lines, both reported with `file:line`, then the summary;
    - the same file on stdin, labelled `<stdin>:line`;
    - a duplicate run id;
    - an unknown status or outcome;
    - `seed_schema` 2.
  - Atomicity, a unit test on the seed writer: `ledger::failpoint` and `storage_full::failpoint` are `cfg(test)` and thread-local, so no binary test can arm them. A `storage_full` error at run 2 of 3 returns an error, and a panic there unwinds through the Drop guard. Both leave no `ledger.sqlite` and no staging files owned by that invocation. Add a coordinated concurrency test: pause one seeder after its first run, start another with distinguishable input, then resume both. Exactly one publishes successfully, the other returns the destination-exists refusal (mapped to exit 2 by the CLI test below), the final database contains precisely the winner's complete input, and neither invocation removes the other's staging files.
  - The integration test covers I/O instead: a state directory made read-only after validation exits 1 and leaves no ledger.
  - Sidecars: after a seed, no `-wal` sits beside `ledger.sqlite`, and `PRAGMA integrity_check` returns `ok`.
  - Publication, a unit test on the seed writer (the failpoint is `cfg(test)`, as above): inject failure removing the temporary name after a successful hard link. Assert the writer returns a published outcome carrying a cleanup warning, and a readable destination containing the complete expected counts. A test of the CLI's outcome-to-exit mapping asserts that this outcome gives exit 0, the normal success output and the warning on stderr, and that a destination-exists refusal gives exit 2.
  - SPEC and `--help` name the command (the repository has no `--help` snapshot file; the test asserts the `seed` subcommand line of `relais dataset --help`).
  - Piloted: the released binary seeds a scratch state, and `report --json` shows the seeded counts. The real ledger's mtime and run count are unchanged.

  - **Observed 2026-10-31,** on `feat/dataset-seed`. Step A (run-65f16342fdc05-c613) and step B (run-65f1b830d7c34-13fb5) were each accepted on green profile checks, with the `report-json` holdout passing 25/25 and a relais review. Step B went through two review-fix successors. Every commit passed the amont gate (31 checks).
    - The six writer unit tests: close, three runs, `storage_full` at run 2, a panic at run 2, two writers, and a failed cleanup after the link. → passed.
    - The integration tests: report counts against the file, the month window, every refusal, I/O exit 1, the exit mapping, the `--help` line. → passed.
    - Piloted with the branch binary on an isolated state. Three seeded runs gave `relais report --json`: accepted 2 (relais 1, person 1), pending_decisions 1, holdout_runs 2, holdout_catches 1, exposures 1 (`store`), by_source named {1, 0} and attached {1, 1, skipped 1}. Each matched the file.
    - A second seed into the same directory exited 2: the ledger already exists. `$HOME/.local/state/relais/` exited 2: the default directory.
    - The real ledger's mtime was unchanged, with 970 runs before and after.
    - The pilot on the released binary follows the release.
  - **Deliberate:** `Ledger::close` reports a checkpoint blocked by another connection as `LedgerError::Corrupt`. A distinct variant would touch every exhaustive match on `LedgerError` for a path only the seed uses. Its detail says that another connection holds the log.

## Outcome

(Filled when the plan closes.)

<!-- panel: repos=relais reviewers=backend,lang:rust,unix,tui,codex body-sha=b714d8d4b6d1 -->
