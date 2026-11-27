---
status: done
plan-id: p-01M8DWJAT8B86CRSSZKNEJ9HFH
branch: prog/agents-many-nodes/an-t37-json-contract-gaps
repos: [relais, homelab]
adrs: []
---
# Close the gaps in the json contract

## Review panel

👉 **Decide:** none — approve if one `outcome:"deferred"` object with a `reason` word is the whole contract for a cycle that did not run.
📍 relais, homelab · 3 phases · next: Phase 1. Panel: backend, lang:rust, tui, unix, po, architect (opus); codex: not run (programme ticket agent).
**Changed by review:** exit codes fixed (20, 1, 1, 1); tests moved onto the existing in-process `Daemon`; fake only for refused, skew, `end_training` count.
**Verdicts:** round 2: po, architect approve; backend, rust, tui, unix approve-with-changes; 9 carried into implementation; words 789 → 961 (×1.22); remove 0, merge 0.
📄 Full reviews: [2026-11-27-an-t37-json-contract-gaps.reviews.md](2026-11-27-an-t37-json-contract-gaps.reviews.md)

## Picture

```
 relais learn cycle --json
   |-- ran -----------> report object {"action"}        exit 0 or 1
   |-- not idle ------> {"outcome":"deferred",
   |                     "reason":"not_idle","agents"}  exit 0
   |-- not due -------> {"outcome":"deferred",
   |                     "reason":"not_due","detail",
   |                     "next_due"}                    exit 0
   '-- coordinator, lease or policy failure
                       {"error","exit"} + one stderr line
```

Every exit 0 prints one object; the key `outcome` marks a cycle that did not run.

## Goal

an-t32 gave the shared-state commands one `--json` error object, with three
gaps left: `learn cycle --json` still prints plain `deferred: not idle` and
`not due` lines on exit 0, the coordinator, lease and policy failure arms
of `learn cycle` have no test, and nothing tests `doctor --json`'s error
object or `trust`'s stale-holdout `code`/`detail` fields. Decide the object
`learn cycle --json` prints for a cycle that did not run, pin it in
`docs/shared-state.md`, and test the arms (an-t37).

## Non-goals

- No change to the exit code of a deferred or not-due cycle (0), to the
  plain-text lines, or to the `--log` stamping of the plain lines.
- No change to the success report object (`CycleReport`, pretty-printed).
- No new seam in `coordinator::Client`: the tests put a fake coordinator on
  the socket the binary connects to.
- No test of a real stale holdout with a git store; see Decision log.

## Code review

- relais: backend, lang:rust, unix
- homelab: none — markdown only

## Behaviour

Under `learn cycle --json`, a cycle that did not run prints exactly one
compact JSON object and exits 0:

```
{"outcome":"deferred","reason":"not_idle","agents":3}
{"outcome":"deferred","reason":"not_due","detail":"<sentence>","next_due":"<words>"}
```

`outcome` is `deferred` for both, `reason` is the machine word, `detail` and
`next_due` are `Due::No`'s `reason` and `next_due` strings, the two the plain
`not due: <detail>; <next_due>` line joins. The success report has no
`outcome` key (it has `action`), so a consumer branches on `outcome`. Not due
wins over not idle under `--if-due`, as today. Without `--json` the plain
lines are as before (`deferred:` stamped under `--log`, `not due` not).
The object is written through `decision_record::write_object` (quiet on a
closed pipe), never `println!`. With `--log <file>` it goes to the file as
one bare line and stdout and stderr stay empty; this is a named exception
to "stdout is never empty" in `docs/shared-state.md`, which also loses the
"still prints its plain lines (a follow-up)" exception.

Exit codes the tests pin: busy 20 (`TrainingLeaseHeld`), refused 1, policy
failure 1, coordinator failure 1, `doctor` removed cwd 3, `trust` stale
holdout 3.

Tests, plain and `--json`, each asserting exit, stderr and stdout (the
object, or empty). Not idle, not due, busy and the policy failure use the
real in-process `Daemon` of `tests/it/learning_cycle.rs`; the arms it cannot
reach use a fake coordinator in `tests/shared_state_cli.rs` (a thread on
`<RELAIS_STATE_DIR>/relais.sock`, bound before the binary runs, its scratch
from `short_temp_dir` for the 104-byte socket cap, every reply encoded from
`relais::coordinator::Response`, never literal JSON):

1. busy with and without a holder: exit 20, object `exit` 20.
2. refused (`depth_exceeded`), fake: exit 1, message names code and detail.
3. not idle: the object; plain, the line; `--log`: bare object in the file.
4. policy failure (a scratch repository whose `relais.toml` does not
   parse, lease granted): exit 1, one stderr line, object, and one
   `end_training` reached the coordinator.
