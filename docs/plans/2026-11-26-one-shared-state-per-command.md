---
status: active
branch: prog/agents-many-nodes/an-t29-one-shared-state-per-command
repos: [relais, homelab]
adrs: []
plan-id: p-01M8AV8H8FZVBSJX311Y6TXTJX
---
# One SharedState per relais command

## Review panel

👉 **Decide:** none — approve if building the state once in each of report, doctor, trust grant, install and learn cycle closes an-t29.
📍 relais, homelab · 2 phases · next: Phase 1. Panel: backend, lang:rust, tui, unix (opus); codex: not run (programme ticket agent).
**Changed by review:** a unit test guards the count (helpers too); tests moved to `shared_state_cli.rs` with learn cycle, install `--hooks`; explicit install build the census can see.
**Verdicts:** final: 4 approve; 1 carried (the unit test also checks `read_dir` of `src/cli`); words 1030 → 1503 (×1.46, mostly fixes to wrong or unverifiable statements); remove 0, merge 0.
📄 Full reviews: [2026-11-26-one-shared-state-per-command.reviews.md](2026-11-26-one-shared-state-per-command.reviews.md)

## Picture

```
 command fn          builds today             after
 ------------------  -----------------------  ------------------------
 report_command      load_price_table x2      one, before the import
 doctor_command      load_machine in two      shared_state().ok()
                     helpers                  once, to both helpers
 trust_command       load_machine             shared_state()? once:
   (grant)           + write_settings         machine_of, write_of
 install_command     two envelope writes      one Result, if needed
   (--write/hooks)   + hook admission         for the three steps
 learn_cycle_command load_machine             shared_state()? once:
                     + shared_state           machine_of, NodePointers
 every other command                          builds once: unchanged
```

Each command function that reached the shared-state factory more than once builds the state once and hands that one instance to every step.

## Goal

an-t29 of agents-many-nodes. an-t22 (`relais run`) and an-t25 (`relais
dataset replay`) gave two commands one `SharedState`; the others still reach
`ctx::shared_state()` once per helper. A closed client that keeps state per
instance (a lease, a grant, a session to the primary) would then see two
holders for one command. After this change no command function builds more
than one: every builder call goes through a state the command built once.
`LocalState` holds no state, so nothing changes for one node.

## Census (main at `017c714b3`)

A builder is `ctx::shared_state()` or a `ctx` wrapper that calls it:
`load_machine`, `load_price_table`, `load_machine_pricing`,
`machine_for_router`, `write_settings`, `tuning_bounds`,
`hook_admission_settings`, `shared_state_for_hook`. Every other construction
is test-only: each `LocalState::new()` outside `shared_state/` sits below a
`#[cfg(test)] mod tests` line (run.rs:728, programme/claim.rs:323,
learn/cycle.rs:1193, coordinator/mod.rs:3159, decide.rs:902,
hook/respond.rs:853), `current_with` is called only from `cli/ctx.rs`, and no
other crate calls it. No CLI command calls another command function in
process (`run_command` and `replay_command` are called only from
`cli/mod.rs`).

Per command, counted through its helpers:

| command | builds | where |
| --- | --- | --- |
| `relais report` | 2 | report.rs:74 and :79, `load_price_table` |
| `relais doctor` | 2 | doctor.rs:23 `note_held_repair_effort`, :81 `holdouts_finding` (`load_machine` each) |
| `relais trust grant` | 2 | trust.rs:21 `load_machine`, then :88 or :119 `write_settings` |
| `relais install --write [--hooks]` | 2 or 3 | install.rs:150 `write_settings_for_install` (two envelopes), :262 `admission_queue_wait` → `hook_admission_settings` |
| `relais learn cycle` | 2 | learn.rs:530 `load_machine`, :652 `shared_state` |

Everything else builds once and is not touched:

- `recipe diff`, `evaluate` and `promote` call `tuning_bounds` once each
  (recipe.rs:300, :457, :606; :805 is a test); `recipe list`, `show` and
  `rollback` build none.
