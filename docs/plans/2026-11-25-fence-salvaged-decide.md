---
status: done
branch: prog/agents-many-nodes/an-t26-fence-salvaged-decide
repos: [relais, homelab]
adrs: []
plan-id: p-01M8AAHN48JRJZJRHGD2VTVMV0
---
# Fence salvaged decide through the grant

## Review panel

👉 **Decide:** none — approve if `salvaged` takes the decision grant like every other answer and nothing else changes.
📍 relais, homelab · 2 phases · next: Phase 1. Panel: backend, lang:rust, tui, unix (opus); codex: not run (programme ticket agent).
**Changed by review:** salvage's containment warning prints only after the write; salvage gets its own next-step tail; eight verification rows added.
**Verdicts:** round 2: tui, unix approve; backend, lang:rust approve-with-changes; 6 carried into implementation (stderr checks in `tests/shared_state_cli.rs`, `answer_error` tail parameter, past-tense warning, closed predicate test); words 642 -> 844 (x1.31); remove 0, merge 0.
📄 Full reviews: [2026-11-25-fence-salvaged-decide.reviews.md](2026-11-25-fence-salvaged-decide.reviews.md)

## Picture

```
 decide <run> --answer salvaged --candidate <sha>
   decide_in (shared state built lazily)
     salvage_command
       argument + state refusals           (no grant asked)
       salvage_containment (read only)     (no grant held)
       node::answer_under_grant(state, run,
          write  = ledger.record_salvage_contained,
          closed = Ok(Some(_)))
          HeldElsewhere      -> exit 3, same line as the other answers
          Granted, Ok(None)  -> decision already on record: grant given back
          Granted, Ok(Some)  -> grant kept (TTL)
          Granted, Err       -> grant given back
```

Salvage asks the shared half for the decision grant first, like every other answer.

## Goal

an-t26 of agents-many-nodes. an-t21 fenced every `relais decide` answer that
closes a decision except `salvaged`, which closes through
`Ledger::record_salvage_contained` with no grant. It can close an OPEN decision
row (a run still waiting on a person), so under a shared half it races a
relais settle or an answer on another node exactly as approve did. After this
change `salvaged` takes the decision grant before that write; a grant held
elsewhere is the same exit 3 refusal. `LocalState` always grants, so one node
is unchanged.

## Non-goals

- No change to the `SharedState` trait, grant types, `LocalState`, `FakeState`
  or the conformance suite; no new node function (`node::answer_under_grant`
  from an-t21 already fits).
- `relais feedback` stays unfenced (an-t21: it answers no decision).
- A salvage of a run with no decision row (it ended on its own) still asks the
  grant: the grant is per run, the check is cheap, and a settle of that run
  elsewhere is the race being fenced. Under `shared_state = "external"` with
  no client it now exits 3 like the other answers (an-t06, refuse rather than
  split).

## Code review

- relais: backend, lang:rust, unix, tui
- homelab: none — markdown only

## Behaviour

- `salvage_command` takes the shared-state builder `decide_in` already has
  (lazy, so an argument refusal or an ineligible run builds none, as in
  an-t21). Order: the argument and state refusals, then `salvage_containment`
  (read only, may be slow, so no grant is held over it), then the grant, then
  `record_salvage_contained`. `salvage_containment` returns the unreachable
  reason instead of printing it; the "recording it as unknown" stderr line is
  printed only after the write returns `Ok(Some(_))`, so a refused salvage
  never claims it is recording.
- The write runs through `node::answer_under_grant`; `closed` is
  `Ok(Some(_))`. `Ok(None)` ("already carries a decision on record") and `Err`
  give the grant back.
