---
status: done
branch: prog/agents-many-nodes/an-t31-shared-state-cli-messages
repos: [relais, homelab]
adrs: []
plan-id: p-01M8AZYVC47W99ZXGFRC35H87H
---
# Tidy what the shared-state commands print

## Review panel

👉 **Decide:** none — approve if one stream rule (refusals always on stderr, `--json` adds the stdout object) fixes each listed line.
📍 relais, homelab · 5 phases · next: Phase 1. Panel: backend, lang:rust, tui, unix (opus); codex: not run (programme ticket agent).
**Changed by review:** `learning_cycle.rs` added to Phase 3; salvage tail made honest across nodes; binary `Refusal` test and `launch.rs` line builder.
**Verdicts:** round 2: lang:rust, unix approve; backend, tui approve-with-changes; 4 carried into implementation; words 1218 → 1470 (×1.21); remove 0, merge 0.
📄 Full reviews: [2026-11-26-tidy-shared-state-cli-messages.reviews.md](2026-11-26-tidy-shared-state-cli-messages.reviews.md)

## Picture

```
 relais <cmd> [--json]
   |-- did it --------> stdout: sentence | one JSON object (result)
   |-- refused -------> stdout: --json ? {"error","exit"} : nothing
   |                    stderr: one line "relais <cmd>: <why>"  (always;
   |                    an error from main reads "relais: <error>")
   |-- warning/fail --> stderr: one line
   '-- exit code ------ agrees with both
```

Every refusal reaches the person on stderr; `--json` adds the object on stdout, never moves the line.

## Goal

The an-t20, an-t24 and an-t26 reviews left small gaps in what a person
reads from the shared-state commands: a `--json` refusal of `relais decide`
reaches stderr on one path and not on another, the held-grant line points a
second node at a status that cannot show a salvage made elsewhere, `trust
grant` names `machine.toml` when the grant went to the primary, and the
counter's drop lines say "check the shared primary" with no place or command
behind it. Settle one stream rule and fix each line against it (an-t31).

## Non-goals

- No trait change: `DecisionGrant` keeps its two arms (settled in an-t19);
  telling "answered" from "being answered" is a follow-up for relais-fabric.
- No new command that reads the primary's counter (an-t24 kept "no
  primary-status command yet" deliberate); the lines say so instead.
- No change to `node.rs`'s held-elsewhere reason: it is a match key read by
  `is_decision_held_elsewhere`, never printed (`answer_error` writes the line).
- `trust show` and the plugin's consent card (`claude-plugin/hooks/consent.ts`,
  "Saved once … in <path>") keep naming `machine.toml`: a follow-up, since
  they read the path from `trust show --json`.
- Released CHANGELOG text stays as written.

## Code review

- relais: backend, lang:rust, unix, tui
- homelab: none — markdown only

## Behaviour

**The stream rule**, stated once as the doc comment of a new
`cli/decision_record.rs` helper, `refusal_streams(command, format, outcome,
message)` (it takes `programme::Format`, not a bool; `outcome.rs` would cycle
with `programme`, which owns `Format`), that `decide` and `feedback` both call; `print_refusal` writes it with
`writeln!` and treats a closed pipe as success, as `cli/programme.rs` does:

> stdout carries what the command did: its result sentence, or under
> `--json` exactly one object (the result, or `{"error","exit"}` for a
> refusal). stderr carries every refusal, warning and failure as one line
> for the person, under `--json` too. The exit code agrees with both.

This is the fleet's `cli.basics.stdout-is-data-stderr-is-talk` and
`cli.output.json-flag` ("errors under `--json` still go to stderr"),
applied; the comment cites them.

So, per line the ticket lists:

1. **`relais decide --json` refusal (held grant, any `Refusal`)**: today
   stdout only; now the `{"error","exit"}` object on stdout AND one stderr
   line `relais decide: <message>`; the absent-layer path keeps main's one
   line `relais: <error>` (one line each; the prefix differs by path, and the
   doc comment says so). `relais feedback --json` refusals get the
   same (identical shape today). The plugin reads stdout only and reads
   stderr only when stdout is not JSON (`claude-plugin/hooks/decide.ts`), so
   it is unaffected. A recorded `decide --json` approval that skipped its
   receipt now also prints `receipt_skipped_line()` on stderr (plain mode
   only today); the object already carries `receipt_skipped`.
