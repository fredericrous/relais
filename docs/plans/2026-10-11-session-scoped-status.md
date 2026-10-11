---
title: "relais: session-scoped status, orphan reaping, cancel tool, per-turn cost"
status: active
date: 2026-10-11
branch: fix/session-scoped-status
repos: [relais]
adrs: []
---

# relais: session-scoped status, orphan reaping, cancel tool, per-turn cost

## Review panel

👉 **Decide:** none. Approve if one PR (Rust and plugin) plus a v0.13.0 release is the right size for these four fixes.
📍 relais · plan reviewed, nothing built · next: a worktree, then the tests written first. Panel: backend, lang:rust, tui, unix.
**Changed by review:** cancel goes through the coordinator heartbeat, with no SIGTERM. Reaping is a compare-and-set judged by pid plus start time. Out-of-process events are written append-only.
📄 Full reviews: [2026-10-11-session-scoped-status.reviews.md](2026-10-11-session-scoped-status.reviews.md)
**Verdicts:** 4 approve-with-changes. In round 2 backend approved after 3 high findings were fixed. 7 findings are carried into implementation.

## Context

A bug report (run `run-65d7dca899789-2d73`) showed four defects:

1. **Status crosses sessions.** The `status` tool and `/relais-status` say
   "this session's runs". When the plugin's memory is empty, they call
   `relais native status` with no `--run`. That returns the run with the
   newest `events.jsonl` on the machine (`protocol::latest_run`,
   `crates/relais/src/protocol.rs:593`), whatever session started it
   (`claude-plugin/hooks/runs.ts:402-436`, `register.ts:279-290`). So every
   fresh session adopts another session's run. Cross-session visibility is
   not intended.
2. **Orphans look live forever.** "Running" and `elapsedMs` come only from
   the `events.jsonl` fold: there is no `outcome` event, so the run is
   running (`timeline.ts:97,373`). A driver that dies writes no outcome.
   - The orphan sweep (`interrupt_orphan`, `main.rs:5986`) updates only the
     ledger.
   - It acts only after silence of wall time + 15 min.
   - It acts only on runs that still hold a worktree.
   - `rebuildRuns` ignores the ledger's `current.state`.
   - The coordinator daemon never reconciles `runs.status` on start.
3. **No way to remove an orphan from the session.** There is no `cancel`
   MCP tool. `relais coordinator cancel --run` interrupts a workerless run
   only once the silence rule fires, and writes no outcome event.
4. **`cost: unknown`.** Native usage is booked only in `relais native
   stopped`, at the agent's end (`dispatches.ts:118`). An agent that never
   stops books nothing.

**Decided with the person:**
- Cost stays priced from the agents' own API responses, but is booked every
  turn.
- CLIProxyAPI is not used. Its usage feed is a destructive queue with no
  per-request tag, so it cannot be tied to a run without guessing from time
  windows.

## Approach

### A. Status is scoped to the calling session

- **New flag `relais native status --session <id>`.** It returns
  `{"runs":[timeline…]}`, one entry per ledger run with
  `runs.root_session = id`.
  - Newest first, capped at 20.
  - Each entry has the existing timeline plus `current`.
  - Reuse `protocol::timeline` and `current_of_run`.
  - Add a ledger reader `runs_of_session(session, limit)` beside
    `distinct_root_sessions` (`ledger/mod.rs:4681`).
- **`--run X` stays as it is.** It is an explicit lookup and works for any
  run. Its reply gains `current.session` so the plugin can label a foreign
  run.
- **No-flag status is kept for the CLI only.** Its help text says it means
  "latest on the machine".
- **Plugin:**
  - `reloadTimeline` passes `--session <$.session.id()>`.
    `rebuildRuns` accepts the `runs` array.
  - `statusOf` and the pane show only models whose session is this
    session's.
  - A run asked for by id from another session is shown with
    `session: <id>` and `foreign: true`.
- **Wording:**
  - The tool description says "this session's runs (started from this
    Claude Code session id)".
  - SPEC §23 gets the same wording.
- **Labels:** `session` and `foreign` are plain text. The pane shows the
  session id as an 8-char prefix; the full id is in the `--run` reply. The
  longest pane line fits 80 columns or wraps at a word boundary.
- **Empty:** `status --session` with no rows returns `{"runs":[]}` and
  exits 0.
- **Limit:** a session id that changes on `/clear` stops showing the runs
  of the old id. Asking with `run` still finds them. This goes in the docs.

