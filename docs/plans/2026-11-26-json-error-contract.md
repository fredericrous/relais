---
status: done
branch: prog/agents-many-nodes/an-t32-json-error-contract
repos: [relais, homelab]
adrs: []
plan-id: p-01M8BFJJWA4VNEFMGX7AVK15FS
---
# One `--json` error contract

## Review panel

👉 **Decide:** none — approve if one `{"error","exit"}` object on stdout for every `--json` failure is the contract.
📍 relais, homelab · 1 phase · next: Phase 1. Panel: backend, lang:rust, tui, unix (opus); codex: not run (programme ticket agent).
**Changed by review:** learn cycle and trust failure returns enumerated; stale-review fields join the one object; exceptions named.
**Verdicts:** round 1: 4 approve-with-changes; round 2 backend approve-with-changes; 1 carried; words 437 → 621 (×1.42); remove 0, merge 0.
📄 Full reviews: [2026-11-26-json-error-contract.reviews.md](2026-11-26-json-error-contract.reviews.md)

## Picture

```
 relais <cmd> --json
   did it ---------> stdout: one result object
   refused --------> stdout: {"error","exit"}
                     stderr: relais <cmd>: why
   other error ----> stdout: {"error","exit"}   (NEW for 4 commands)
                     stderr: relais: why
   no --json ------> stdout empty on every failure
```

Every failure under `--json` is one object on stdout plus the stderr line.

## Goal

After an-t31, `decide --json` and `feedback --json` print the `{"error","exit"}`
object on stdout for an error that is not a refusal; `report --json` and
`learn cycle --json` print nothing. Decide one contract and apply it (an-t32).

## Decision

**The object always.** Under `--json`, every failure of a shared-state
command, refusal or not, prints exactly one `{"error","exit"}` object on
stdout, with the message of the stderr line, and the stderr line stays. Why
not "stdout empty": an-t31 already ships the object on stdout for refusals and
the plugin reads stdout first (`claude-plugin/hooks/decide.ts`), so empty would
change shipped behaviour and give a consumer two shapes depending on which
kind of failure; one shape with the object is parseable on every exit code.
Without `--json`, stdout stays empty on failure. ADR-0021 `cli.output.json-flag`
("errors under `--json` still go to stderr") is kept: stderr still carries the
line.

## Non-goals

- Commands outside the six (`status`, `run`, `report runs`, `dataset seed`...)
  keep their shapes; a follow-up if wanted.
- A result object that itself reports an error (`learn cycle`'s report with
  `action = error`) stays the one object it is.
- No exit-code change.
- `learn cycle --json`'s plain text lines on exit 0 (`deferred: not idle`,
  `not due`) stay text: a follow-up. A clap usage error (exit 2) prints nothing
  on stdout. A write that fails partway through a result document may leave a
  partial document before the error object; accepted. `print_document`'s
  BrokenPipe handling is a separate pre-existing gap.

## Code review

- relais: backend, lang:rust, unix, tui
- homelab: none — markdown only (the ticket file and the map's Decisions line)

## Behaviour

- `cli/decision_record.rs`: `print_error_object(format, outcome, message)` and
  `error_object_under_json(format, command_result)` beside `print_refusal`; the contract is
  stated in `refusal_streams`' doc and the object is built by it (one builder).
- `decide`, `feedback` use `print_error_object` in their `Err` arm (their inline
  `serde_json::json!` copies go); `report`, `learn cycle`, `doctor`, `trust`
  are wrapped at their `dispatch` arms with `error_object_under_json`.
- `learn cycle`'s early failure returns print the object under `--json`:
  coordinator unreachable, lease request error, lease `Busy` (exit 20), lease
  `Refused` and the repo-policy error (`learn.rs`). Under `--log` the stderr line
  goes to the log file as before and the object still goes to stdout.
- The `trust` subcommands' `eprintln!` + `Ok(failure)` returns (`--mark-reported`
  unreported, empty `--reviewed-by`, grant write failure, holdouts discovery,
  unread store, shadowed, review failure) go through `print_refusal`; the two
  stale-review refusals keep their `code`, `current_key` and `detail` fields on
  the one object (`print_refusal_with`), where they were a separate document.
- `docs/shared-state.md` states the contract once; CHANGELOG `## Unreleased`.

## Phases

- [x] Phase 1: helpers, six commands, tests, docs, CHANGELOG: `crates/relais/src/cli/decision_record.rs`, `crates/relais/src/cli/decide.rs`, `crates/relais/src/cli/feedback.rs`, `crates/relais/src/cli/learn.rs`, `crates/relais/src/cli/mod.rs`, `crates/relais/tests/shared_state_cli.rs`, `crates/relais/src/cli/trust.rs`, `docs/shared-state.md`, `CHANGELOG.md`.

## Verification

- `cargo test -p relais --test shared_state_cli` (the report and learn-cycle
  tests gain `--json` forms and a `starts_with("relais: ")` check; the salvage test
  pins `decide`'s error arm under `--json`; `feedback` shares the helper).
- `cargo test -p relais` and `make check`.
- Input -> expected: a failing `trust` grant (empty `--reviewed-by`) with
  `--json` -> one object, exit 2, a `relais trust grant: ` stderr line; each of
  the six failing without `--json` -> empty stdout. The `learn cycle` lease and
  coordinator returns need a live coordinator: checked by review, not by test.
- Piloted: `RELAIS_STATE_DIR` scratch with `shared_state = "external"`,
  `relais report --json` prints one object, exit 3.

## Implementation review

- implementation-review (opus): approve-with-changes; 3 blocking (flag argument, test helper flag, placeholder name) and 2 medium fixed; tests tightened.
- backend: approve-with-changes; decide/feedback test tightened; stale-holdout and doctor tests `deliberate: carried`, doctor needs a failing project dir, holdouts a store.
- lang:rust: approve-with-changes; unused import, pure `error_object`, extras guard, `Format` parameter, test helper: fixed.
- unix: approve-with-changes; `--log` sentence fixed in docs; second stderr line on a failed stdout write `deliberate: documented`.
- Delta (rebased on origin/main, docs only): implementation-review, lang:rust, backend, unix, tui all resolved. Kept as `deliberate`: lang:rust's release-build guard on extras, `use` placement in mod.rs and a long doc line (lows, no third pass); a unit test for the stale-holdout fields.
- tui: approve; the follow-up for `deferred` / `not due` text is in the ticket.

## Observed

- `cargo test -p relais`: lib 2759, bin 83, `shared_state_cli` 14 (the salvage test pins `decide --json`'s error arm; a generic decide/feedback test was dropped, both need a real run to reach the arm), all passed; clippy `--all-targets -D warnings` clean. Pilot on the branch binary, `shared_state = "external"` scratch: `report --json` -> stdout one `{"error":...,"exit":3}` line, stderr one `relais: ` line, exit 3.

## Your steps

none

<!-- panel: repos=relais,homelab reviewers=backend,lang:rust,tui,unix body-sha=314dc7e6a6d3 -->
