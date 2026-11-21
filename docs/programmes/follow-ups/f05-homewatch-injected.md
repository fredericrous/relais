+++
type = "task"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais"]
+++
# Inject the home watch into the engine

## Question

Should the engine get its home and environment as a `HomeWatch` value built once at the CLI root, so `FingerprintSettings.home_override` can go?

## Look at

`crates/relais/src/homeprint.rs`, `runner/engine.rs` (`run_inner` entry, `run_to_outcome`), `runner/home_drift.rs`, `policy/mod.rs` `FingerprintSettings`; the architect's carried finding on #512 in `docs/plans/2026-11-20-home-fingerprint-warns.md` (Implementation review).

## Answer looks like

A refactor PR: `HomeWatch { home, env }` built in the CLI and passed down; `home_override` removed; the runner fixture tests unchanged in what they assert.

## Resolution

2026-11-21: **yes.** `RunConfig` carries `home: &HomeWatch` (the home
directory and its variable lookup), built once by `relais run` and the
dataset replay with `HomeWatch::from_process()`; a package's child config
passes its root's. `FingerprintSettings.home_override` is gone. The
runner fixtures pass `HomeWatch::fixed(dir/home)`, which answers `HOME`
alone, as the override did; no test assertion changed. The no-home
warning is still printed only for a root run with the fingerprint
enabled.