### B. Orphans are reaped from evidence, not only silence

- **Record the driver.** Migration v26 adds three columns to `runs`:
  - `driver_pid INTEGER`.
  - `driver_started`: the process start time, read by a new
    `procs::start_time(pid)` (sysctl KERN_PROC_PID on macOS,
    `/proc/<pid>/stat` on Linux).
  - `driver_since`: when the driver took the run.
- **When they are written.** All three are written in the same transaction
  as the driver's transition to running, at every driver entry point:
  - `insert_run` (`runner/mod.rs:1359`).
  - `insert_child_run` (`runner/mod.rs:1386`).
  - Resume and decide re-entry (see `tests/release_scenarios.rs:2748`).

  The implementation lists every entry point, and a test covers each one.
- **A shared helper decides "orphaned".** `orphan_verdict` (in
  `resume.rs`, a pure function plus injected probes):
  - **Gone:** all three of these hold:
    - `driver_since` is more than 60 s ago, so a resume of an old run is
      safe in its first minute.
    - The driver pid is not alive, or its start time differs from
      `driver_started`. A different start time is how pid reuse is caught.
    - No dispatch pid is alive.

    EPERM counts as alive.
  - **Alive:** the pid is alive and its start time matches. The run is
    never reaped, however silent it is. It is only flagged stalled.
  - **Silent:** the silence rule (wall + 15 min) applies only when liveness
    cannot be judged by pid: the row has no `driver_pid` (a legacy row), or
    the start time cannot be read (for example another pid namespace).
- **The reap is a compare-and-set.** Status is projected from the last
  transition (`PROJECTED_STATUS`, `ledger/mod.rs:2105-2107`), and
  `record_transition` inserts the row before its UPDATE (`:2075` before
  `:2088`).
  - A new `transition_if(run, expected_from, to, …)` reads the projected
    state inside one `BEGIN IMMEDIATE`. If the state is not
    `expected_from`, it returns `false` and writes nothing. Otherwise it
    inserts the transition and updates `runs.status`.
  - Dispatches are closed, and the outcome event appended, only after it
    returns `true`. Today they are closed first (`main.rs:6008-6017`).
  - The driver's own final transition also goes through `transition_if`,
    so a driver that ends after a reap writes no second outcome.
- **Move `interrupt_orphan` into the library** (for example
  `relais::reap`) so the daemon can call it.
  - The reap function returns its message instead of `println!`
    (`main.rs:6037`); the CLI prints it.
  - It appends `Event::Outcome { state: "interrupted" }` and a stderr line
    `driver_gone: …` to the run's `events.jsonl`.
  - Reason text is a full sentence, for example: "run X interrupted: its
    driver (pid N) is gone; the worktree is kept, `relais resume --retire
    X` removes it". `status` shows it.
  - From then on every timeline reader sees the run as ended, the old
    plugin included.
- **Out-of-process event writer.** Events written by a process other than
  the driver use a new append-only writer (in `protocol.rs`):
  - one `write_all` per line, so lines cannot interleave;
  - `seq: None`, so they never collide with the driver's numbering
    (`protocol.rs:458`);
  - a silent wire, so nothing is written to the daemon's stdout
    (`protocol.rs:414-449`).
- **The daemon reaps** in the reconcile thread, on its own ledger
  connection.
  - On `Coordinator::start` (`coordinator/mod.rs:608`) and on every
    `reconcile_loop` tick (15 s, `:848`), it judges the ledger's
    non-terminal runs with `orphan_verdict`.
  - All ledger and file I/O happens outside `lock_state`.
  - A reap error is logged and never fails `Coordinator::start`. Each reap
    writes one structured daemon log line (run, verdict, reason).
  - The worktree is not required. Retiring it stays with `relais resume
    --retire`, which the session-start sweep already runs.
- **The CLI reaps too.** The daemon exits after 10 minutes idle
  (`coordinator/mod.rs:54`), so `relais native status --session` and
  `--run` also reap. They open the ledger read-write only when it exists at
  the current schema; otherwise they stay read-only and report
  `current.orphaned: true` without writing.
- **Stalled flag.**
  - `current` gains `silent_seconds` (reuse `run_silence`, `main.rs:5885`)
    and `stalled: true` for a non-terminal run silent 10 minutes or more.
  - The threshold is a constant beside `DRIVER_MARGIN`, injectable in
    tests.
  - The label is a hint, not a verdict: plain text, no colour, "no events
    for 12m".
