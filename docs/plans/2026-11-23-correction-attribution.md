---
status: active
branch: prog/phase8-learner/t12-correction-attribution
repos: [relais]
adrs: []
programme: docs/programmes/phase8-learner
ticket: t12-correction-attribution
plan-id: p-01M8394B6JCY6NB1NYH5MQHDWA
---
# A correction records its files and charges one task

## Review panel

👉 **Decide:** none — approve if a correction should be charged to the earlier task whose files it fixes, decided when the ledger is read.
📍 relais · 3 phases · 1 release · 1 step for you · next: Phase 1. Panel: backend, lang:rust, lang:typescript, tui, unix (opus), codex.
**Changed by review:** per-correction `hashFiles`; a flag is never charged away; `outcome_uncorrected` only from ended records, first kept.
**Verdicts:** final: 1 approve, 5 approve-with-changes; 11 carried; words 1314 → 1630 (×1.24); remove 0, merge 0.
📄 Full reviews: `2026-11-23-correction-attribution.reviews.md`

## Picture

```
 task A (session 1)          task B (session 1 or 2), open
 ended, completed_verified   "that fix in A was wrong": relation correction
 files {x, y}                 |
                              v
                     reassess row (event correction, at T)
                     files: []            <- sent at T, as today
                              |
                     B edits x, z after T (each correction keeps its
                              |      own list; B's task.files is not read)
                              v
                     B ends: same row re-sent (same key, at T), files {x, z}
                     relais merges: files_json = old ∪ new
                              |
     read time (relais): A ended in [T - 24 h, T), completed at end,
                         shares x with the row  ->  charged to A
                              |
          every correction of B charged elsewhere?
             yes -> B's gate-2 outcome is its outcome_uncorrected
             no  -> B stays corrected
```

One correction in task B, from its prompt to the ledger read: the row
gets its files when B ends, and relais charges A when the files overlap.

## Goal

Two gaps from phase8 t02 ("cannot see" 5–6):
- a `correction` reassess row has `files_json = '[]'`, so the miss query
  compares everything task B ever edited, files from before the correction
  included;
- plan §5 says a correction inside open task B that edits an earlier task
  A's files is A's miss only, but the hook marks B `corrected` anyway, so
  gate 2 would count it too.

After this change a correction row carries the hashed files edited between
the correction and the end of B, and relais can say, when it reads the
ledger, which task each correction is charged to. A charged-away B is not
`corrected` for gate 2.

## Non-goals

- Gates 2 and 4 themselves (`relais router evaluate`): they do not exist
  yet (t03/t05). This change ships the data and the read rule they call.
- Rewriting the outcome relais stores on B: a stored `corrected` never moves
  (merge rule), and A's record may reach the ledger after B's (another
  session, flushed at its end). The charge is decided when the ledger is
  read, never when a record is written.
