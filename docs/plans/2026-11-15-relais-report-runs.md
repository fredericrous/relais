---
canonical: amont-agent:docs/plans/2026-11-15-measure-each-plans-time-to-done.md
phases: [1]
status: active
branch: feat/report-runs
repos: [relais]
adrs: [ADR-0070, ADR-0064]
---
# relais report runs (pointer)

This repository's slice of the time-to-done measure (Phase 1): `relais
report runs --json [--since <date>]` publishes each root run's start, end
and hours as `{"schema": 1, "since", "runs": […]}`, so greenpiste's
`plan-review stats --time` and retro-weekly can join relais hours onto a
plan by `root_session` without reading the ledger file
(`harness.cross-tool-reads`). Relais keeps no branch, so the join is the
root session and the plan's session window.

## Review panel
👉 **Decide:** none — approve if a read-only subcommand with a `schema` token is the right published query.
📍 relais · Phase 1 built · next: implementation review, release. Panel: the full plan's panel (backend, lang:rust, tui, unix, architect, po, all approve on its final body).

<!-- panel: repos=amont-agent,relais,dotfiles,decisions adds=cli,contract reviewers=backend,lang:rust,tui,unix,architect,po body-sha=19053529d9c2 -->

## Code review
- relais: backend, lang:rust, unix, tui

## Behaviour
- `relais report runs --json [--since <date>]` is a subcommand of `report` with its own arguments. `--json` is required (exit 2 without it), and `report`'s `--by`, `--no-import` and `--since` before `runs` are refused.
- One object per root run created at or after `--since` (default: the first of the month, UTC), oldest first: `id`, `repo`, `root_session`, `status` (the projected state), `started_at` (the earliest attempt start, else the run's creation), `ended_at` and `hours`.
- `ended_at` is `null` while the projected state is not terminal, whatever its finished attempts say; for a terminal run it is the latest attempt end, else the run's `updated_at`. This is stricter than the full plan's wording ("the latest attempt end, else `updated_at` for a terminal run, else null"), which would end a running run at its first finished attempt.
- `hours` runs to now for a run still going, never negative, to three decimals.
- It opens the ledger read-only (`Ledger::open_read_only`: no migration, no write, no usage import), and a machine with no ledger gets `runs: []` and no file.

## Decision log
- 2026-11-15 — `ended_at` is `null` for any non-terminal run (see Behaviour), because a run with one finished attempt and a second in progress is still going.
- 2026-11-15 — Only root runs (`parent_run IS NULL`) are listed, as `report`'s own run list does, so a package run's hours are not counted twice.

## Verification
- `cargo test -p relais --test it report_runs`: the window, an empty window, no ledger, the default `since`, `--json` required, `report`'s flags refused, and a ledger byte-identical after two queries with `usage_events` unchanged.
- Live: `relais report runs --json --since 2026-11-08 | jq '.schema, (.runs|length)'` → `1` and a count above 0, run from the installed release.