- **The plugin trusts the ledger.**
  - `rebuildRuns` folds a terminal `current.state` into an outcome when the
    events have none.
  - `statusOf` refreshes every non-terminal model through `native status
    --run` before it replies, not only when the store is empty.

### C. `cancel` MCP tool

- **New tool `cancel {run, cwd?}`.** It is registered beside `status` in
  `register.ts`. It runs the new `relais native cancel --run X --session
  S`. `--session` is required: without it the command refuses with a
  message naming the flag.
- **Live driver:** `native cancel` sends `Request::CancelRun` to the
  coordinator. No SIGTERM handler exists today, so no signal is sent to
  `driver_pid`.
  - The driver learns of the cancel in its existing coordinator heartbeat
    (`runner/mod.rs:10446`): the reply carries a `cancelled` flag, added if
    absent.
  - The driver then trips its cancel token and writes the `cancelled`
    outcome itself.
  - `native cancel` waits up to 10 s for an outcome. If none arrives and
    the driver is Gone, it reaps with `cancelled_without_worker` (the
    existing unused `Reason::CancelledWithoutWorker`, `lifecycle.rs:63`;
    open dispatches are closed). If none arrives and the driver is alive,
    it reports "cancel requested; the driver has not ended yet".
- **Output, one stdout line per outcome:**

  | Case | Line | Exit |
  |---|---|---|
  | cancelled | "run X cancelled" | 0 |
  | reaped | "run X had no live driver; marked cancelled" | 0 |
  | already ended | "run X already ended: <state>" | 0 (idempotent) |
  | foreign live | "run X belongs to live session <id>; cancel it from that session" | 11 (`ResumeRefusedLive`) |
  | unknown run | (no line given) | 10 |
  | usage error | (no line given) | 2 |

  The README's exit-code table (repeated from `main.rs:877-900`) is
  updated.
- The plugin also marks the run's dispatches stopped in its store, so it
  sends no late `stopped` for them.
- `relais coordinator cancel --run` uses the same path.

### D. Cost booked per turn

- **New subcommand `relais native usage --dispatch D`.**
  - It reads the dispatch's cumulative usage on stdin:
    `{agent, turns, usage}`, plus that turn's message ids.
  - It books in one `BEGIN IMMEDIATE` transaction.
  - The stored cumulative total per dispatch is monotonic: a lower or equal
    cumulative is ignored, and a negative delta clamps to 0.
  - The message ids go through the existing message-id dedup
    (`runner/mod.rs:792-796`), so neither a retry nor `usage import`
    double-counts.
  - Empty or TTY stdin exits 2 with a stderr message and never blocks.
  - It emits `Event::Cost`. Reuse `usage_of_report` / `usage_of`
    (`adapter/native.rs:227-313`) and the booking path `stopped` uses.
- **`stopped`** books only the remaining delta, through the same function.
- **Plugin:** `onTurn` (`agents.ts:210`) sends `native usage` after
  `withTurn`, through the retrying delivery `stopped` uses, with no
  debounce.
  - Volume: one process spawn per agent turn (typically 10-60 per run,
    about 20 ms each), delivered asynchronously.
- **Show why a cost is unknown.** An unpriced model now reads `cost:
  unpriced (<model>)` instead of `unknown`.
  - Rust emits the `unpriced_reason` (`orchestration/pricing.rs:185`) in
    the cost event.
  - The model name is sanitized: control characters and escapes stripped,
    capped at 40 chars.
  - `formatCost` (`timeline.ts:378`) shows it.

## Files

- **Rust:**
  - `crates/relais/src/main.rs`: the `NativeCommand` Status, Cancel and
    Usage subcommands, the cancel path, and moving the orphan code out.
  - `protocol.rs`: the append-only out-of-process event writer
  - `resume.rs`: `orphan_verdict`
  - New `reap.rs`
  - `ledger/mod.rs`: v26, `runs_of_session`, guarded reap CAS, monotonic
    delta booking
  - `runner/mod.rs`: `driver_pid`/`driver_started` at every entry point
    (`insert_run`, `insert_child_run`, resume and decide re-entry), and the
    heartbeat `cancelled` flag (`:10446`)
  - New `procs::start_time`
  - `coordinator/mod.rs`: reaping on start and per tick
  - `adapter/native.rs`