2. **The held-grant line** (`answer_error`) stops claiming more than the
   node knows. New text:
   `another answer holds run R's decision (it may already be recorded):
   check `relais status R`; after the hold (up to N min), <next>`,
   N computed from `DECISION_GRANT_TTL_SECS` as today,
   where `<next>` is "answer again if it is still open" or, for salvage,
   "run `relais decide R --answer salvaged` again (status cannot show
   another node's salvage)"
   (`relais status` reads this node's ledger only, so the old tail "unless
   status shows it already salvaged" pointed at what cannot show it; a
   repeat after the hold is recorded on this node, which the line now says
   rather than hides). The sign-off
   note is unchanged. Exit 3 is unchanged.
3. **The installer's "… is on; turn it off with …" lines** move to stderr:
   they are "done" talk, which `cli.basics.stdout-is-data-stderr-is-talk`
   (decisions ADR-0021) sends to stderr, beside their failure lines. The
   plan render and "applied N change(s)" stay on stdout: they are the
   report the command was asked for (unchanged, out of this ticket).
4. **`trust grant` under `shared_state = "external"`**: the text line reads
   `trust grant: repository <key> recorded on the shared primary` (or `was
   already on the shared primary`), and the ledger warning says "the grant
   is saved, but this node's ledger did not log it", under `local` too (the
   old "in machine.toml" was untrue under `external`; one text reads right
   in both). The `local` result sentence is unchanged. `--json` keeps `machine_settings` (the plugin reads it)
   and adds `"recorded_on": "machine_settings" | "shared_primary"`. The
   selection is read with `shared_state::selection_at(&machine_path)` (no
   second `ctx::shared_state()` build, which an-t29's census counts). The
   lines are built by one pure function of (selection, scope, key, path,
   written), unit-tested for both selections, since no build links an
   external client. The text line stays on stdout: it is the grant's
   result sentence (the rule's first clause), unlike item 3's notices of a
   side effect nobody asked for.
5. **The counter's lines that say "check the shared primary"**
   (`coordinator/seats.rs`: the cap drop, the seat wait; `runner/launch.rs`:
   the unmanaged booking failure) end instead with what is true: the
   amount the counter is short by where the line knows it, and "ask the
   primary's operator to check it (no relais command reads the shared
   primary yet)". Departure from the round-2 plan, which had the line name
   `docs/shared-state.md, Where to look`: an installed binary has no `docs/`
   path, and the coordinator log would print it into the log the section
   tells the person to search, so the line states the fact and the docs
   (item 6) carry Where to look. The `launch.rs` line moves into a pure
   builder so a unit test reads its tail. The refused
   settle line keeps "if it repeats" and gets the same tail. These edits
   are written on the post-an-t28 text (relais PR 610 adds
   `(dropped_micros=<n>)` to the drop lines): this phase starts only after
   PR 610 merges and this branch is rebased. `runner/launch.rs`'s "run
   `relais coordinator status`" hint is kept: the dispatch stays held there,
   counted as active and then as a stale lease.
6. **Where to look**: `docs/shared-state.md` and this ticket's CHANGELOG
   paragraph name the coordinator log, `<state dir>/coordinator.log`
   (rotated once to `coordinator.log.1`), and the line to search for,
   `seat settles: dropped`.

## Phases

- [x] Phase 1 — stream rule and refusals: `crates/relais/src/cli/decision_record.rs`,
  `crates/relais/src/cli/decide.rs`, `crates/relais/src/cli/feedback.rs`,
  `crates/relais/tests/shared_state_cli.rs`.
- [x] Phase 2 — held-grant line: `crates/relais/src/cli/decision_record.rs`,
  `crates/relais/src/cli/decide.rs` (tests), `docs/shared-state.md`.
- [x] Phase 3 — trust grant under external, installer lines: `crates/relais/src/cli/trust.rs`,
  `crates/relais/src/cli/install.rs`, `crates/relais/tests/it/plugin_install.rs`,
  `crates/relais/tests/it/learning_cycle.rs` (its checks at 605 and 618 read
  stdout; every check, the negative ones too, moves to stderr).
- [x] Phase 4 — after relais PR 610 merges, rebase, then the counter lines
  (if 610 is still open when Phases 1–3 pass, they land with their own
  CHANGELOG lines and Phase 4 follows as its own commit after 610 merges):
  `crates/relais/src/coordinator/seats.rs`,
  `crates/relais/src/coordinator/seats/tests.rs`,
  `crates/relais/src/runner/launch.rs`, `docs/shared-state.md`,
  `CHANGELOG.md`.
- [x] Phase 5 — close an-t31 in homelab:
  `docs/programmes/agents-many-nodes/an-t31-shared-state-cli-messages.md`
  (Resolution, `status = "closed"`) and one Decisions line in
  `docs/programmes/agents-many-nodes/map.md`.

## Verification

| Check | Input | Expected | Actual |
| --- | --- | --- | --- |
| json refusal, both streams | `refusal_streams` (pure) unit test, `json = true` and `false` | json: object on stdout and one stderr line; plain: stderr only | pass: `cli::decision_record::a_refusal_is_one_stderr_line_and_under_json_also_one_stdout_object` (takes `Format`) |
| refusal wiring, binary | `tests/shared_state_cli.rs`: `decide --json` and `feedback --json` invocations that return a `Refusal` | stdout one `{"error","exit"}`, stderr one `relais <cmd>:` line, exit agrees; fails on today's `refusal_lines` | pass: `a_refusal_under_json_is_one_stdout_object_and_one_stderr_line`, decide and feedback, plain and `--json` |
| absent layer, binary | `tests/shared_state_cli.rs`, `relais decide --json` and `relais feedback --json` under `external` with no client | stdout one `{"error","exit":3}`, stderr one line starting `relais: `, exit 3 | pass: `a_salvage_under_an_absent_external_layer_is_refused_and_records_nothing` pins one `relais: ` line both ways |
| held line | `cli::decide` held tests, answer and salvage | the new text, exit 3, nothing written | pass: the four `cli::decide` held tests and `only_the_held_elsewhere_refusal_…`; salvage line 288 chars with a 21-char run id (was ~335) |
| trust lines | unit test of the line builder, `Local` and `External`, written and not | `recorded in <path>` vs `recorded on the shared primary`; JSON `recorded_on` | pass: `cli::trust::a_grant_names_where_it_was_recorded`, both selections, written and kept |
| installer lines | `tests/it/plugin_install.rs` and `tests/it/learning_cycle.rs` install `--write` in a temp HOME | "session routing is on" and "automatic promotion is on" on stderr, not stdout | pass: `plugin_install::an_install_records_the_routing_envelope_once_…`, `learning_cycle::installing_records_…` and `…off_tombstone…`, stderr present, stdout absent |
| trust, binary | `relais trust grant --reviewed-by x --json` in a temp HOME | `recorded_on: machine_settings`, exit 0 | pass: `a_json_trust_grant_says_where_it_was_recorded`: `recorded_on: machine_settings`, exit 0 |
| counter lines | `coordinator::seats` tests on the drop and refused lines | the new tail, `dropped_micros` kept | pass: `coordinator::seats` tests (cap drop, count cap, refused settle, seat wait) on the new tail, `dropped_micros` kept, no `check the shared primary` |
| unbooked line | unit test of the `runner/launch.rs` line builder | the new tail | pass: `runner::tests::shared::an_unbooked_cost_…`, cost once, new tail |
| pilot | the branch binary: `relais decide <unknown run> --answer approve --json` | stdout object, stderr line, exit equals the object's `exit` | binary 06:33: `decide run-nope --answer approve --json` exit 10; stdout `{"error":"unknown run run-nope","exit":10}`; stderr `relais decide: unknown run run-nope`; with stdout closed (`head -c0`) no panic |
| suite | `cargo test -p relais`, `cargo clippy -p relais --all-targets -- -D warnings` | 0 failed, clean | `cargo test -p relais` on `1bde131b1`: lib 2750, bin 80, it 354, shared_state_cli 10, 26, 16, 1; 0 failed. clippy `--all-targets -D warnings` clean |

## Implementation review

- implementation-review: round 1 approve-with-changes (blocking: `json: bool` flag args, fixed with `programme::Format`; doc split, fixed); Delta approve-with-changes (comment paths fixed; Where-to-look departure recorded in Behaviour 5; plan Actuals filled); ~79k+74k, 59+29 s.
- backend: round 1 approve-with-changes (doc split, selection read once, amount asserted: all fixed); Delta approve; 74k+71k.
- lang:rust: round 1 approve-with-changes (doc split, comment, layout: fixed); Delta approve; `deliberate:` take `&Refusal` and a typed scope (lows, file-local style); 65k+63k.
- unix: round 1 approve-with-changes (only BrokenPipe dropped: fixed); Delta approve; 79k+52k.
- tui: round 1 approve-with-changes (6 wording items fixed); Delta approve-with-changes, its two lows fixed in `1bde131b1` (held line 288 chars, "ask the primary's operator to check it"); 63k+64k.

## Follow-ups

- A grant answer that says the holder recorded (`DecisionGrant` arm), so a
  second node can say "already answered": relais-fabric with the trait.
- A command that reads the primary's day counter and waiting settles.
- `trust show` and the plugin consent card under `external`.

<!-- panel: repos=relais,homelab adds= reviewers=backend,lang:rust,tui,unix body-sha=cc5dd9591750 -->