- Old rows: correction rows written before the release keep `[]` and charge
  no one; B stays `corrected` for them. t02's query keeps its fallback
  (B's whole `files_json`) for those rows.
- Showing a charge or an override to the person: t03/t05 own that output, and it must not rely on colour alone.
- A release: t10, t11 and t12 ship together after this PR (map Handoff).

## Code review

- relais: backend, lang:rust, lang:typescript (unix, tui left out: no command line, no output a person reads)

## Behaviour

### Plugin (`claude-plugin/hooks/router.ts`, `routing.ts`)

- `TaskState` gains `corrections?: { at: number; record: ReassessRecord;
  files: string[] }[]`. Optional: a task stored by 0.72.0 loads without it
  and reads as `[]`.
- `decideMain`, on a correction: after `escalate`, it appends
  `{ at: input.now, record, files: [] }`. `routing.ts` already queues the
  row with `reassessRecord(out.task, record, now)`, and `now` is the
  `input.now` given to `decideMain`, so the row's key `(session, task,
  agent, 'correction', at)` is known to the task.
- `onEvidence`, on an edit with a file: the file is appended to every
  entry of `corrections` that does not hold it yet. `task.files` is not
  used: it skips a file B edited before the correction and again after.
  Each correction's window runs to the end of the task, so a second
  correction's edits also count for the first. `corrections` keeps the last 8 entries.
- `queueCorrectionFiles(fx, store, task)` calls `hashFiles` (t11) once per
  entry; `hashFiles` returns no path-to-hash map, and its root cache makes
  the later calls spawn no git. It queues
  `{ ...reassessRecord(task, entry.record, entry.at), files }` for each
  entry with a hash. It is awaited in `endTask` (also run by
  `onSessionEnd`) and in `queueCorrected`, so a session that dies early
  still sent the earlier corrections' files.
- The `/flag` path sets `flagged: true` on the task (optional, default
  `false`). `taskRecord` adds `outcome_uncorrected`: `outcomeOf({ ...task, corrected:
  false })` on an ended record (`endedAt !== null`) when `task.corrected`
  and not `flagged`, else `null`; the early `queueCorrected` snapshot
  carries none. Only the plugin can compute it.

### Wire and ledger (`crates/relais/src/router/wire.rs`, `ledger/router.rs`)

- Task records accept `outcome_uncorrected` (optional, one of the task
  outcomes, default `null`). Stored in a new column
  `router_tasks.outcome_uncorrected TEXT` (migration v49, expand-only).
  Merge: like `completed_at_end`, the first value from a record with
  `ended_at` set is kept, so a late retry of an earlier snapshot never
  moves it; a test sends the two orders.
- The reassess insert keeps `ON CONFLICT … DO NOTHING` for every event but
  `correction`. For `correction` it reads the stored row in the same
  transaction and writes `union(stored, incoming)` (`merge_task`'s
  helper), never a replacement: a git timeout can drop paths from one send.

### Read rule (`Ledger::router_correction_charges`)

`router_correction_charges(stop: &[String]) -> Result<Vec<CorrectionCharge>>`
(a new `TaskKey { session, task_id }`, `Ord`), one row per main-session
(`agent_id = ''`) correction reassess row: `session`, `task_id`, `at`,
`charged: Vec<TaskKey>`, `legacy: bool` (the row's files are empty, so
`charged = []` means unknown, not no overlap). `stop` is the caller's list
of hashes to ignore (t02's memory-file hashes for rows before t11; scoped
hashes already drop paths outside a repository), bound through a temp
table, not an `IN (…)` list. A task A is charged
when all of these hold:
- A is a main task (`agent_id = ''`) and not B itself (same session and
  task id);
- A's `completed_at_end` is `completed_verified` or `completed_accepted`
  (the outcome as it ended, which a later record never moves);
- A's `ended_at` is in `[at − 24 h, at)`, in any session, compared as
  parsed chrono `DateTime<Utc>` (SQLite `datetime()` text does not sort
  against RFC 3339);
- A's `files_json` shares a hash with the row's, outside `stop`.

`corrections_charged_away(&[CorrectionCharge]) -> BTreeSet<TaskKey>`: the
tasks B **every** one of whose correction rows has a non-empty `charged`,
and that hold no `flag` reassess row (a flag is never charged away; a test
sends correction, flag, overlapping files).
For gate 2 such a B's outcome is its `outcome_uncorrected` (unknown when
`null`). A subagent task whose outcome came from that parent
(`outcome_source = 'parent'`) takes the parent's `outcome_uncorrected` the
same way; t03/t05 wire this into the gates, this change states it in the
plan and the protocol.

### Docs

- `docs/plans/2026-10-07-session-router.md` §5: the files window, the read
  rule and `outcome_uncorrected`.
- `docs/router-protocol.md`: `files` on a correction reassess, the
  correction upsert in the idempotency list, `outcome_uncorrected`.
- `t13-revert-overlap-live-count.md`: its correction leg joins on the
  correction row's `files_json` for rows after the release.

## Phases

- [x] Phase 1 — plugin: `corrections`, `queueCorrectionFiles`,
  `outcome_uncorrected`, with tests: `claude-plugin/hooks/router.ts`,
  `claude-plugin/hooks/routing.ts`, `claude-plugin/tests/router.test.ts`,
  `claude-plugin/tests/routing.test.ts`
- [x] Phase 2 — Rust: migration v49, wire field, merge, correction upsert,
  `router_correction_charges`, `corrections_charged_away`, with tests:
  `crates/relais/src/router/wire.rs`, `crates/relais/src/ledger/router.rs`,
  `crates/relais/src/ledger/schema/migrations.rs` (and schema tests)
- [x] Phase 3 — docs and close t12 (Resolution, Decisions line, Handoff
  naming the joint release): `docs/plans/2026-10-07-session-router.md`,
  `docs/router-protocol.md`, `docs/programmes/phase8-learner/`

### 🧑 Your steps
- After the release that carries t10–t12, reinstall the plugin:
  `relais install --claude --user --write`, then `relais coordinator stop` —
  because the hooks run from the installed plugin, and only rows the new
  code writes carry correction files. Check with `relais doctor`: its
  `plugin` finding names the new release; if it names an older one, the
  reinstall did not land, so run the install command again. The plugin is
  embedded in the binary and installed at its version, so the new plugin
  never writes to an older relais (whose wire refuses unknown fields).

## Verification

- `amont run --stamp plugin-test`:
  - a correction then edits of x (edited before the correction too) and z:
    the end queues one correction reassess row with the original `at` and
    the hashes of x and z only;
  - two corrections: the first's row holds the files of both windows, the
    second's only its own;
  - a task with `corrections` round-trips through the store serialization, and the queued row's `at` equals `iso(entry.at)`;
  - a `/flag` after a correction leaves `outcome_uncorrected` null;
  - a task with no `corrections` field (0.72.0 state) loads and ends with
    no correction row and no error;
  - `outcome_uncorrected` is `completed_verified` for a corrected task
    whose last check passed, `null` for one never corrected.
- `cargo test -p relais` (focused, then the crate):
  - one correction row sent `[x]`, `[z]`, `[z]` stores `[x, z]` (retries
    are safe); a revert row sent twice keeps its first files;
  - charged case: A (other session, completed_verified, ended 2 h before,
    files {x}) and B's correction row {x}: `charged = [A]`, B in
    `corrections_charged_away`;
  - kept cases, each leaving B `corrected`: no overlap; A ended 25 h
    before; A `corrected` at its end; A is B itself; A is a subagent task;
    B with two corrections of which one is uncharged; a legacy row `[]`;
    an overlap only through a `stop` hash;
  - the end row `[x, z]` arriving before the early `[]` row stores `[x, z]`
    (the union does not depend on order);
  - a task record without `outcome_uncorrected` (0.72.0 plugin) still parses
    and stores `null`;
  - `outcome_uncorrected` round-trips through the wire and the merge: a
    record with it set, then one with `null`, reads back the set value via
    `router_task` (the upsert's SET clause), and v49 applies on a v48
    ledger copy;
  - the window: A ended 23 h 59 m before is charged, 24 h 01 m before is not;
  - a `stop` list of more than 999 hashes works.
- Piloted: a `.backup` copy of the live ledger migrated by the branch
  binary (`RELAIS_STATE_DIR` set to a scratch dir, never the live state):
  v49 applies, `router_correction_charges` over the existing correction
  rows returns each `legacy`, with `charged = []`. It records the row
  count and the query time; the live ledger's sha256 is unchanged.

## Outcome

Phases 1–3 done 2026-11-23. Both relais runs lost their verify to the
1800 s admission wait on a jammed host (load 84 on 6 cores); each kept
candidate was read in full against its contract, applied and checked
in-session (Phase 1 salvaged as `01c90452`, Phase 2 committed as
`443d7d4b`).

| Check | Expected | Actual |
|---|---|---|
| `claude plugin test` (from `claude-plugin/`) | the new tests pass | all 9 new or changed tests pass in two runs; other failures differ per run (1, then 7), each a 5 s hook-budget timeout under load, none in routing |
| `cargo test -p relais --lib -- ledger:: router::` | the new tests pass | 279 passed, 0 failed (13 new) |
| `cargo fmt --check` | clean | clean |
| clippy | clean | deferred by the commit gate (no slot); CI runs it |
| pilot: `.backup` of the live ledger, branch binary, `RELAIS_STATE_DIR` on a scratch dir | v49 applies, live untouched | the copy reads v49 with `outcome_uncorrected` (4,643 tasks, all `null`); its 26 main correction rows are all `[]`, so all legacy, `charged = []`; the live ledger still reads v48. `relais report` took 3 min 29 s under load |

Decision log:
- The wire gate (found while writing the protocol doc): `router-state`
  lists `outcome_uncorrected` in `accepts`, and the plugin drops the field
  for a relais that does not list it, as it does for `answered`. The panel's
  skew finding (unix 1) was right; its rejection rested on the plugin being
  embedded, but the plugin may reach a binary other than the one that
  installed it (`runFallingBackHome`).
- Carried into implementation and done: per-entry `hashFiles` under
  `Promise.all`; the session captured before any await; `flagged` on
  `CorrectionCharge`; a subagent-edit test; millisecond parse with
  unparsable rows skipped; `stop` filtered in Rust.
- Carried and not done: `files_present` (`legacy` stays "files empty");
  `BEGIN IMMEDIATE` (the batch's write transaction serializes writers);
  the first-8 cap (the last 8 are kept; a dropped entry's row was already
  sent with the files it had then).
- The migration is ledger step v50, not v49: the carry-passes PR (#546)
  merged its own v49 (`check_timings.carried_from`) during this branch;
  resolved at the rebase. The pilot above ran before the rebase, as v49;
  the step's SQL is unchanged.
- `greengate run --raw pre-commit-plugin-test` exited 0 without running
  the suite: check the plugin with `claude plugin test`.

<!-- panel: repos=relais reviewers=backend,lang:rust,lang:typescript,tui,unix body-sha=265bd8800120 -->