- A held grant is the an-t21 refusal: `answer_error` moves from `cli/decide.rs`
  to `cli/decision_record.rs` (a `salvage.rs` import of `decide.rs` would be a
  cycle `scripts/check-child-cycles.py` refuses), takes a `NextStep` enum for
  its tail (an-t21's callers pass `AnswerAgain`), and is reused, so the line,
  exit 3 and `--json` shape
  are identical (no sign-off note: salvage takes no criterion), except the
  next-step tail for salvage reads "then run the same `--answer salvaged`
  again unless status shows it already salvaged", since a terminal run with
  no decision row has nothing "still open".
- The in-place worktree freeing after the answer is untouched.
- `docs/shared-state.md` row and the CHANGELOG Unreleased section gain the
  line; the an-t21 CHANGELOG sentences stay (history).

## Phases

- [x] Phase 1 — route salvage: `crates/relais/src/cli/salvage.rs`,
  `crates/relais/src/cli/decide.rs` (call site, `answer_error` visibility,
  tests with the `Elsewhere` state; `try_answer_through` gains a candidate
  argument, since it hard-codes `None` today and every salvage row but the
  missing-candidate one needs one), `docs/shared-state.md`, `CHANGELOG.md`.
- [x] Phase 2 — close an-t26 in homelab:
  `docs/programmes/agents-many-nodes/an-t26-fence-salvaged-decide.md`
  (Resolution, `status = "closed"`) and one Decisions-so-far line in
  `docs/programmes/agents-many-nodes/map.md`.

## Verification

| Check | Input | Expected | Actual |
| --- | --- | --- | --- |
| held | `cargo test -p relais --bin relais cli::decide`: `a_salvage_held_elsewhere_is_refused_with_exit_3_and_writes_nothing`, a waiting run under the held state | exit 3 (`Blocked`), the an-t21 refusal text with the salvage next step, decision still open, 1 ask, 0 releases | pass |
| held, ended run | `a_salvage_of_an_ended_run_held_elsewhere_is_refused_and_writes_nothing`, a `Failed` run with no decision row | exit 3, no decision resolved, 1 ask, 0 releases | pass |
| granted | `a_granted_salvage_closes_the_open_decision_and_keeps_the_grant` | recorded (exit 0), open decision resolved, 1 ask, 0 releases; a repeat is refused before any grant | pass |
| not recorded | `a_salvage_of_an_already_answered_decision_gives_its_grant_back`: a rejected run salvaged under a granting state | `InvalidInput`, 1 ask, 1 release | pass |
| closed predicate | `only_a_recorded_salvage_keeps_the_grant`: `Ok(Some)` keeps, `Ok(None)` and `Err` give back | keep, give back, give back | pass |
| no grant on refusal | `a_salvage_refused_for_its_arguments_asks_no_grant_and_builds_no_state`: no `--candidate` | `InvalidInput`, 0 asks, the shared builder never called | pass |
| external, no client (lib) | `a_salvage_under_a_failing_shared_state_is_refused_before_any_write` | the builder's error comes through, the decision stays open | pass |
| external, no client (binary) | `tests/shared_state_cli.rs::a_salvage_under_an_absent_external_layer_is_refused_and_records_nothing`, plain and `--json` | exit 3; plain: one stderr line naming the layer, empty stdout; `--json`: the layer's line on stderr and one `{"error","exit":3}` object on stdout; no decision row | pass |
| held, `--json` | the held refusal's JSON shape is the shared `refusal_lines` path (an-t21); no test runs a held grant under `--json` | covered by the absent-layer `--json` row above, not by a held run | not run for a held grant (deliberate: needs an external client no build has) |
| LocalState | the existing `cli::decide` and salvage tests, unchanged | pass | pass |
| suite | `cargo test -p relais` on `135827ba6`: lib 2720, bin 79, 353, 26, 16, 7, 1; after the merge of origin/main, the full run's lib 2741, 0 failed | 0 failed | 0 failed (`--workspace` not run; the other crates are untouched) |
| lint | `cargo clippy -p relais --all-targets -- -D warnings` on `135827ba6` and after the merge | clean | clean (the `--features conformance` run was not made) |
| pilot | a seeded waiting run through the branch binary | exit 0, recorded | not run (the granting bin test covers the path) |

## Decision log

- 2026-11-25 — decided-by: agent (ADR-0080): route salvaged through the grant
  rather than state why it needs none, because it closes an open decision row.


## Implementation review

- implementation-review: approve after two Deltas; fixed: the sign-off `write!` now `push_str(&format!)`, the held test rows, the shared-state builder called before containment (round 1 and delta 1 approve-with-changes, delta 2 and the post-merge Delta approve) (opus).
- backend: approve after the Delta; fixed: the not-recorded and ended-held tests (opus).
- lang:rust: approve after the Delta; fixed: `Containment` returned whole, `NextStep` enum, `use` imports (opus).
- unix: approve, no findings in round 1 (opus).
- tui: approve after the Delta; deliberate: salvage tail names `decision_salvaged` by words, not the status field (low), CHANGELOG restated in words (opus).

<!-- panel: repos=relais,homelab reviewers=backend,lang:rust,tui,unix body-sha=9feac01cda25 -->
