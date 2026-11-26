+++
type = "research"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais"]
+++
# How the relais crate is coupled inside

## Question

What is the module-to-module dependency graph of `crates/relais`? Who owns
each ledger table, and what state is shared between modules? Which of the
5-7k-line `mod.rs` files (`verify`, `admission`, `coordinator`, `report`) hide
several jobs, counted in non-test lines (inline `#[cfg(test)]` modules
excluded, and the counting method stated)? Which ties block a crate split:
the `include_str!` and `include_bytes!` paths (13 sites in 9 files), the
`conformance` feature, the tests that pin the skill text and the policy hash,
and the `pub(crate)` surface?

## Look at

- `crates/relais/Cargo.toml` (features, the `conformance` fake) and
  `crates/relais/src/lib.rs` (the module list and what each exports).
- `crates/relais/src/{verify,admission,coordinator,report,policy,route}/mod.rs`
  and `crates/relais/src/ledger/` with its schema.
- `command grep -rn 'include_str!\|include_bytes!' crates/relais/src` for the
  path-relative loads that break when a file moves to another crate.
- `crates/relais/src/install/` and the tests named in
  `docs/SPEC.md` (the skill text and the policy hash are pinned by tests).
- `use crate::` edges between the module groups named in the map's Notes.

## Answer looks like

A table of the module groups with the edges between them (who imports whom,
counted), the ledger tables each group owns, and the shared state. A second
table of the large files with non-test lines per job inside each. A list of the
ties that block a split, each with the file and the cost to cut it. Last, the
baseline for the map's Destination: the build time after a one-line edit and
the largest file's line count, with the command that measured each.

## Resolution

Measured 2026-11-26 on `origin/main` by relais inspect run
`run-66127806354ed-13004` (verified as `run-6612828d4c0d3-a084`) and a hand
build. Counting method: non-test lines are the lines before the file's
`#[cfg(test)]` line (every inline test module in these four files runs to end
of file, so this is exact); edges are production `use crate::` lines, a braced
list counting once per item, a group being the top-level module under `src/`;
ledger writers are a text match of INSERT/UPDATE/DELETE against table names.

### Baseline (the map's Destination)

| Measure | Value | Command |
|---|---|---|
| Full build, tests included (cold target) | 235 s | `cargo build -p relais --tests` |
| Rebuild after a one-line edit in `ids.rs` | 29 s | same, after `echo '// t01' >> crates/relais/src/ids.rs` |
| Rebuild after a one-line edit in `verify/mod.rs` | 23 s | same, after appending a comment |
| Largest production file | `verify/mod.rs`, 7227 raw, 3586 before its tests (3403 non-blank) | `find crates/relais/src -name '*.rs' \| xargs wc -l \| sort -n \| tail -15` |
| Largest file overall | `runner/tests/mod.rs`, 10961 (test-only) | same |
| Largest by non-blank production lines | `admission/mod.rs`, 3865 | the pre-`#[cfg(test)]` count |

Host: Intel i7-9750H, 6 cores, rustc 1.94.1, dev profile, three other
sessions' tests holding host slots. The edit time is a ceiling for noise;
repeat before using it as a target. The edit cost is only 23-29 s because
the crate is one compilation unit rebuilt whole: a split pays off in what an
edit rebuilds, so t02 should set the target against this.

### Module-group edges (production; 151 cross-group, 62 within, 291 touch unnamed modules)

| from \ to | routing | execution | verification | scheduling | state | programme | plugin |
|---|---|---|---|---|---|---|---|
| routing | 15 | – | 8 | 1 | 17 | – | – |
| execution | 19 | 28 | 27 | 15 | 15 | – | – |
| verification | 1 | 2 | 4 | 6 | – | – | – |
| scheduling | 1 | – | – | 5 | 1 | – | – |
| state | 14 | 1 | 7 | – | 9 | – | – |
| programme | – | – | – | 1 | – | – | – |
| plugin | 1 | 4 | 1 | 4 | 1 | 5 | 1 |

Unnamed hubs: `runner` (145 out, 2 in, the top of the stack), `ledger` (50 in,
49 out), `policy` (67 in, 5 out), `shared_state` (7 in, 34 out).
Large files, production edges: `verify` out procs 5, protocol 3, policy 3,
others 1; in runner 15. `admission` out policy 1; in runner 5, hook 3,
coordinator 3. `coordinator` out admission 3, shared_state 2, procs 2; in none.
`report` out ledger 9, lifecycle 6, ids 5, money 3; in none.

### Ledger tables (46 `CREATE TABLE`, all in `ledger/schema/migrations.rs`)

