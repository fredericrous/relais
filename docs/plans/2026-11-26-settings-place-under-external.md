---
status: active
branch: prog/agents-many-nodes/an-t33-settings-place-under-external
repos: [relais]
adrs: []
plan-id: p-01M8BFAREFPVYRBCH1ASHND13G
---
# Name where settings live under an external client

## Review panel

👉 **Decide:** none — approve if the place comes from the built state in all three outputs.
📍 relais · 1 phase · next: implement. Panel: backend, lang:rust, tui, unix, architect, po (opus); codex: not run.
**Changed by review:** one phase; `RecordedOn` serde as the only wire spelling; SPEC.md, `selected in:` line and typescript profile added.
**Verdicts:** round 2 backend approve; round 1 six approve-with-changes, all applied; words 341 → 527 (×1.55, each addition a finding fix); remove 0, merge 1.
📄 Full reviews: [2026-11-26-settings-place-under-external.reviews.md](2026-11-26-settings-place-under-external.reviews.md)

## Picture

```
 SharedState (built once)  --selection()-->  RecordedOn
        |                                      |-- trust grant : line + --json recorded_on
        |                                      |-- trust show  : text line + --json recorded_on
        |                                      '-- consent card (plugin reads show --json)
 machine.toml second read (selection_at) removed from trust grant
```

## Goal

an-t31 made `trust grant` name the shared primary under `external`. `trust show` and the plugin's consent card still name `machine.toml`, and `trust grant` reads `machine.toml` a second time (`selection_at`) after building its state. Take the place from the built state's `selection()` (already on the trait) and use it in all three places (an-t33).

## Non-goals

- No trait change: `SharedState::selection()` already exposes the selection.
- `selection_at` stays (it builds the state in `current_with`).
- No change to other commands' wording.
- Public relais links no external client, so the new line is visible only in the private build (an-t13); unit tests prove it here.

## Code review

- relais: backend, lang:rust, lang:typescript, unix, tui (architect left out: the trait is untouched; the only contract change is the additive `recorded_on` of `trust show --json`, read optionally by the plugin)

## Behaviour

`relais trust show` prints `settings: the shared primary (shared_state = "external")` and `selected in: <machine.toml path>` instead of the `machine.toml:` line under `external`; `--json` of `trust show` gains `recorded_on` (`machine_settings` | `shared_primary`), the same values as `trust grant --json`; `machine_settings` stays in both documents and, under `external`, names this node's settings file, not where the grant lives. The consent card says `Saved once for the repository on the shared primary.` when `recorded_on` is `shared_primary`; an older relais without the field keeps the file wording, and an older plugin ignores the field. Local output is unchanged apart from the new JSON field.

## Phases

### Phase 1: the place from the built state

Touches `crates/relais/src/trust.rs` (`RecordedOn` with serde `snake_case` as the only spelling of the wire value, `Shown.recorded_on`, a `show()` parameter), `crates/relais/src/cli/trust.rs` (`place = RecordedOn::of(shared.selection())` once after the state is built; drop `selection_at` and `GrantPlace`; `trust grant --json` serializes `place`; `trust_show_text` branches on it; the module doc stops saying grants live only in machine.toml), `claude-plugin/hooks/consent.ts` (the card line), `docs/SPEC.md` (the `trust show` paragraph and `recorded_on`). Tests and changelog in the same phase: `crates/relais/src/cli/trust.rs` (unit test of `trust_show_text` and the JSON for both selections), `crates/relais/tests/shared_state_cli.rs` (`trust show --json` locally), `claude-plugin/tests/consent.test.ts` (the card) and `CHANGELOG.md` (Unreleased).

## Verification

- Local run, `relais trust show --json | jq -r .recorded_on`: expected `machine_settings`; the text keeps the `machine.toml:` line (binary test in `shared_state_cli.rs`).
- `show_names_where_settings_live`: under `Selection::External` the text has `settings: the shared primary` and `selected in: <path>`, no `machine.toml:`, and `recorded_on` is `shared_primary`; under `Local` it is `machine_settings`. A binary run under `external` exits 3 before showing and `ctx::shared_state()` has no test seam, so the `shared.selection()` wiring itself has no external test; the first real external run (private build, an-t13) is the check, carried to relais-fabric.
- `consent.test.ts`: the card says `Saved once for the repository on the shared primary.` for `recorded_on: 'shared_primary'`, keeps the `in <path>` line without it; every line at most 80 columns.
- `grep -n selection_at crates/relais/src/cli/trust.rs`: exit 1.

Your steps: none.

<!-- panel: repos=relais adds= reviewers=backend,lang:rust,tui,unix,architect,po body-sha=328812f43f51 -->
