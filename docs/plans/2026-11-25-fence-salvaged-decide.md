---
status: active
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
- A held grant is the an-t21 refusal: `answer_error` in `cli/decide.rs`
  becomes `pub(super)` and is reused, so the line, exit 3 and `--json` shape
  are identical (no sign-off note: salvage takes no criterion), except the
  next-step tail for salvage reads "then run the same `--answer salvaged`
  again unless status shows it already salvaged", since a terminal run with
  no decision row has nothing "still open".
- The in-place worktree freeing after the answer is untouched.
- `docs/shared-state.md` row and the CHANGELOG Unreleased section gain the
  line; the an-t21 CHANGELOG sentences stay (history).

## Phases

- [ ] Phase 1 — route salvage: `crates/relais/src/cli/salvage.rs`,
  `crates/relais/src/cli/decide.rs` (call site, `answer_error` visibility,
  tests with the `Elsewhere` state; `try_answer_through` gains a candidate
  argument, since it hard-codes `None` today and every salvage row but the
  missing-candidate one needs one), `docs/shared-state.md`, `CHANGELOG.md`.
- [ ] Phase 2 — close an-t26 in homelab:
  `docs/programmes/agents-many-nodes/an-t26-fence-salvaged-decide.md`
  (Resolution, `status = "closed"`) and one Decisions-so-far line in
  `docs/programmes/agents-many-nodes/map.md`.

## Verification

| Check | Input | Expected | Actual |
| --- | --- | --- | --- |
| held | `cargo test -p relais --bin relais cli::decide`: salvage on a waiting run and on a terminal run through the held state | exit 3, stated line, decision still open / no row written, 1 ask; stderr carries only the refusal line (no "recording it as unknown") | |
| granted | same through a granting state | recorded; open decision's `resolved_at` set; 1 ask, 0 releases | |
| not recorded | salvage on a run already carrying a decision on record, granting state | InvalidInput refusal, 1 release | |
| write fails | salvage through a granting state whose ledger write errors (`node::answer_under_grant` unit test with `closed = Ok(Some(_))`) | the error is returned, 1 release | |
| no grant on refusal | salvage without `--candidate` through the held state | InvalidInput, 0 asks, shared builder never called | |
| external, no client | salvage on a terminal run with a `shared` builder that returns the an-t06 refusal | exit 3, no decision row written | |
| held, `--json` | held salvage with `--json` | one stdout object `{"error","exit":3}`, empty stderr | |
| LocalState | existing salvage and in-place tests | pass unchanged | |
| suite | `cargo test --workspace`; `cargo clippy --workspace --all-targets -- -D warnings` | 0 failed tests, 0 clippy warnings | |

## Decision log

- 2026-11-25 — decided-by: agent (ADR-0080): route salvaged through the grant
  rather than state why it needs none, because it closes an open decision row.

<!-- panel: repos=relais,homelab reviewers=backend,lang:rust,tui,unix body-sha=9feac01cda25 -->