Every production write is in `ledger/*.rs`: `runs.rs` (runs, contract_revisions,
attempts, candidate_inputs), `transitions.rs` (transitions, decisions,
human_signoffs, landings, decide_nonces), `usage.rs` (usage_events, dispatches,
orchestration_usage), `evidence.rs`/`snapshot.rs` (evidence, receipts,
verification_snapshots), `outcomes.rs`, `review.rs`, `review_parts.rs`,
`router.rs` (7 router_* tables), `trust.rs`, `trials.rs`, `learning.rs`,
`successor.rs`, `task_types.rs`, `admission.rs` (group_bindings, run_epochs),
`holdout_*.rs`, `check_timings.rs`, `sessions.rs`. Readers outside `ledger/`:
`runner` 15 edges, `learn` 10, `report` 9, `router` 6.

Shared state: one SQLite file behind `Ledger`; `coordinator/mod.rs:685`
`Arc<Mutex<AdmissionState>>`; `coordinator/mod.rs:794` `Arc<dyn SharedState>`;
process globals in `protocol.rs`, `procs.rs`, `ledger/clock.rs`,
`learn/salvage.rs`, `fsutil.rs`, `context/mod.rs`; thread-local failpoints in
`ledger/` and `storage_full/`; `coordinator.log` and the group-store journal.

### Large files, non-test lines per job (non-blank; line ranges inferred)

- `verify/mod.rs` 3586 (3403): acceptance gaps and verdicts 1175; command run,
  retry, JUnit 473; profile and setup 266; input classification 191; amont
  hook inventory, attestation, waivers 522; toolchain and baseline cache 393;
  receipt and review records 266; verification worktree 83; integration gaps 34.
- `admission/mod.rs` 4090 (3865): types and wire outcomes 1159; state setup
  323; admission decision 605; lifecycle 423; write leases and reconcile 270;
  group bindings 270; status 249; `Gate` trait 284; `LocalGate` 282.
- `coordinator/mod.rs` 3158 (3036): wire types 454; `Client` and `RemoteGate`
  788; `handle()` dispatch 331; background loops and group store 332; handler
  threads 293; implementation 310; session resolution 182; header 205; election
  97; daemon 44.
- `report/mod.rs` 1781 (1721): cohort and replay statistics 567; rendering 411;
  `runs_report` builder 372; report model 243; spend and run line types 128.

Each of the four holds five to ten jobs. `coordinator` is the cleanest cut:
`Client`/`RemoteGate` (788) and the wire types (454) are a client crate's
surface; `verify` hides the amont-hook and toolchain jobs (915) apart from
verdict settlement.

### Ties that block a crate split

| Tie | Where | Cost to cut |
|---|---|---|
| `include_str!`/`include_bytes!`: 13 sites, 9 files; only one in production | `context/mod.rs:322` pulls `.adr/packs/decisions.pack` four levels up | medium: move or copy the pack into its crate |
| 12 test-only sites | `catalog.rs:368-370`, `context/guidance.rs:875`, `route/mod.rs:2322`, `verify/stamps.rs:535`, `doctor/mod.rs:3568`, `adapter/claude.rs:356-379`, `hook/decide.rs:485`, `policy/mod.rs:4180` | low each: the relative depth changes |
| `build.rs:44` embeds `../../claude-plugin` into `plugin_files.rs`, read by `install/plugin.rs:35` | the skill-text pin tests in `install/mod.rs` read it | medium: `build.rs` and the pin move with `install` |
| `conformance` feature | `Cargo.toml:14`, gate at `shared_state/mod.rs:32-34`; nothing here enables it, the closed build does | low to medium: becomes a crate dependency name |
| Policy hash pins | `policy/mod.rs` tests ~4100-4200, live-file pin at ~4180 | small: move with `policy`, repoint the root path |
| `pub(crate)` surface: 634 production lines (runner 122, doctor 29, hook 16, coordinator 9, admission 8; `cli` 402 belongs to the binary) | eight `admission` items used by `coordinator` (`SharedSeat`, `SpendBooked`, `track_shared_spend`, `take_spend_journal`, `shared_seats`, `has_dispatch`, `unclaim`, `revoke_grant`) | low: become `pub` |
| Test seam `pub mod test_support` | `lib.rs:57-62` | low: lives where the `tests/` suites link |
| Structural: `ledger` needs `ids` (18) and `lifecycle` (8) below it; `verify/heavy.rs` imports `admission`; `report/auto_settled.rs` imports `verify` and `workspace`; `shared_state` imports `money`, `programme`, `ids` | | each one a moved or extracted module; these are the real blockers |

`runner` stays in the outermost crate.

### What this tells t02

Lowest layer: `ids`, `lifecycle`, `policy`, `money`. Then `ledger`. The
coordinator client and wire types, and `report` (no inbound edges), are the
cheapest first cuts. Run the second build-time sample (and one on a quiet
host) before fixing the target.