- `relais decide` takes a factory (`decide_in`'s `shared`); each `shared()?`
  (decide.rs:513, :618, :869, salvage.rs:83) sits on its own path that
  returns, so one decide calls it once.
- `run`, `dataset replay` (an-t22, an-t25), `plan`, `programme claim`,
  `adopt` and `next`, `coordinator daemon`, the `native` router commands and
  `native usage`, `learn` promote, rollback, envelope, off, dataset build
  and train, `usage import`, and `hook respond` (`shared_state_for_hook`, the
  admission settings read through the same state) build once.

## Phases

- [ ] Phase 1 (relais): `crates/relais/src/cli/{ctx,report,doctor,trust,install,learn}.rs`, `crates/relais/tests/shared_state_cli.rs`, `crates/relais/tests/it/plugin_install.rs`, `CHANGELOG.md`, `docs/shared-state.md`.
  - `ctx.rs`: add `write_settings_of(state: &dyn SharedState, edit)` with the same `Home → CliError::Home` mapping and the operator principal; `write_settings` becomes its wrapper. Delete `hook_admission_settings()` (its only caller, the installer, now passes its state to `hook_admission_settings_from`), and repoint the docs that name it (ctx.rs:351, install.rs:259).
  - `report_command`: one `load_price_table()?` after `open_ledger`, before `if !no_import`, so an absent external layer still exits 3 before the import writes anything; both uses take it.
  - `doctor_command`: `let shared = shared_state().ok();` before `note_held_repair_effort`; both helpers take `Option<&dyn SharedState>` and read `machine_of` through it. A state that does not build still says nothing on these two findings, as today.
  - `trust_command`: `let shared = shared_state()?;`, `machine_of(&*shared)?`, and both grant writes through `write_settings_of(&*shared, …)`. The first error (exit 3 under an absent external layer) is unchanged.
  - `install_command`: one `Option<Result<Arc<dyn SharedState>, CliError>>`, built up front only when a step needs it: `(Apply && plugin.failure.is_none()) || targets == OwnedFilesAndHooks`, via an explicit `if need { Some(shared_state()) } else { None }` (not `bool::then(shared_state)`: a function pointer has no `shared_state(` text, so the census and the unit test would not count it). So a preview without `--hooks` builds nothing, as today. The two envelope writes take it; an `Err` still prints each "not recorded" line once, from the one stored error. `install_hooks_target` takes it for `admission_queue_wait`, which on `Err` falls back to `Arc::new(LocalState::new())`, as `shared_state_for_hook` does (`hook_admission_settings_from` takes `&Arc<dyn SharedState>`).
  - Integration test `an_install_with_hooks_under_an_absent_external_layer_still_succeeds` in `crates/relais/tests/it/plugin_install.rs`, beside the existing install test: under `shared_state = "external"` with no client, `relais install --claude --write --hooks` exits 0, `session routing not recorded` and `automatic promotion not recorded` each appear once on stderr, the hooks step's applied report appears on stdout and the PreToolUse handler is written with the default admission wait, and `machine.toml` is unchanged. This pins the hooks step falling back to `LocalState` rather than failing the install; it passes on main too (a regression guard).
  - Unit test `every_command_builds_its_shared_state_once` in `cli/ctx.rs`: it reads the cli sources with `include_str!`, applies the census rules (stop at the first `#[cfg(test)]`, top-level `fn`s, the builder names, `.write_settings(` outside ctx.rs skipped) and asserts that, outside ctx.rs, only a command entry (`*_command`, or `hook_respond`) holds a builder call, and at most one, so a builder added to a helper fails it. It fails on main (`report_command` 2, `trust_command` 3, `learn_cycle_command` 2, and the doctor and install helpers) and passes on the branch: the guard that stays after this ticket.
  - `learn_cycle_command`: `let shared = shared_state()?;` where `load_machine` is now, `machine_of(&*shared)?`, and `NodePointers(shared)` at :652.
  - Integration tests in `crates/relais/tests/shared_state_cli.rs` (its `Scratch::with_machine_toml` harness already pins exit 3 and one sentence):
    - `a_report_under_an_absent_external_layer_is_refused_before_the_import`: under `shared_state = "external"` with no client, `relais report` exits 3, the "sets shared_state = \"external\"" sentence appears once on stderr, stdout is empty, and a transcript placed under a scratch `CLAUDE_CONFIG_DIR/projects` (the test sets `CLAUDE_CONFIG_DIR` explicitly, since paths.rs:173 prefers it over `HOME` and the harness does not clear it) is not imported (the ledger holds no imported run afterwards). This pins that the refusal precedes the import writes; it passes on main too (`load_price_table()?` is already before the import there) and guards the reorder.
    - `a_learn_cycle_under_an_absent_external_layer_is_refused_before_the_coordinator`: `relais learn cycle` exits 3 with the one sentence, and `<scratch>/state/relais.sock` does not exist afterwards (`socket_path()` is `state_dir()/relais.sock`; `Scratch` sets `RELAIS_STATE_DIR` to `<scratch>/state`) (the build stays before `ensure_running`, learn.rs:532).
    - Both tests pin the failure and its single report, not the build count: `LocalState` holds no state and the count is pinned by the unit test above.
  - `CHANGELOG.md`: one sentence. `docs/shared-state.md`: the "builds its state once" bullet names every command.
- [ ] Phase 2 (homelab): `docs/programmes/agents-many-nodes/an-t29-one-shared-state-per-command.md`, `map.md`. Resolution, status closed, one Decisions-so-far line.

## Verification

- Census: the per-function script below, run on `017c714b3` and on the branch, must show exactly this diff and no other line change: `report.rs:report_command 2→1`, `trust.rs:trust_command 3→1`, `learn.rs:learn_cycle_command 2→1`, `doctor.rs:note_held_repair_effort 1→gone`, `doctor.rs:holdouts_finding 1→gone`, `doctor.rs:doctor_command gone→1`, `install.rs:write_settings_for_install 1→gone`, `install.rs:admission_queue_wait 1→gone`, `install.rs:install_command gone→1`, `ctx.rs:hook_admission_settings 1→gone`, `ctx.rs:write_settings_of gone→1` (its `.write_settings(` call, counted by design in ctx.rs). The branch prints at most 1 for every command function (the unit test above asserts it in CI).

  ```bash
  for f in crates/relais/src/cli/*.rs; do awk -v F="$(basename "$f")" '
    /^#\[cfg\(test\)\]/ { exit }
    /^(pub\(crate\) )?fn [a-z_0-9]+/ { match($0, /fn [a-z_0-9]+/); fn = substr($0, RSTART+3, RLENGTH-3) }
    /^(pub\(crate\) )?fn / { next }
    /(^|[^_])(shared_state|load_machine|load_price_table|load_machine_pricing|machine_for_router|write_settings|tuning_bounds|hook_admission_settings|shared_state_for_hook)\(/ {
      if ($0 ~ /^ *\/\//) next; if ($0 ~ /\.write_settings\(/ && F != "ctx.rs") next; n[fn]++ }
    END { for (k in n) printf "%s:%s %d\n", F, k, n[k] }' "$f"; done | sort
  ```
- The integration tests in Phase 1 pass, and so does `an_install_under_an_absent_external_layer_says_not_recorded_and_writes_nothing` (one "not recorded" line per write from the one error).
- `cargo clippy -p relais --all-targets -- -D warnings` exit 0; `cargo test -p relais` exit 0.

## Non-goals

- Direct `machine.toml` reads that do not go through the factory (`doctor::doctor`'s own findings, the coordinator daemon's settings read): they build no `SharedState`; routing them is not this ticket.
- `decide`'s factory parameter stays a factory: each path calls it once.
- No trait, `node.rs`, runner or coordinator change; no output change.

## Code review

- relais: backend, lang:rust (tui: no pane or terminal output changes; unix: no process, signal or file-mode change)
- homelab: none — markdown only

## Decision log

- 2026-11-26 — install builds its state only when a step needs it, not at the top: a preview without `--hooks` built none before, and an external client's build may reach the primary. decided-by: agent (ADR-0080).
- 2026-11-26 — round 1 (tui): moving the report's price-table load ahead of the claude-projects dir lookup means a broken home and an external layer together show the external refusal first. Rejected: both messages are actionable, exit 3 is unchanged, and it is noted for the code review.
- 2026-11-26 — round 2 (unix, low): "the report test must fail on main" rejected: on main `load_price_table()?` (report.rs:74) already precedes `import_for_report`, so the test passes there by design; it guards the reorder, and the plan now says so.
- 2026-11-26 — the round-1 ledger record (`greenbelay plan-review record`) did not bind the four reviews to this plan (no completed plan-review found in the session transcript); the review text stays in the Full reviews section, and the record is retried at the close.

<!-- panel: repos=relais,homelab reviewers=backend,lang:rust,tui,unix body-sha=108aaa073d90 -->