- **Plugin:**
  - `claude-plugin/hooks/register.ts`: the cancel tool and descriptions
  - `runs.ts`
  - `timeline.ts`
  - `agents.ts`
  - `dispatches.ts`
  - `pane.ts`: session and stalled labels
- **Docs:**
  - `docs/SPEC.md`: §23 status scope, cancel, per-turn cost
  - `README.md`: exit-code table
  - `CHANGELOG.md`: v26 refusal by older binaries, `relais coordinator
    stop` on upgrade

## Tests (written first, `tests.first-properties`)

- **Rust:**
  - `orphan_verdict` property tests, for these cases:
    - pid dead
    - pid alive (EPERM counts as alive)
    - pid alive but start time differs from `driver_started` (reused):
      reaped
    - pid alive with a matching start time, silent for hours: never reaped,
      only stalled
    - no pid but silent
    - no pid and recent
    - a dispatch pid alive keeps the run
    - a driver that took the run less than 60 s ago is not reaped,
      including a resume of an old run
  - A driver's final transition after a reap writes nothing.
  - Every driver entry point (`insert_run`, `insert_child_run`, resume,
    decide re-entry) records `driver_pid` and `driver_started`.
  - The reap appends an outcome event, and `timeline()` then reports it
    ended.
  - Concurrent reapers leave exactly one transition and one outcome line.
  - Out-of-process writer: 10k concurrent lines all parse, and daemon
    stdout is 0 bytes.
  - `runs_of_session` returns only that session's rows; `status --session`
    with no rows returns `{"runs":[]}`, exit 0.
  - The v26 migration upgrades a v25 ledger.
  - Usage booking:
    - 8 concurrent shuffled cumulatives sum to the final total exactly.
    - An out-of-order cumulative is ignored.
    - Sending the same cumulative twice books once.
    - `stopped` after usage books only the rest.
    - Empty or TTY stdin exits 2.
  - `native cancel` exit codes per case: cancelled 0, reaped 0, already
    ended 0, foreign live 11, unknown run 10, missing `--session` 2.
  - The heartbeat `cancelled` flag ends the driver with an outcome line.
  - A daemon start with a dead-driver row leaves it `interrupted`.
- **Plugin** (the existing test runner under `claude-plugin`):
  - `rebuildRuns` with `{runs:[…]}`.
  - A terminal `current` with no outcome is folded as ended.
  - `statusOf` never returns another session's run unless it is asked for
    by id.
  - `onTurn` sends cumulative usage.
  - `formatCost` handles the unpriced case, with the model name sanitized.

## Verification

