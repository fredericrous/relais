# Full reviews: The SharedState trait and LocalState

Panel on opus, 2026-11-23. Round 1 on body 1f72d7ac680c, round 2 on
a60beaea05a2, final backend and lang:rust on c9cc3dc6c48f.

## Round 1

- **architect** approve-with-changes (26k, 19 s). 1 high selection/config
  cycle: accepted. 2 high pre-main registry: accepted in part (OnceLock, the
  private main's route is an-t13 fog). 3 high concrete `&Ledger`/`&Registry`
  in a public trait: rejected (an-t06 makes it public API; docs mark it
  unstable). 4 medium registry check-then-act: accepted (flock). 5 medium
  speculative surface: rejected (the ticket names those ops). 6 low
  deny_unknown_fields downgrade: accepted.
- **tui** approve-with-changes (24k, 14 s). 1 high refusal text and exit:
  accepted. 2 high doctor lock-out: accepted. 3 medium `--json`: rejected
  (existing CliError path). 4 medium 80 columns: rejected in part (path is
  unbounded; ASCII, no colour kept). 5 low wider byte diff: accepted.
- **lang:rust** approve-with-changes (35k, 32 s). 1 high non-atomic CAS:
  accepted. 2 high no pre-main in Rust: accepted. 3 high `cli/mod.rs`
  missing: accepted. 4 medium test-edit check blind to inline tests:
  accepted. 5 medium epoch table per process: accepted (stated).
- **po** approve-with-changes (34k, 29 s). 1 high `main.rs` vs dispatch:
  accepted. 2 high cut publish/mode/schema: rejected (ticket). 3 high user
  outcome: accepted. 4 medium conformance first: accepted. 5 medium
  baseline: accepted. 6 medium post-release measures: rejected (no release
  in this ticket). 7 low unix/tui seats: rejected (reason in Code review).
- **backend** approve-with-changes (47k, 68 s). 1 high registration path:
  accepted. 2 high "every caller follows" false: accepted (direct readers
  narrowed). 3 high CAS race: accepted. 4 medium per-instance epoch table:
  accepted. 5 medium doctor/hook refusal: accepted. 6 medium downgrade and
  writer keeps key: accepted. 7 low test-edit check: accepted.
- **unix** approve-with-changes (37k, 41 s). 1 high exit code via
  `CliError`: accepted. 2 high hook must exit 0, doctor reports: accepted.
  3 medium refuse only where machine.toml was read: accepted. 4 low inside
  dispatch for `--protocol`: accepted.

## Round 2

- **architect** approve (32k, 9 s). Carried: an-t13 must prove
  `register_external` is reachable before dispatch; a compile-time
  "unstable" marker; name the split config paths in `docs/shared-state.md`.
- **lang:rust** approve-with-changes (36k, 9 s); final body (51k, 20 s).
  Carried: the raw selector parse yields a typed enum and an unknown value
  is left to the strict parse; a round-trip test that an absent key stays
  absent; `cli/doctor.rs` holds the doctor finding; the flock test opens
  two separate file descriptions.
- **po** approve-with-changes (36k, 12 s). Carried: rollback trigger (any
  byte diff or failing existing test); a read-only registry directory
  check; state config routing is partial in the docs; the an-t13
  dependency in the Resolution.
- **tui** approve-with-changes (30k, 11 s). Carried: a `--json` routed
  command under external; doc which commands refuse; confirm exit 3 is
  `Blocked` (checked: `cli/outcome.rs:120`).
- **unix** approve-with-changes (46k, 17 s). Applied: the registry lock is
  `procs::LockFile` (flock, never deleted).
- **backend** approve-with-changes (round 2), then approve on the final body
  (58k, 7 s). Applied: refusal probe moved to Phase 3; `release_claim` on
  the trait; non-root mode-000 check. Its `trust list` finding withdrawn:
  `cli/trust.rs:21` calls `load_machine`.

Codex: not run (a programme ticket agent; no person to relay to).
Recording: `greenbelay plan-review record` exited 1 (no transcript found for
this agent session); the review went on.