5. not due: after the policy failure's error row, `--if-due` prints the
   not-due object, with the lease granted and with another session's agent
   active (the `NotIdle` arm's due check, same object).
6. the fake answering `ping` with a well-formed `Pong` at `PROTOCOL_VERSION
   + 1` (`VersionSkew`): exit 1; any other ping failure would spawn a real
   detached daemon, so none is used.
7. `doctor --json` from a removed working directory (spawned through
   `sh -c 'cd "$1" && rmdir "$1" && exec "$0" doctor --json'`, since
   `current_dir` on a missing path fails the spawn): exit 3, one object,
   its message equal to the stderr line; plain, stdout empty.
8. `trust holdouts --accept … --json` with no `[holdouts]` store: exit 3,
   the object carries `code = "stale_holdout_review"` and `detail`, and
   `error` equals `refused: ` plus `detail`.

## Phases

- [x] Phase 1 — the object: `crates/relais/src/cli/learn.rs`, reading
  `Due::No`'s existing fields for both the line and the object
- [x] Phase 2 — tests: `crates/relais/tests/it/learning_cycle.rs`,
  `crates/relais/tests/shared_state_cli.rs` (a `Scratch` on `short_temp_dir`
  and a variant taking the working directory)
- [x] Phase 3 — docs and changelog: `docs/shared-state.md`, `docs/SPEC.md`
  §17, `README.md` where it quotes the lines,
  `crates/relais/src/cli/outcome.rs` module doc, `CHANGELOG.md`; close
  homelab `docs/programmes/agents-many-nodes/an-t37-json-contract-gaps.md`

## Decision log

- 2026-11-27 — One `outcome` word (`deferred`) with two `reason`s, as the
  ticket's example, so a consumer tests one key.
- 2026-11-27 — The stale-holdout fields are reached with no `[holdouts]
  store`: `holdout::accept` refuses with its text and the arm that prints
  `code` and `detail` is the same. A real stale review needs a git store
  and a moved commit, and adds no branch of this code.
- 2026-11-27 — Review changed the exit codes (20, 1, 1, 1), moved most
  tests onto the existing real `Daemon`, and rejected a `next_due_at`
  timestamp: `Due` holds words only. Follow-up: a machine-readable
  next-due instant.

## Verification

Observed 2026-11-27 on the branch build, through the real binary with scratch
`RELAIS_STATE_DIR`s.

| Check | Input | Expected | Observed |
|---|---|---|---|
| not idle | `learn cycle --json`, another session's agent active (real `Daemon`) | one object, exit 0 | `{"outcome":"deferred","reason":"not_idle","agents":1}`, exit 0, stderr empty, no row |
| not due | `--if-due --json` after a cycle ran | `reason=not_due`, `next_due` set, exit 0 | object with `detail` "the last cycle started …" and `next_due` "next due 20…"; same with an agent active (not due wins) |
| log | `--json --log f`, not idle and not due | `f` gains one JSON line; stdout and stderr empty | one bare line each, stdout empty |
| busy | held lease, `--json`; fake `Busy{holder:None}`, plain and `--json` | exit 20, object `exit` 20, one stderr line | pass; "its holder is not recorded" |
| refused, skew | fake `Refused(depth_exceeded)`; fake `Pong` at `PROTOCOL_VERSION + 1` | exit 1, object equals the stderr text, no lease given back | pass; skew message names both protocol numbers |
| policy | malformed `relais.toml`, plain and `--json` | exit 1, object `exit=1`, one `end_training` | pass on the real `Daemon` (object) and the fake (count 1) |
| doctor, trust | `doctor --json` from a removed cwd; `trust holdouts --accept … --json` with no store | exit 3, one object; trust's has `code=stale_holdout_review` | pass |
| suite | `cargo test -p relais`, `cargo clippy --all-targets -D warnings` | 0 failed, clean | see the PR checks |

## Implementation review

- implementation-review: approve-with-changes ×3, all code findings resolved; typed `Format`/`Deferred`, fake's swallowed errors, `Format::Text` arms and one `not_due_line` source fixed (81k+46k, 66 s+207 s).
- lang:rust: approve-with-changes ×2; bool flag, untyped literals, doc comment fixed; `Format::Text` arms fixed after the Delta (63k+29k).
- backend: approve-with-changes, then approve; skew assertion, fake timeouts fixed; deliberate: exit-1-on-write-failure has no test (needs an injectable writer) (73k+30k).
- unix: approve-with-changes, then approve; failed stdout write now exits 1; deliberate: plain-line `println!` predates this change (64k+29k).

<!-- panel: repos=relais,homelab adds=contract reviewers=backend,lang:rust,tui,unix,po,architect body-sha=fd2a042ae1ed -->