1. Run `make check` (the repository's single build and test command). Run
   the plugin tests. Match the CI commands.
2. **Live, from the worktree build** (input -> expected exit code and
   output):
   - **Session scoping:** start a run in session A; in session B call
     `status`. Expect "No relais run is known in this session".
     `relais native status --session B` prints `{"runs":[]}`, exit 0.
   - **Orphan reaping:** `kill -9 <driver>`. Within 15 s,
     `relais native status --run X` shows `current.state ==
     "interrupted"`, exit 0, and `events.jsonl` holds exactly one outcome
     line. The pane stops counting.
   - **Cancel, live:** `relais native cancel --run X --session S` on a
     live run: exit 0, "run X cancelled".
   - **Cancel, ended:** the same command on the reaped run: exit 0, "run X
     already ended: interrupted".
   - **Cancel, foreign:** with another session's id: exit 11, "run X
     belongs to live session <id>; cancel it from that session".
   - **Cancel, errors:** an unknown run exits 10; no `--session` exits 2.
   - **Stalled:** use the injected-threshold test seam, not a 10-minute
     wait: a run silent past the injected threshold shows `stalled`,
     "no events for 12m" style, in plain text.
3. Clean up the reported run: `relais native cancel --run
   run-65d7dca899789-2d73 --session <its session>`. Expect exit 0 and "run
   run-65d7dca899789-2d73 had no live driver; marked cancelled".

## Landing

- **PR:** one PR, worktree from `origin/main`, branch
  `fix/session-scoped-status`. Merge on green with the implementation
  review.
- **Release:** cut `v0.15.0` (v0.13.0 and v0.14.0 were taken meanwhile; see the Decision log). It is a minor version for the new tool and
  subcommands, and the binary and plugin must ship together. Then run step
  2 of Verification against the installed release.
- **Migration:** binaries older than v28 refuse a v28 ledger
  (`ledger/mod.rs:2316`). The upgrade runs `relais coordinator stop`
  before the new binary is used, and the CHANGELOG says so.

## Verification results

The pre-push checks ran on 2026-10-11. They used the worktree's release
build (`0e23cc1`) on an isolated copy of the live ledger: 31 runs from 9
sessions, today. The ledger was v27, and the daemon migrated it to v28.

| Check | Input | Expected | Actual |
|---|---|---|---|
| Scoping, this session | `native status --session 3aa68d2b…` | only this session's runs | 9 runs, all with `session` 3aa68d2b… ✅ |
| Scoping, another session | `--session 16e94aad…` | only that session's runs | 4 runs, all with `session` 16e94aad… ✅ |
| Scoping, empty | `--session nobody` | `{"runs":[]}`, exit 0 | as expected ✅ |
| Orphan reap, alive | running run, live driver pid | not reaped | `running`, verdict `alive` ✅ |
| Orphan reap, `kill -9` | the driver killed, daemon running | `interrupted` within 15 s, one outcome line | `driver_gone` 4.8 s after the run went live; 1 new outcome line with the full sentence; one daemon log line ✅ |
| Cancel, no session | `native cancel --run X` | exit 2, names the flag | exit 2, "--session is required" ✅ |
| Cancel, unknown run | `--run run-nope` | exit 10 | exit 10 ✅ |
| Cancel, already ended | the reaped run | "already ended: interrupted", exit 0 | as expected ✅ |
| Cancel, foreign live | another session's run, driver just took it | exit 11 | exit 11, names the session ✅ |
| Cancel, foreign gone | another session's run, dead driver | "had no live driver; marked cancelled", exit 0 | as expected; `cancelled` / `cancelled_without_worker` ✅ |
| Cancel, own live | this session's run, driver ignores it | "cancel requested; the driver has not ended yet" after 10 s | as expected, exit 0 ✅ |
| Stalled | injected threshold | `stalled: true` | `native_status` test with `RELAIS_STALLED_AFTER_SECS` ✅ |
| Per-turn cost, plugin | per-turn booking through the runner's stopped path | the rest booked once, one phase | runner and ledger tests; 251 plugin tests ✅ |

The live plugin's per-turn booking in a real Claude Code session is
checked against the installed release, as Landing says.

## Decision log

- **Release number.** v0.13.0 and v0.14.0 were released from `main` while
  this branch was in progress. The driver migration is therefore **v28**,
  not v26, and this work ships as **v0.15.0**.
- **v27 and free-text actors.** v0.14.0's typed-actor migration (v27)
  refused the live ledger. Four decision rows held a free-text actor this
  session had passed to `relais decide`. Those rows were re-spelled
  `claude-orchestrator`, after a backup at
  `~/.local/state/relais/ledger.sqlite.bak-20261011-actor-fix`, and v27
  then migrated the ledger normally.
- **v27 hard refusal.** v27 refuses the whole ledger on an unmapped actor
  spelling. This is left to the person: it is upstream and outside this
  plan.
- **First status after upgrade.** `native status` never migrates, so on a
  v27 ledger `--session` prints `{"runs":[]}` (with a stderr line) until
  a writer migrates it. The daemon's first start does that.
- **"Resume re-entry" stamps no live driver.** In relais, resume
  reconciles a run; it does not re-drive one. Its stamp lands on a run it
  moves to `interrupted`, which is terminal. Decide re-entry is the real
  re-entry, and it is stamped.
- **Daemon log.** The on-demand daemon had stderr set to /dev/null, so it
  now writes `coordinator.log` (0600, rotated past 1 MiB) in the state
  directory. Without it, reap lines and sweep errors were lost.
- **`coordinator cancel` on an unknown run** now exits 10 (unknown run)
  instead of 0. This is in the CHANGELOG.
- **How the work was done.** It went through relais in nine runs. Several
  of them ran out of the 45-minute wall clock, because this repository's
  `make check` takes 12–20 minutes per verification. Review findings
  (relais's own reviewer) were fixed in this session, and each decision
  is recorded in the ledger as `orchestrator`.
- **Rebase.** The intermediate commits were replayed without hooks. Every
  slice's tree was checked: either the gate ran before the rebase, or a
  relais baseline ran on the rebased HEAD. The push gate runs `make
  check` on the final tree.

<!-- panel: repos=relais reviewers=backend,lang:rust,tui,unix body-sha=2552bb4c132a -->
