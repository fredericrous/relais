+++
type = "grilling"
mode = "hitl"
status = "closed"
blocked_by = ["t01-coupling-map"]
repos = ["relais"]
+++
# Which crates relais splits into, and in what order

## Question

Which member crates does the workspace get, in what order are they cut, and
which of them, if any, become separate tools later? Does the split land before
or after the `epic-vocabulary` rename implementation? Both rewrite
`crates/relais/src/programme/` and `cli/programme.rs`, so one lands before the
other starts.

## Look at

- t01's Resolution: the coupling graph, the ties and the baseline.
- t03's Resolution once it closes (whether `admission`/`coordinator` stay).
- `docs/programmes/epic-vocabulary/map.md` and its t01.
- `harness.tool-boundaries` (ADR-0064): what a separate tool may reach.

## Answer looks like

An ordered list of crates, each with the modules it takes, the ties to cut
first, and the order against the rename implementation. For each crate one
line: stays inside the relais binary, or becomes its own tool later. The two
targets for the map's Destination, stated as numbers from t01's baseline.

## Resolution

Closed 2026-11-27 with the person. Measured on `origin/main` at `a18a1c6f`.

**Method.** Edges are `crate::<module>` references in production code: the
lines before each file's first `#[cfg(test)]`, outside `tests/` directories,
with comment lines dropped, braced `use crate::{…}` lists counting each item.
Modules are the 53 top-level modules under `crates/relais/src/` (`lib`,
`main` and `test_support` excluded). Strongly connected components by
Tarjan's algorithm. Sizes are non-blank code lines on the same cut (93,797
in all). The script lived in the session's scratch space; the method above
reproduces it.

**The production module graph has no cycle.** With comments kept, 16
modules (39.8k lines: `ledger`, `admission`, `verify`, `policy`, `router`,
`route`, `workspace`, `holdout`, …) form one component; every edge that
closes it is a rustdoc link (`/// [`crate::ledger::Ledger`]`, `policy ->
backend`, `policy -> verify`, `lifecycle -> ledger`, …). Dropped, the graph is
a DAG. t01's group table showed groups pointing both ways because its groups
cut across these layers: crates follow the module order, not the groups. The
ties to cut first are those rustdoc links (each becomes a plain path or moves
to the higher crate) and t01's tie table, unchanged.

### Crates, bottom-up, in cut order

Working labels; t04 names each one once.

| # | Crate | Modules | ~Lines | Depends on | Later |
|---|---|---|---|---|---|
| 0 | none | repoint the rustdoc links; move `workspace::git_command` into `core` | – | – | – |
| 1 | `core` | ids, acceptance, contract, money, procs, fsutil, disk, storage_full, fsclone, paths, homeprint, orchestration, protocol, lifecycle, rng, units, task_labels, native | 5.8k | none | stays in the binary |
| 2 | `policy` | policy, catalog, outcome, tooling, backend | 3.3k | core | stays |
| 3 | `admission` | admission, workspace, holdout | 7.6k | core, policy | stays |
| 4 | `ledger` | ledger | 8.6k | 1–3 | stays |
| 5 | `routing` | route, router, learn, trust, review_outcome | 14.1k | 1–4 | stays |
| 6 | `verify` | verify, repo, exhaustion | 5.4k | 1–5 | stays |
| 7 | `report` | report | 2.6k | 1–6 | stays |
| 8 | `programme` | programme | 2.9k | core only (once `git_command` moved) | stays |
| – | `relais` (binary) | runner, cli, hook, install, doctor, adapter, context, shared_state, coordinator, ipc, reap, resume, prune, seed, quiet, native_pricing | ~43k | all | – |

- `programme` depends only on `procs`, `fsutil`, `paths` and
  `workspace::git_command`, so it can be cut any time after `core`.
- `coordinator` stays in the binary: it needs `ledger` and `shared_state`,
  and nothing but `install` depends on it (t03 kept it relais's own).
- `runner` stays in the binary, as t01 said.

### Order against the epic-vocabulary rename

**The rename lands first** (the person, 2026-11-27). epic-vocabulary's code
change rewrites `programme/`, `cli/programme.rs` and `hook/resume.rs`; the
`programme` crate (row 8) is cut after it, named for the new word once.
Rows 0–7 do not touch `programme/` and may run beside the rename.

### Names and tools

- **The split stories wait for t04's names** (the person), so no crate is
  renamed twice.
- **No crate becomes its own tool** (the person). `harness.tool-boundaries`
  (decisions ADR-0075; the ticket's ADR-0064 is an older head) already puts
  programme-frontier reading inside greenlift and host slots in greengate's
  slots crate; a separate tool would supersede it.

### Targets for the map's Destination

Set by the person: **a one-line edit in `verify/` or `runner/` rebuilds in
10 s or less** (t01: 23–29 s), and **no production file over 700 non-blank
lines** (t01's measure, before `#[cfg(test)]`; today `admission/mod.rs`,
3,865).

- The rebuild number is from t01's busy-host sample. On 2026-11-27 all three
  host slots were held at load 14.5, so it was not resampled. The first split
  story re-measures the baseline on a quiet host (`greengate slots` showing no
  holder) before the target is judged.
- The file target is the larger job: 56 of 228 production files are over 700
  lines, 30,969 lines above it in all. The largest are `admission/mod.rs`
  3,865, `coordinator/mod.rs` 3,037, `policy/mod.rs` 2,787, `doctor/mod.rs`
  2,525, `verify/mod.rs` 2,023, `cli/programme.rs` 1,812 and `cli/mod.rs`
  1,787. A crate move shrinks no file: these are file-split stories of their
  own, after the crate they sit in is cut, and t01's job sections for the
  four largest are where each split starts.
